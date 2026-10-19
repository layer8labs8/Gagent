//! The AI providers Gagent can talk to.
//!
//! The agent loop only knows the [`ModelClient`] trait. Gemini has its own
//! client; NVIDIA (build.nvidia.com), OpenAI, OpenRouter, Ollama and every
//! other OpenAI-compatible service share [`crate::openai::OpenAiClient`].

use std::sync::Arc;

use serde_json::Value;

use crate::config::{Provider, Settings};
use crate::gemini::{ApiError, GeminiClient, ModelInfo, ModelReply};
use crate::openai::OpenAiClient;

/// A chat model with function calling.
pub trait ModelClient: Send + Sync {
    /// Sends the conversation (Gemini-style `contents`) and returns the model's next turn.
    fn generate(
        &self,
        model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
    ) -> Result<ModelReply, ApiError>;

    /// Lists the usable chat models. Doubles as an API key check.
    fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError>;
}

impl ModelClient for GeminiClient {
    fn generate(
        &self,
        model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
    ) -> Result<ModelReply, ApiError> {
        GeminiClient::generate(self, model, system, contents, tool_declarations)
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        GeminiClient::list_models(self)
    }
}

/// Creates the client for the provider chosen in the settings.
pub fn make_client(settings: &Settings) -> Arc<dyn ModelClient> {
    let key = settings.effective_api_key();
    let base = settings.api_base_url();
    match settings.provider {
        Provider::Gemini => Arc::new(GeminiClient::new(&key, &base)),
        Provider::Nvidia | Provider::Openai => Arc::new(
            OpenAiClient::new(&key, &base, settings.max_output_tokens)
                .with_tool_mode(settings.tool_mode),
        ),
    }
}
