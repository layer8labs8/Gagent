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

    /// Things the client wants the user to know ("model X is out of quota, Y answers instead").
    /// Taken (and forgotten) by whoever calls it.
    fn take_notices(&self) -> Vec<String> {
        Vec::new()
    }

    /// The model that answered last, when the client picks it itself (automatic choice).
    fn serving_model(&self) -> Option<String> {
        None
    }

    /// The company that answers the requests (`Gemini`, `NVIDIA`, `Groq`, or the host of another
    /// server), for the count of tokens per company.
    fn company(&self) -> String {
        String::new()
    }
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

    fn company(&self) -> String {
        "Gemini".to_string()
    }
}

/// Creates the client for the settings: one that chooses the model itself when `auto_pick` is on,
/// one that goes on with another model when the chosen one fails (`auto_failover`), or else the
/// plain client of the chosen model.
pub fn make_client(settings: &Settings) -> Arc<dyn ModelClient> {
    use crate::router::{AutoClient, Mode};
    let start = Some((settings.provider, settings.active_model().to_string()));
    if settings.auto_pick {
        return Arc::new(AutoClient::new(settings.clone(), Mode::Pick, start));
    }
    if settings.auto_failover {
        return Arc::new(AutoClient::new(settings.clone(), Mode::Failover, start));
    }
    make_direct(settings)
}

/// The plain client of the provider and model chosen in the settings.
pub fn make_direct(settings: &Settings) -> Arc<dyn ModelClient> {
    let key = settings.effective_api_key();
    let base = settings.api_base_url();
    crate::ratelimit::configure(&crate::ratelimit::host_of(&base), settings.rate_limit());
    match settings.provider {
        Provider::Gemini => Arc::new(GeminiClient::new(&key, &base)),
        Provider::Nvidia | Provider::Groq | Provider::Openai => Arc::new(
            OpenAiClient::new(&key, &base, settings.max_output_tokens)
                .with_tool_mode(settings.tool_mode),
        ),
    }
}
