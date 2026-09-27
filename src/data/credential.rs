//! Credential Store
//!
//! Manages device credentials with TTL-based expiry and forced refresh.

use anyhow::Result;
use std::path::PathBuf;
use tokio::sync::Mutex;

use crate::asr::{get_asr_token, register_device, DeviceCredentials};
use crate::data::AppConfig;

/// Credential store for managing device credentials
pub struct CredentialStore {
    credentials_path: PathBuf,
    /// Cached credentials, shared so that a refresh is visible to later callers.
    credentials: Mutex<Option<DeviceCredentials>>,
    /// Maximum age of cached credentials, in days. 0 disables expiry checks.
    ttl_days: u64,
}

impl CredentialStore {
    /// Create a new credential store
    pub fn new(config: &AppConfig) -> Result<Self> {
        let credentials_path = AppConfig::credentials_path();

        // Try to load existing credentials
        let credentials = if credentials_path.exists() {
            match DeviceCredentials::load(&credentials_path) {
                Ok(creds) => Some(creds),
                Err(e) => {
                    tracing::warn!(
                        "Ignoring unreadable credentials file {:?}: {}",
                        credentials_path,
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            credentials_path,
            credentials: Mutex::new(credentials),
            ttl_days: config.asr.credential_ttl_days,
        })
    }

    /// Ensure we have valid, non-expired credentials.
    ///
    /// Reuses the cached credentials when they are complete and still within the
    /// TTL, otherwise registers a new device.
    pub async fn ensure_credentials(&self) -> Result<DeviceCredentials> {
        let mut guard = self.credentials.lock().await;

        if let Some(ref creds) = *guard {
            if creds.is_usable(self.ttl_days) {
                match creds.age() {
                    Some(age) => tracing::info!(
                        "Using cached credentials (age: {} days)",
                        age.as_secs() / 86_400
                    ),
                    None => tracing::info!("Using cached credentials"),
                }
                return Ok(creds.clone());
            }

            // Explain precisely why the cache was rejected; this is the failure
            // mode that used to surface as an opaque hang on connect.
            if !creds.is_complete() {
                tracing::info!("Cached credentials incomplete, re-registering device");
            } else {
                match creds.age() {
                    Some(age) => tracing::info!(
                        "Cached credentials expired (age: {} days, TTL: {} days), re-registering device",
                        age.as_secs() / 86_400,
                        self.ttl_days
                    ),
                    None => tracing::info!(
                        "Cached credentials have no issue timestamp (written by an older version), re-registering device"
                    ),
                }
            }
        }

        let creds = self.register_and_store().await?;
        *guard = Some(creds.clone());
        Ok(creds)
    }

    /// Discard the cached credentials so the next call re-registers.
    ///
    /// Used when a session fails mid-flight with a credential-shaped error: the
    /// token is clearly no longer accepted, so it must not be reused next time.
    pub async fn invalidate(&self) {
        let mut guard = self.credentials.lock().await;
        if guard.is_some() {
            tracing::info!("Invalidating cached credentials");
            *guard = None;
        }
        // Remove the on-disk copy too, otherwise a restart would load it again.
        if self.credentials_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.credentials_path) {
                tracing::warn!(
                    "Could not remove stale credentials file {:?}: {}",
                    self.credentials_path,
                    e
                );
            }
        }
    }

    /// Force a fresh device registration, discarding any cached credentials.
    ///
    /// Used when the server rejects an apparently valid token at session setup.
    pub async fn force_refresh(&self) -> Result<DeviceCredentials> {
        let mut guard = self.credentials.lock().await;
        tracing::info!("Forcing credential refresh");
        *guard = None;

        let creds = self.register_and_store().await?;
        *guard = Some(creds.clone());
        Ok(creds)
    }

    /// Register a new device, fetch an ASR token, and persist the result.
    async fn register_and_store(&self) -> Result<DeviceCredentials> {
        tracing::info!("Registering new device...");
        let mut creds = DeviceCredentials::new_generated();

        // Register device to get device_id
        register_device(&mut creds).await?;

        // Get ASR token (this also stamps issued_at_ms)
        get_asr_token(&mut creds).await?;

        // Save credentials; a failure here is not fatal since the in-memory
        // credentials are still usable for this run.
        match creds.save(&self.credentials_path) {
            Ok(_) => tracing::info!("Credentials saved to {:?}", self.credentials_path),
            Err(e) => tracing::warn!(
                "Could not save credentials to {:?}: {} (continuing with in-memory credentials)",
                self.credentials_path,
                e
            ),
        }

        Ok(creds)
    }
}
