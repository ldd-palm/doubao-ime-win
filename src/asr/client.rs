//! ASR WebSocket Client
//!
//! Handles the WebSocket connection to the Doubao ASR server.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use super::constants::*;
use super::device::DeviceCredentials;
use super::proto::FrameState;
use super::protocol::{
    build_finish_session, build_start_session, build_start_task, build_task_request,
    parse_response, AsrResponse, ResponseType, SessionConfig,
};

/// Error returned when the ASR session could not be established because the
/// server rejected our credentials.
///
/// The server does not answer with a clean auth error for an expired token. It
/// either stalls the handshake or fails the session with a backend discovery
/// error, so those symptoms are classified here and mapped to "credentials are
/// stale, re-register and retry".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupFailure {
    /// Handshake or session setup exceeded the timeout.
    Timeout,
    /// Server refused the session in a way that points at stale credentials.
    StaleCredentials,
    /// Server refused the session for some other reason.
    Rejected,
}

/// Error type for a failed realtime session setup
#[derive(Debug)]
pub struct AsrSetupError {
    pub kind: SetupFailure,
    pub detail: String,
}

impl AsrSetupError {
    /// Whether re-registering the device is a sensible response.
    ///
    /// A stalled handshake is included: an expired token manifests as the server
    /// dragging out session setup rather than returning a clean auth error.
    pub fn warrants_credential_refresh(&self) -> bool {
        matches!(
            self.kind,
            SetupFailure::Timeout | SetupFailure::StaleCredentials
        )
    }
}

impl std::fmt::Display for AsrSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            SetupFailure::Timeout => write!(f, "ASR session setup timed out: {}", self.detail),
            SetupFailure::StaleCredentials => {
                write!(f, "ASR session rejected (stale credentials): {}", self.detail)
            }
            SetupFailure::Rejected => write!(f, "ASR session rejected: {}", self.detail),
        }
    }
}

impl std::error::Error for AsrSetupError {}

/// Heuristic: does this server error indicate stale credentials?
///
/// Also used for errors that surface mid-session through the response channel,
/// which is how `service discovery failure` is actually reported.
pub fn is_credential_failure(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("service discovery failure")
        || m.contains("auth")
        || m.contains("token")
        || m.contains("unauthorized")
        || m.contains("permission")
}

/// ASR Client for real-time speech recognition
pub struct AsrClient {
    credentials: DeviceCredentials,
    connect_timeout_secs: u64,
}

impl AsrClient {
    /// Create a new ASR client with credentials and the default connect timeout
    pub fn new(credentials: DeviceCredentials) -> Self {
        Self {
            credentials,
            connect_timeout_secs: 8,
        }
    }

    /// Create a new ASR client with an explicit connect timeout
    pub fn with_timeout(credentials: DeviceCredentials, connect_timeout_secs: u64) -> Self {
        Self {
            credentials,
            // Guard against a zero timeout, which would fail instantly.
            connect_timeout_secs: connect_timeout_secs.max(1),
        }
    }

    /// Get WebSocket URL with parameters
    fn ws_url(&self) -> String {
        format!(
            "{}?aid={}&device_id={}",
            WEBSOCKET_URL, AID, self.credentials.device_id
        )
    }

    /// Start real-time ASR session
    ///
    /// Returns a receiver for ASR responses
    pub async fn start_realtime(
        &self,
        mut audio_rx: mpsc::Receiver<Vec<u8>>,
    ) -> Result<mpsc::Receiver<AsrResponse>> {
        let url = self.ws_url();
        let request_id = Uuid::new_v4().to_string();
        let token = self.credentials.token.clone();
        let device_id = self.credentials.device_id.clone();

        // Build request with headers
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("User-Agent", USER_AGENT)
            .header("proto-version", "v2")
            .header("x-custom-keepalive", "true")
            .header("Host", "frontier-audio-ime-ws.doubao.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .body(())?;

        // Bound the handshake. An expired token causes the server to stall here
        // for tens of seconds while audio piles up in the capture channel, which
        // previously showed up only as "Channel full, dropping frame" spam.
        let connect_timeout = Duration::from_secs(self.connect_timeout_secs);
        let setup_start = Instant::now();

        tracing::info!("Connecting to ASR WebSocket: {}", url);
        let (ws_stream, _) = match tokio::time::timeout(connect_timeout, connect_async(request)).await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Rejected,
                    detail: format!("websocket connect failed: {}", e),
                }
                .into())
            }
            Err(_) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Timeout,
                    detail: format!(
                        "no websocket handshake within {}s",
                        connect_timeout.as_secs()
                    ),
                }
                .into())
            }
        };
        tracing::info!(
            "WebSocket connected successfully ({}ms)",
            setup_start.elapsed().as_millis()
        );
        let (mut write, mut read) = ws_stream.split();

        // Create response channel
        let (result_tx, result_rx) = mpsc::channel::<AsrResponse>(100);

        // Clone values for tasks
        let request_id_clone = request_id.clone();
        let token_clone = token.clone();

        // Send StartTask
        tracing::debug!("Sending StartTask (request_id: {})", &request_id[..8]);
        let start_task_msg = build_start_task(&request_id, &token);
        write.send(Message::Binary(start_task_msg)).await?;

        // Wait for TaskStarted response
        match tokio::time::timeout(connect_timeout, read.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                let response = parse_response(&data);
                if response.response_type == ResponseType::Error {
                    return Err(classify_setup_error("StartTask", &response.error_msg));
                }
                tracing::info!(
                    "TaskStarted received ({}ms since setup start)",
                    setup_start.elapsed().as_millis()
                );
            }
            Ok(Some(Ok(_))) => tracing::debug!("Ignoring non-binary frame during StartTask"),
            Ok(Some(Err(e))) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Rejected,
                    detail: format!("StartTask transport error: {}", e),
                }
                .into())
            }
            Ok(None) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Rejected,
                    detail: "connection closed before TaskStarted".to_string(),
                }
                .into())
            }
            Err(_) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Timeout,
                    detail: format!("no TaskStarted within {}s", connect_timeout.as_secs()),
                }
                .into())
            }
        }

        // Send StartSession
        tracing::debug!("Sending StartSession");
        let session_config = SessionConfig::new(&device_id);
        let start_session_msg = build_start_session(&request_id, &token, &session_config);
        write.send(Message::Binary(start_session_msg)).await?;

        // Wait for SessionStarted response
        match tokio::time::timeout(connect_timeout, read.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                let response = parse_response(&data);
                if response.response_type == ResponseType::Error {
                    return Err(classify_setup_error("StartSession", &response.error_msg));
                }
                tracing::info!(
                    "SessionStarted received ({}ms since setup start, session ready)",
                    setup_start.elapsed().as_millis()
                );
            }
            Ok(Some(Ok(_))) => tracing::debug!("Ignoring non-binary frame during StartSession"),
            Ok(Some(Err(e))) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Rejected,
                    detail: format!("StartSession transport error: {}", e),
                }
                .into())
            }
            Ok(None) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Rejected,
                    detail: "connection closed before SessionStarted".to_string(),
                }
                .into())
            }
            Err(_) => {
                return Err(AsrSetupError {
                    kind: SetupFailure::Timeout,
                    detail: format!("no SessionStarted within {}s", connect_timeout.as_secs()),
                }
                .into())
            }
        }

        // Spawn audio sending task
        tracing::info!("Starting audio frame sender task");
        tokio::spawn(async move {
            let mut frame_index = 0u64;
            let start_time = current_time_ms();

            // Process audio frames until channel is closed
            while let Some(opus_frame) = audio_rx.recv().await {
                let frame_state = if frame_index == 0 {
                    FrameState::First
                } else {
                    FrameState::Middle
                };

                let timestamp_ms = start_time + frame_index * FRAME_DURATION_MS as u64;
                let msg = build_task_request(
                    &request_id_clone,
                    opus_frame,
                    frame_state,
                    timestamp_ms,
                );

                if write.send(Message::Binary(msg)).await.is_err() {
                    tracing::warn!("Failed to send audio frame {}", frame_index);
                    break;
                }

                frame_index += 1;
                
                // Log every 50 frames (about 1 second)
                if frame_index % 50 == 0 {
                    tracing::info!("Sent {} audio frames ({:.1}s)", frame_index, frame_index as f64 * 0.02);
                }
            }

            tracing::info!("Audio channel closed, sent {} total frames", frame_index);

            // Send last frame to signal end
            if frame_index > 0 {
                let timestamp_ms = start_time + frame_index * FRAME_DURATION_MS as u64;
                let silent_frame = vec![0u8; 100];
                let msg = build_task_request(
                    &request_id_clone,
                    silent_frame,
                    FrameState::Last,
                    timestamp_ms,
                );
                let _ = write.send(Message::Binary(msg)).await;

                // Send FinishSession
                let finish_msg = build_finish_session(&request_id_clone, &token_clone);
                let _ = write.send(Message::Binary(finish_msg)).await;
                tracing::info!("Sent FinishSession");
            }
        });

        // Spawn response receiving task
        let result_tx_clone = result_tx.clone();
        tokio::spawn(async move {
            // The server sends periodic heartbeats; if nothing at all arrives
            // for this long the connection is probably half-dead (TCP hasn't
            // noticed, but nothing is getting through either). Treat that the
            // same as an explicit drop instead of waiting forever.
            const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

            loop {
                let next = match tokio::time::timeout(IDLE_TIMEOUT, read.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        tracing::warn!(
                            "No ASR message received for {}s, treating connection as dead",
                            IDLE_TIMEOUT.as_secs()
                        );
                        let _ = result_tx_clone
                            .send(AsrResponse {
                                response_type: ResponseType::Error,
                                error_msg: format!(
                                    "no message received within {}s (connection likely dead)",
                                    IDLE_TIMEOUT.as_secs()
                                ),
                                ..Default::default()
                            })
                            .await;
                        break;
                    }
                };

                // Every path below that ends the loop for a reason other than
                // an explicit ResponseType::Error/SessionFinished from the
                // server must still signal *something* downstream: silently
                // dropping the channel previously left the caller's session
                // looking "stuck" with no idea it had died.
                match next {
                    Some(Ok(Message::Binary(data))) => {
                        let response = parse_response(&data);

                        match response.response_type {
                            ResponseType::Error | ResponseType::SessionFinished => {
                                let _ = result_tx_clone.send(response).await;
                                break;
                            }
                            ResponseType::Heartbeat => {
                                // Ignored, but logged at debug level so the
                                // real server heartbeat cadence can be
                                // observed to sanity-check IDLE_TIMEOUT above.
                                tracing::debug!(
                                    "Heartbeat received ({}ms since setup start)",
                                    setup_start.elapsed().as_millis()
                                );
                                continue;
                            }
                            _ => {
                                if result_tx_clone.send(response).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(_)) => {
                        // Non-binary frame (e.g. a ping/pong control frame);
                        // nothing to parse, keep waiting.
                        continue;
                    }
                    Some(Err(e)) => {
                        tracing::warn!("ASR response stream error: {}", e);
                        let _ = result_tx_clone
                            .send(AsrResponse {
                                response_type: ResponseType::Error,
                                error_msg: format!("connection lost: {}", e),
                                ..Default::default()
                            })
                            .await;
                        break;
                    }
                    None => {
                        tracing::warn!("ASR response stream closed by server");
                        let _ = result_tx_clone
                            .send(AsrResponse {
                                response_type: ResponseType::Error,
                                error_msg: "connection closed by server".to_string(),
                                ..Default::default()
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(result_rx)
    }
}

/// Map a server-side setup error to a [`SetupFailure`] classification
fn classify_setup_error(stage: &str, error_msg: &str) -> anyhow::Error {
    let kind = if is_credential_failure(error_msg) {
        SetupFailure::StaleCredentials
    } else {
        SetupFailure::Rejected
    };
    AsrSetupError {
        kind,
        detail: format!("{} failed: {}", stage, error_msg),
    }
    .into()
}

/// Get current timestamp in milliseconds
fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
