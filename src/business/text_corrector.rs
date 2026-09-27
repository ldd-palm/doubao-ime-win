//! LLM-based ASR text correction
//!
//! Sends the final ASR transcript to a Doubao model via the Volcano Ark
//! chat completions API (OpenAI-compatible) to fix homophones, typos, and
//! punctuation before the text is inserted.

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::data::LlmConfig;

const SYSTEM_PROMPT: &str = "你是语音识别结果校对助手。任务：只修正输入文本中的同音字错误、错别字和标点，不要改变原意，不要增删实质内容，不要输出任何解释，只输出修正后的文本本身。如果输入本身没有问题，原样输出。";

/// Corrects ASR final-result text via a Doubao (Volcano Ark) chat model.
pub struct TextCorrector {
    client: Client,
    endpoint_id: String,
    api_key: String,
    base_url: String,
    timeout: Duration,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

impl TextCorrector {
    /// Build a corrector from config. Returns `None` if correction is
    /// disabled or required credentials are missing, in which case the
    /// caller should skip correction entirely rather than fail.
    ///
    /// The API key is read from the `ARK_API_KEY` environment variable if
    /// set (preferred), falling back to `config.api_key`.
    pub fn from_config(config: &LlmConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        let api_key = std::env::var("ARK_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .or_else(|| Some(config.api_key.clone()).filter(|k| !k.is_empty()));

        let api_key = match api_key {
            Some(k) => k,
            None => {
                tracing::warn!(
                    "LLM correction is enabled but no API key was found (set ARK_API_KEY or [llm].api_key in config.toml); disabling correction"
                );
                return None;
            }
        };

        if config.endpoint_id.is_empty() {
            tracing::warn!(
                "LLM correction is enabled but [llm].endpoint_id is empty; disabling correction"
            );
            return None;
        }

        tracing::info!("LLM correction enabled (endpoint: {})", config.endpoint_id);

        Some(Self {
            client: Client::new(),
            endpoint_id: config.endpoint_id.clone(),
            api_key,
            base_url: config.base_url.clone(),
            timeout: Duration::from_secs(config.timeout_secs.max(1)),
        })
    }

    /// Correct a final ASR transcript.
    ///
    /// On any failure (timeout, network error, malformed response), returns
    /// the original text unmodified so a flaky correction call never blocks
    /// text insertion or corrupts the result.
    pub async fn correct(&self, text: &str) -> String {
        if text.trim().is_empty() {
            return text.to_string();
        }

        match self.try_correct(text).await {
            Ok(corrected) if !corrected.trim().is_empty() => corrected,
            Ok(_) => text.to_string(),
            Err(e) => {
                tracing::warn!("LLM correction failed, using original text: {}", e);
                text.to_string()
            }
        }
    }

    async fn try_correct(&self, text: &str) -> Result<String> {
        let body = ChatRequest {
            model: &self.endpoint_id,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: SYSTEM_PROMPT,
                },
                ChatMessage {
                    role: "user",
                    content: text,
                },
            ],
            temperature: 0.1,
        };

        let send_result = tokio::time::timeout(
            self.timeout,
            self.client
                .post(&self.base_url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send(),
        )
        .await;

        let response = match send_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => return Err(anyhow!("correction request failed: {}", e)),
            Err(_) => {
                return Err(anyhow!(
                    "correction request timed out after {}s",
                    self.timeout.as_secs()
                ))
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            return Err(anyhow!("correction API returned {}: {}", status, detail));
        }

        let parsed: ChatCompletionResponse = response.json().await?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow!("correction response had no choices"))?;

        Ok(content.trim().to_string())
    }
}
