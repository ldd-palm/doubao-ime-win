//! Application Configuration
//!
//! Handles loading and saving application configuration.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub hotkey: HotkeyConfig,
    #[serde(default)]
    pub floating_button: FloatingButtonConfig,
    #[serde(default)]
    pub asr: AsrConfig,
    #[serde(default)]
    pub llm: LlmConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            hotkey: HotkeyConfig::default(),
            floating_button: FloatingButtonConfig::default(),
            asr: AsrConfig::default(),
            llm: LlmConfig::default(),
        }
    }
}

impl AppConfig {
    /// Get the config file path
    pub fn config_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("config.toml")
    }

    /// Get the credentials file path
    pub fn credentials_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("credentials.json")
    }

    /// Load configuration from file or create default
    pub fn load_or_default() -> Result<Self> {
        let path = Self::config_path();

        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let config: AppConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = AppConfig::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Save configuration to file
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }
}

/// General configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "zh-CN".to_string()
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            auto_start: false,
            language: default_language(),
        }
    }
}

/// Hotkey configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default = "default_hotkey_mode")]
    pub mode: String,
    #[serde(default = "default_combo_key")]
    pub combo_key: String,
    #[serde(default = "default_double_tap_key")]
    pub double_tap_key: String,
    #[serde(default = "default_double_tap_interval")]
    pub double_tap_interval: u64,
    /// A second key that, pressed while `double_tap_key` is held down, pauses
    /// or resumes the whole service instead of toggling recording (e.g.
    /// "RAlt" + "Space" = hold Right Alt and tap Space). Empty string
    /// disables this. Only applies to the bare-modifier-key hook path
    /// (double_tap/single_tap modes); combo mode has no chord concept.
    #[serde(default = "default_pause_combo_key")]
    pub pause_combo_key: String,
}

fn default_hotkey_mode() -> String {
    "combo".to_string()
}

fn default_combo_key() -> String {
    "Ctrl+Shift+V".to_string()
}

fn default_double_tap_key() -> String {
    "Ctrl".to_string()
}

fn default_double_tap_interval() -> u64 {
    300
}

fn default_pause_combo_key() -> String {
    "Space".to_string()
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mode: default_hotkey_mode(),
            combo_key: default_combo_key(),
            double_tap_key: default_double_tap_key(),
            double_tap_interval: default_double_tap_interval(),
            pause_combo_key: default_pause_combo_key(),
        }
    }
}

/// Floating button configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingButtonConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_position")]
    pub position_x: i32,
    #[serde(default = "default_position")]
    pub position_y: i32,
}

fn default_true() -> bool {
    true
}

fn default_position() -> i32 {
    100
}

impl Default for FloatingButtonConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position_x: 100,
            position_y: 100,
        }
    }
}

/// ASR configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    #[serde(default = "default_true")]
    pub vad_enabled: bool,
    /// Maximum age of cached device credentials, in days.
    ///
    /// The Doubao ASR token expires server-side; a stale token makes the
    /// websocket handshake hang and then fail. Re-registering before the token
    /// goes bad avoids that. Set to 0 to disable expiry checking.
    #[serde(default = "default_credential_ttl_days")]
    pub credential_ttl_days: u64,
    /// Timeout for establishing the ASR websocket connection, in seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
}

fn default_credential_ttl_days() -> u64 {
    3
}

fn default_connect_timeout_secs() -> u64 {
    8
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            vad_enabled: true,
            credential_ttl_days: default_credential_ttl_days(),
            connect_timeout_secs: default_connect_timeout_secs(),
        }
    }
}

/// LLM-based correction of the ASR final transcript (homophones, typos,
/// punctuation) before it's inserted. Works with any OpenAI-compatible chat
/// completions API (Volcano Ark / Doubao, DeepSeek, etc.) — just point
/// `base_url` and `model` at the provider you want.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Model name / inference endpoint id sent as the `model` field, e.g.
    /// "deepseek-flash" or a Volcano Ark endpoint id ("ep-...-xxxxx").
    #[serde(default)]
    pub model: String,
    /// API key. Leave empty to read the `LLM_API_KEY` environment variable
    /// instead (preferred, so the key isn't stored in plaintext next to the
    /// binary). The env var takes priority when both are set.
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    /// Timeout for the correction request, in seconds. On timeout or any
    /// other failure, the original ASR text is used unmodified.
    #[serde(default = "default_llm_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_llm_base_url() -> String {
    "https://api.deepseek.com/chat/completions".to_string()
}

fn default_llm_timeout_secs() -> u64 {
    5
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            api_key: String::new(),
            base_url: default_llm_base_url(),
            timeout_secs: default_llm_timeout_secs(),
        }
    }
}
