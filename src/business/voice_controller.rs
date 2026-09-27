//! Voice Controller
//!
//! Coordinates voice input between audio capture, ASR, and text insertion.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;

use crate::asr::{is_credential_failure, AsrClient, AsrResponse, AsrSetupError, ResponseType};
use crate::audio::AudioCapture;
use crate::business::{TextCorrector, TextInserter};
use crate::data::CredentialStore;

/// How many times to retry re-establishing the ASR session after it drops
/// mid-recording, before giving up and actually stopping. Backoff doubles
/// starting at 1s (1s, 2s, 4s).
const MAX_RECONNECT_ATTEMPTS: u32 = 3;

/// Voice input controller
pub struct VoiceController {
    /// Swapped out when credentials are refreshed, hence the lock.
    asr_client: Arc<AsyncMutex<Arc<AsrClient>>>,
    audio_capture: Arc<AudioCapture>,
    text_inserter: Arc<TextInserter>,
    is_recording: Arc<AtomicBool>,
    stop_signal: Arc<AtomicBool>,
    /// Present when automatic credential refresh is enabled.
    credential_store: Option<Arc<CredentialStore>>,
    /// Connect timeout propagated to rebuilt clients.
    connect_timeout_secs: u64,
    /// Set when the active token is known to be dead, so the next session
    /// re-registers before connecting instead of retrying a doomed token.
    credentials_dirty: Arc<AtomicBool>,
    /// Present when LLM-based correction of final results is enabled.
    text_corrector: Option<Arc<TextCorrector>>,
    /// Called when recording stops on its own (session dropped and
    /// reconnect attempts were exhausted) rather than via `stop()`, so the UI
    /// can notice and stop showing "recording" for a session that's actually
    /// dead.
    on_stopped_unexpectedly: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl VoiceController {
    /// Create a new voice controller
    pub fn new(
        asr_client: Arc<AsrClient>,
        audio_capture: Arc<AudioCapture>,
        text_inserter: Arc<TextInserter>,
    ) -> Self {
        Self {
            asr_client: Arc::new(AsyncMutex::new(asr_client)),
            audio_capture,
            text_inserter,
            is_recording: Arc::new(AtomicBool::new(false)),
            stop_signal: Arc::new(AtomicBool::new(false)),
            credential_store: None,
            connect_timeout_secs: 8,
            credentials_dirty: Arc::new(AtomicBool::new(false)),
            text_corrector: None,
            on_stopped_unexpectedly: None,
        }
    }

    /// Enable automatic credential refresh on session setup failure.
    ///
    /// Without this the controller still works, it just cannot recover from an
    /// expired token on its own.
    pub fn with_credential_refresh(
        mut self,
        credential_store: Arc<CredentialStore>,
        connect_timeout_secs: u64,
    ) -> Self {
        self.credential_store = Some(credential_store);
        self.connect_timeout_secs = connect_timeout_secs.max(1);
        self
    }

    /// Enable LLM-based correction of final ASR results before insertion.
    pub fn with_text_corrector(mut self, corrector: Arc<TextCorrector>) -> Self {
        self.text_corrector = Some(corrector);
        self
    }

    /// Register a callback for when recording stops on its own (dropped
    /// session, reconnect attempts exhausted) instead of via `stop()`.
    ///
    /// Takes `&mut self` rather than being a consuming builder step because
    /// the caller (the UI layer) typically only has access to the controller
    /// after it's already wrapped in `Arc<Mutex<_>>`.
    pub fn set_stopped_unexpectedly_callback<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_stopped_unexpectedly = Some(Arc::new(callback));
    }

    /// Check if currently recording
    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    /// Toggle voice input on/off
    pub async fn toggle(&mut self) -> Result<()> {
        if self.is_recording() {
            self.stop().await
        } else {
            self.start().await
        }
    }

    /// Establish an ASR session, re-registering the device once if the first
    /// attempt fails in a way that points at expired credentials.
    async fn connect_with_retry(
        &self,
    ) -> Result<(
        tokio::sync::mpsc::Sender<Vec<u8>>,
        tokio::sync::mpsc::Receiver<AsrResponse>,
    )> {
        establish_session(
            &self.asr_client,
            &self.credential_store,
            self.connect_timeout_secs,
            &self.credentials_dirty,
        )
        .await
    }

    /// Start voice input
    pub async fn start(&mut self) -> Result<()> {
        if self.is_recording() {
            return Ok(());
        }

        tracing::info!("Starting voice input...");
        let start_requested_at = std::time::Instant::now();
        self.is_recording.store(true, Ordering::SeqCst);
        self.stop_signal.store(false, Ordering::SeqCst);

        // Establish the ASR session BEFORE capturing audio. Starting capture
        // first meant a slow handshake filled the frame channel and the backlog
        // was discarded ("Channel full, dropping frame").
        let (audio_tx, mut result_rx) = match self.connect_with_retry().await {
            Ok(pair) => pair,
            Err(e) => {
                // Do not leave the controller stuck in the recording state.
                self.is_recording.store(false, Ordering::SeqCst);
                return Err(e);
            }
        };
        tracing::info!(
            "ASR connection established ({}ms since start() was called)",
            start_requested_at.elapsed().as_millis()
        );

        // Now start audio capture into the connected session.
        tracing::debug!("Starting audio capture...");
        if let Err(e) = self.audio_capture.start_into(audio_tx) {
            self.is_recording.store(false, Ordering::SeqCst);
            return Err(e);
        }
        tracing::info!(
            "Audio capture started, frames will be sent to ASR ({}ms since start() was called)",
            start_requested_at.elapsed().as_millis()
        );

        // Clone for the task
        let text_inserter = self.text_inserter.clone();
        let is_recording = self.is_recording.clone();
        let stop_signal = self.stop_signal.clone();
        let audio_capture = self.audio_capture.clone();
        let credential_store = self.credential_store.clone();
        let credentials_dirty = self.credentials_dirty.clone();
        let text_corrector = self.text_corrector.clone();
        let asr_client_for_task = self.asr_client.clone();
        let connect_timeout_secs = self.connect_timeout_secs;
        let on_stopped_unexpectedly = self.on_stopped_unexpectedly.clone();

        // Spawn result processing task
        tokio::spawn(async move {
            let mut last_text = String::new();
            let mut response_count = 0u32;

            tracing::info!("ASR result processing task started");

            loop {
                // Check stop signal
                if stop_signal.load(Ordering::SeqCst) {
                    tracing::info!("Voice input stopped by user (processed {} responses)", response_count);
                    break;
                }

                // Use timeout to periodically check stop signal
                match tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    result_rx.recv()
                ).await {
                    Ok(Some(response)) => {
                        response_count += 1;
                        match response.response_type {
                            ResponseType::InterimResult => {
                                tracing::debug!("[INTERIM #{}] {}", response_count, response.text);
                                println!("📝 [识别中] {}", response.text);
                                if !response.text.is_empty() {
                                    if let Err(e) = update_text(&text_inserter, &last_text, &response.text) {
                                        tracing::error!("Failed to update text: {}", e);
                                    }
                                    last_text = response.text.clone();
                                }
                            }
                            ResponseType::FinalResult => {
                                tracing::info!("[FINAL #{}] {}", response_count, response.text);
                                if !response.text.is_empty() {
                                    let corrected_text = match text_corrector.as_ref() {
                                        Some(corrector) => corrector.correct(&response.text).await,
                                        None => response.text.clone(),
                                    };
                                    println!("✅ [确认] {}", corrected_text);
                                    if let Err(e) = update_text(&text_inserter, &last_text, &corrected_text) {
                                        tracing::error!("Failed to update text: {}", e);
                                    }
                                    // 清空 last_text，这样新的语句不会删除已确认的文字
                                    last_text = String::new();
                                } else {
                                    println!("✅ [确认] {}", response.text);
                                }
                            }
                            ResponseType::SessionFinished => {
                                tracing::info!("ASR session finished (total {} responses)", response_count);
                                println!("🏁 [会话结束]");
                                break;
                            }
                            ResponseType::Error => {
                                tracing::error!("ASR error: {}", response.error_msg);
                                println!("❌ [错误] {}", response.error_msg);
                                // A credential-shaped error can also arrive here,
                                // after the session was nominally established.
                                // Drop the cached token so the next start
                                // re-registers instead of failing the same way.
                                if is_credential_failure(&response.error_msg) {
                                    if let Some(ref store) = credential_store {
                                        tracing::warn!(
                                            "Session failed with a credential error; invalidating cached credentials"
                                        );
                                        println!(
                                            "ℹ️  凭据已失效，下次启动将自动重新注册设备"
                                        );
                                        store.invalidate().await;
                                        credentials_dirty.store(true, Ordering::SeqCst);
                                    }
                                }

                                // Don't just give up on a dropped connection:
                                // most "instability" users see is a plain
                                // network blip, not a credential problem, and
                                // used to end the session silently with no
                                // way back short of re-pressing the hotkey.
                                if stop_signal.load(Ordering::SeqCst) {
                                    break;
                                }
                                match try_reconnect(
                                    &audio_capture,
                                    &asr_client_for_task,
                                    &credential_store,
                                    connect_timeout_secs,
                                    &credentials_dirty,
                                )
                                .await
                                {
                                    Some(new_result_rx) => {
                                        result_rx = new_result_rx;
                                        last_text.clear();
                                        continue;
                                    }
                                    None => break,
                                }
                            }
                            _ => {
                                tracing::trace!("Other response type: {:?}", response.response_type);
                            }
                        }
                    }
                    Ok(None) => {
                        // Channel closed without an explicit error response
                        // (shouldn't normally happen now that the ASR client
                        // always sends one before its task ends, but handled
                        // the same way as a safety net).
                        tracing::warn!("ASR result channel closed unexpectedly");
                        if stop_signal.load(Ordering::SeqCst) {
                            break;
                        }
                        match try_reconnect(
                            &audio_capture,
                            &asr_client_for_task,
                            &credential_store,
                            connect_timeout_secs,
                            &credentials_dirty,
                        )
                        .await
                        {
                            Some(new_result_rx) => {
                                result_rx = new_result_rx;
                                last_text.clear();
                                continue;
                            }
                            None => break,
                        }
                    }
                    Err(_) => {
                        // Timeout, continue loop to check stop signal
                        continue;
                    }
                }
            }

            // Cleanup
            audio_capture.stop();
            is_recording.store(false, Ordering::SeqCst);

            // If the loop ended without the user asking to stop (session
            // dropped and reconnects were exhausted, or the server ended the
            // session), let the UI know so it doesn't keep showing
            // "recording" for a session that's actually dead.
            if !stop_signal.load(Ordering::SeqCst) {
                if let Some(callback) = on_stopped_unexpectedly {
                    callback();
                }
            }
        });

        Ok(())
    }

    /// Stop voice input
    pub async fn stop(&mut self) -> Result<()> {
        if !self.is_recording() {
            return Ok(());
        }

        tracing::info!("Stopping voice input...");

        // Signal stop
        self.stop_signal.store(true, Ordering::SeqCst);
        self.audio_capture.stop();

        // Wait a bit for the task to finish
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        
        self.is_recording.store(false, Ordering::SeqCst);

        Ok(())
    }
}

/// Establish an ASR session, re-registering the device once if the first
/// attempt fails in a way that points at expired credentials.
///
/// Free function (rather than a `&self` method) so it can also be called
/// from the background result-processing task spawned by `start()`, to
/// reconnect after a mid-session drop without needing `VoiceController`
/// itself to be shared across tasks.
async fn establish_session(
    asr_client: &Arc<AsyncMutex<Arc<AsrClient>>>,
    credential_store: &Option<Arc<CredentialStore>>,
    connect_timeout_secs: u64,
    credentials_dirty: &Arc<AtomicBool>,
) -> Result<(mpsc::Sender<Vec<u8>>, mpsc::Receiver<AsrResponse>)> {
    // If a previous session died from a credential error, the cached token is
    // already known to be dead; re-register before spending a connect attempt.
    if credentials_dirty.swap(false, Ordering::SeqCst) {
        if let Some(store) = credential_store {
            tracing::info!("Credentials were marked stale; re-registering before connecting");
            match store.ensure_credentials().await {
                Ok(fresh) => {
                    let rebuilt = Arc::new(AsrClient::with_timeout(fresh, connect_timeout_secs));
                    *asr_client.lock().await = rebuilt;
                }
                Err(e) => {
                    // Fall through and try the existing client anyway.
                    tracing::warn!("Pre-emptive credential refresh failed: {}", e);
                }
            }
        }
    }

    let (audio_tx, audio_rx) = AudioCapture::channel();

    tracing::debug!("Connecting to ASR server...");
    let client = asr_client.lock().await.clone();
    let first_attempt = client.start_realtime(audio_rx).await;

    let setup_err = match first_attempt {
        Ok(result_rx) => return Ok((audio_tx, result_rx)),
        Err(e) => e,
    };

    // Only retry for credential-shaped failures, and only when a store is
    // available to refresh them.
    let refreshable = setup_err
        .downcast_ref::<AsrSetupError>()
        .map(|e| e.warrants_credential_refresh())
        .unwrap_or(false);

    if !refreshable {
        return Err(setup_err);
    }

    let Some(store) = credential_store.clone() else {
        tracing::warn!("ASR setup failed and no credential store is configured for refresh");
        return Err(setup_err);
    };

    tracing::warn!(
        "ASR session setup failed ({}); refreshing credentials and retrying once",
        setup_err
    );
    println!("⚠️  ASR 会话建立失败，正在重新注册设备后重试...");

    let fresh = store.force_refresh().await.map_err(|refresh_err| {
        anyhow::anyhow!(
            "ASR setup failed ({}) and credential refresh also failed: {}",
            setup_err,
            refresh_err
        )
    })?;

    // Replace the shared client so subsequent sessions use the new token.
    let new_client = Arc::new(AsrClient::with_timeout(fresh, connect_timeout_secs));
    *asr_client.lock().await = new_client.clone();

    // The first receiver was consumed by the failed attempt; make a new pair.
    let (audio_tx, audio_rx) = AudioCapture::channel();
    let result_rx = new_client.start_realtime(audio_rx).await.map_err(|e| {
        anyhow::anyhow!(
            "ASR setup failed again after credential refresh: {} (original: {})",
            e,
            setup_err
        )
    })?;

    tracing::info!("ASR session established after credential refresh");
    println!("✅ 重新注册成功，已建立 ASR 会话");
    Ok((audio_tx, result_rx))
}

/// Try to reconnect a dropped mid-session ASR connection, with a few retries
/// and increasing backoff. Returns `None` if all attempts failed.
async fn reconnect_with_backoff(
    asr_client: &Arc<AsyncMutex<Arc<AsrClient>>>,
    credential_store: &Option<Arc<CredentialStore>>,
    connect_timeout_secs: u64,
    credentials_dirty: &Arc<AtomicBool>,
) -> Option<(mpsc::Sender<Vec<u8>>, mpsc::Receiver<AsrResponse>)> {
    let mut backoff = Duration::from_secs(1);

    for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
        tracing::warn!(
            "ASR session dropped; reconnect attempt {}/{}",
            attempt,
            MAX_RECONNECT_ATTEMPTS
        );
        println!(
            "🔄 连接断开，正在自动重连（第 {}/{} 次）...",
            attempt, MAX_RECONNECT_ATTEMPTS
        );

        match establish_session(asr_client, credential_store, connect_timeout_secs, credentials_dirty)
            .await
        {
            Ok(pair) => {
                tracing::info!("Reconnected after {} attempt(s)", attempt);
                println!("✅ 已重新连接，继续识别");
                return Some(pair);
            }
            Err(e) => {
                tracing::warn!("Reconnect attempt {} failed: {}", attempt, e);
                if attempt < MAX_RECONNECT_ATTEMPTS {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
            }
        }
    }

    tracing::error!("Giving up after {} reconnect attempts", MAX_RECONNECT_ATTEMPTS);
    println!("❌ 多次重连失败，已停止语音输入，请手动重新开始");
    None
}

/// Reconnect a dropped ASR session and redirect audio capture into the new
/// one. Stops the (now-dead) old capture stream first so the new one doesn't
/// fight it for the input device. Returns `None` if reconnecting or
/// restarting capture failed.
async fn try_reconnect(
    audio_capture: &Arc<AudioCapture>,
    asr_client: &Arc<AsyncMutex<Arc<AsrClient>>>,
    credential_store: &Option<Arc<CredentialStore>>,
    connect_timeout_secs: u64,
    credentials_dirty: &Arc<AtomicBool>,
) -> Option<mpsc::Receiver<AsrResponse>> {
    audio_capture.stop();
    // Give the old capture thread a moment to actually tear down its stream
    // before opening a new one on the same device.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (new_audio_tx, new_result_rx) =
        reconnect_with_backoff(asr_client, credential_store, connect_timeout_secs, credentials_dirty)
            .await?;

    if let Err(e) = audio_capture.start_into(new_audio_tx) {
        tracing::error!("Reconnected to ASR but failed to restart audio capture: {}", e);
        println!("❌ 重连成功但音频采集启动失败：{}", e);
        return None;
    }

    Some(new_result_rx)
}

/// Update text in the focused window using incremental updates
///
/// Uses prefix matching to minimize deletions and insertions:
/// 1. Find the common prefix between old and new text
/// 2. Only delete characters beyond the common prefix
/// 3. Only append the new suffix
/// 
/// This significantly reduces visual flickering compared to full replacement.
fn update_text(text_inserter: &TextInserter, old_text: &str, new_text: &str) -> Result<()> {
    // 找到公共前缀长度（无需删除和重新输入的部分）
    let common_prefix_len = old_text
        .chars()
        .zip(new_text.chars())
        .take_while(|(a, b)| a == b)
        .count();
    
    // 计算需要删除的字符数 = 旧文本超出公共前缀的部分
    let chars_to_delete = old_text.chars().count() - common_prefix_len;
    
    // 需要追加的文本 = 新文本超出公共前缀的部分
    let text_to_append: String = new_text.chars().skip(common_prefix_len).collect();
    
    // 执行增量更新
    if chars_to_delete > 0 {
        text_inserter.delete_chars(chars_to_delete)?;
    }
    if !text_to_append.is_empty() {
        text_inserter.insert(&text_to_append)?;
    }
    
    tracing::debug!(
        "Updated text incrementally: '{}' -> '{}' (kept {} chars, deleted {}, appended '{}')",
        old_text, new_text, common_prefix_len, chars_to_delete, text_to_append
    );
    Ok(())
}
