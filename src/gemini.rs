//! Client for the Gemini REST API (`generateContent` with function calling).

use std::time::Duration;

use serde_json::{json, Value};

use crate::http::Http;

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Things that can go wrong when talking to the API.
#[derive(Debug, Clone)]
pub enum ApiError {
    /// The server answered with an error status.
    Http {
        status: u16,
        message: String,
        retry_after: Option<u64>,
    },
    /// The request never completed (offline, DNS, TLS, timeout).
    Transport(String),
    /// The prompt was rejected by the safety system.
    Blocked(String),
    /// The reply did not have the expected shape.
    Malformed(String),
}

impl ApiError {
    /// Temporary problems that are worth retrying.
    pub fn is_retryable(&self) -> bool {
        match self {
            // A daily quota ("retry in 22h") will not come back within a few seconds.
            ApiError::Http {
                status: 429,
                message,
                ..
            } => retry_hint_secs(message).is_none_or(|s| s <= 120),
            ApiError::Http { status, .. } => matches!(status, 408 | 500 | 502 | 503 | 504),
            ApiError::Transport(_) => true,
            ApiError::Blocked(_) | ApiError::Malformed(_) => false,
        }
    }

    /// A message suitable for showing to the user.
    pub fn user_message(&self) -> String {
        match self {
            ApiError::Http {
                status, message, ..
            } => {
                let lower = message.to_ascii_lowercase();
                match status {
                    400 if lower.contains("api key") => {
                        "The API key is not valid. Open Settings and check the key.".to_string()
                    }
                    400 => format!("The request was rejected: {message}"),
                    401 => "The API key was rejected. Open Settings and check the key.".to_string(),
                    403 => format!(
                        "Access denied. The key may be restricted, or the model is not enabled for it. ({message})"
                    ),
                    404 => format!(
                        "The model was not found. Choose another model in Settings. ({message})"
                    ),
                    429 if retry_hint_secs(message).is_some_and(|s| s > 120) => format!(
                        "The quota of this model is used up for now. Open Settings and choose another model or another AI service (for example NVIDIA). ({message})"
                    ),
                    429 => format!("Rate limit or quota reached. Wait a moment, or pick another model. ({message})"),
                    500..=599 => format!("The AI service had a temporary problem ({status}). {message}"),
                    _ => format!("The AI service returned HTTP {status}: {message}"),
                }
            }
            ApiError::Transport(m) => format!("Could not reach the AI service: {m}"),
            ApiError::Blocked(m) => format!("The AI service blocked this request: {m}"),
            ApiError::Malformed(m) => format!("Unexpected answer from the AI service: {m}"),
        }
    }
}

/// Reads "Please retry in 22h30.6s" / "retry in 28s" from an error message, in seconds.
pub fn retry_hint_secs(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let rest = &lower[lower.find("retry in ")? + "retry in ".len()..];
    let mut total = 0.0f64;
    let mut number = String::new();
    let mut seen = false;
    for c in rest.chars() {
        if c.is_ascii_digit() || (c == '.' && !number.is_empty()) {
            number.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => break,
        };
        total += number.parse::<f64>().ok()? * unit;
        number.clear();
        seen = true;
    }
    seen.then_some(total as u64)
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for ApiError {}

/// A request from the model to run one of our tools.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub id: Option<String>,
    pub name: String,
    pub args: Value,
}

/// Token counts reported by the API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// One answer from the model.
#[derive(Debug, Clone)]
pub struct ModelReply {
    /// The complete `{"role":"model","parts":[...]}` object. It is sent back
    /// unchanged on the next request so that thought signatures are preserved.
    pub content: Value,
    /// All text parts joined together.
    pub text: String,
    pub calls: Vec<FunctionCall>,
    pub finish_reason: Option<String>,
    pub usage: Usage,
}

impl ModelReply {
    pub fn has_parts(&self) -> bool {
        self.content["parts"]
            .as_array()
            .map(|p| !p.is_empty())
            .unwrap_or(false)
    }
}

/// A model returned by the model list endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// Identifier to put in the request URL, e.g. `gemini-2.5-flash`.
    pub id: String,
    pub display_name: String,
}

pub struct GeminiClient {
    http: Http,
    base_url: String,
    api_key: String,
}

impl GeminiClient {
    pub fn new(api_key: &str, base_url: &str) -> Self {
        Self {
            http: Http::new(Duration::from_secs(600), 64 << 20),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
        }
    }

    fn headers(&self) -> [(&str, &str); 1] {
        [("x-goog-api-key", self.api_key.as_str())]
    }

    /// Sends the conversation to the model and returns its next turn.
    pub fn generate(
        &self,
        model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
    ) -> Result<ModelReply, ApiError> {
        let model = model.trim().trim_start_matches("models/");
        let url = format!("{}/models/{}:generateContent", self.base_url, model);
        let mut body = json!({
            "systemInstruction": { "parts": [ { "text": system } ] },
            "contents": contents,
        });
        if tool_declarations
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            body["tools"] = json!([ { "functionDeclarations": tool_declarations } ]);
            body["toolConfig"] = json!({ "functionCallingConfig": { "mode": "AUTO" } });
        }
        let response = self
            .http
            .post_json(&url, &self.headers(), &body)
            .map_err(|e| ApiError::Transport(e.0))?;
        if !response.is_success() {
            return Err(http_error(&response));
        }
        let value = response.json().map_err(ApiError::Malformed)?;
        parse_reply(&value)
    }

    /// Lists the models that support `generateContent`. Doubles as an API key check.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        let mut models = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..10 {
            let mut url = format!("{}/models?pageSize=100", self.base_url);
            if let Some(t) = &page_token {
                url.push_str(&format!("&pageToken={}", crate::http::url_encode(t)));
            }
            let response = self
                .http
                .get(&url, &self.headers())
                .map_err(|e| ApiError::Transport(e.0))?;
            if !response.is_success() {
                return Err(http_error(&response));
            }
            let value = response.json().map_err(ApiError::Malformed)?;
            models.extend(parse_models(&value));
            page_token = value["nextPageToken"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(sort_models(models))
    }
}

fn http_error(response: &crate::http::Response) -> ApiError {
    let message = serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            let t = response.text();
            crate::util::truncate_end(t.trim(), 300)
        });
    ApiError::Http {
        status: response.status,
        message,
        retry_after: response.retry_after,
    }
}

/// Converts a `generateContent` response into a [`ModelReply`].
pub fn parse_reply(value: &Value) -> Result<ModelReply, ApiError> {
    if let Some(reason) = value["promptFeedback"]["blockReason"].as_str() {
        return Err(ApiError::Blocked(reason.to_string()));
    }
    let candidate = value["candidates"]
        .as_array()
        .and_then(|c| c.first())
        .ok_or_else(|| ApiError::Malformed("the reply contains no candidates".into()))?;

    let parts: Vec<Value> = candidate["content"]["parts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in &parts {
        if part["thought"].as_bool() == Some(true) {
            continue;
        }
        if let Some(t) = part["text"].as_str() {
            text.push_str(t);
        }
        if let Some(call) = part.get("functionCall").filter(|c| c.is_object()) {
            let name = call["name"].as_str().unwrap_or_default().to_string();
            if name.is_empty() {
                continue;
            }
            calls.push(FunctionCall {
                id: call["id"].as_str().map(str::to_string),
                name,
                args: call.get("args").cloned().unwrap_or_else(|| json!({})),
            });
        }
    }

    let usage = &value["usageMetadata"];
    Ok(ModelReply {
        content: json!({ "role": "model", "parts": parts }),
        text,
        calls,
        finish_reason: candidate["finishReason"].as_str().map(str::to_string),
        usage: Usage {
            prompt_tokens: usage["promptTokenCount"].as_u64().unwrap_or(0),
            output_tokens: usage["candidatesTokenCount"].as_u64().unwrap_or(0),
            total_tokens: usage["totalTokenCount"].as_u64().unwrap_or(0),
        },
    })
}

/// Extracts usable chat models from a model list response.
pub fn parse_models(value: &Value) -> Vec<ModelInfo> {
    const SKIP: &[&str] = &[
        "embedding",
        "tts",
        "image",
        "live",
        "audio",
        "native",
        "computer-use",
        "robotics",
        "imagen",
        "veo",
        "aqa",
        "learnlm",
        "vision",
        "thinking-exp",
        "gemma",
    ];
    value["models"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|m| {
                    m["supportedGenerationMethods"]
                        .as_array()
                        .map(|methods| methods.iter().any(|x| x == "generateContent"))
                        .unwrap_or(false)
                })
                .filter_map(|m| {
                    let id = m["name"]
                        .as_str()?
                        .trim_start_matches("models/")
                        .to_string();
                    if !id.starts_with("gemini") || SKIP.iter().any(|s| id.contains(s)) {
                        return None;
                    }
                    Some(ModelInfo {
                        display_name: m["displayName"].as_str().unwrap_or(&id).to_string(),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Orders models with the newest/most capable first and removes duplicates.
pub fn sort_models(mut models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    fn rank(id: &str) -> (i32, i32) {
        let tier = if id.contains("pro") {
            0
        } else if id.contains("flash-lite") {
            2
        } else if id.contains("flash") {
            1
        } else {
            3
        };
        let preview = i32::from(id.contains("preview") || id.contains("exp"));
        (tier, preview)
    }
    models.sort_by(|a, b| {
        let (ta, pa) = rank(&a.id);
        let (tb, pb) = rank(&b.id);
        ta.cmp(&tb).then(pa.cmp(&pb)).then(b.id.cmp(&a.id))
    });
    models.dedup_by(|a, b| a.id == b.id);
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_and_calls_and_keeps_raw_parts() {
        let v = json!({
            "candidates": [{
                "content": { "role": "model", "parts": [
                    { "text": "Let me look." },
                    { "functionCall": { "name": "read_file", "args": { "path": "a.py" } }, "thoughtSignature": "abc==" }
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15 }
        });
        let r = parse_reply(&v).unwrap();
        assert_eq!(r.text, "Let me look.");
        assert_eq!(r.calls.len(), 1);
        assert_eq!(r.calls[0].name, "read_file");
        assert_eq!(r.calls[0].args["path"], "a.py");
        assert_eq!(r.usage.total_tokens, 15);
        // The signature must survive untouched.
        assert_eq!(r.content["parts"][1]["thoughtSignature"], "abc==");
        assert_eq!(r.content["role"], "model");
    }

    #[test]
    fn skips_thought_summaries() {
        let v = json!({"candidates":[{"content":{"parts":[
            {"text":"internal","thought":true},{"text":"visible"}]}}]});
        assert_eq!(parse_reply(&v).unwrap().text, "visible");
    }

    #[test]
    fn empty_candidate_is_allowed_but_has_no_parts() {
        let v = json!({"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL"}]});
        let r = parse_reply(&v).unwrap();
        assert!(!r.has_parts());
        assert_eq!(r.finish_reason.as_deref(), Some("MALFORMED_FUNCTION_CALL"));
    }

    #[test]
    fn blocked_prompts_are_errors() {
        let v = json!({"promptFeedback":{"blockReason":"SAFETY"}});
        assert!(matches!(parse_reply(&v), Err(ApiError::Blocked(_))));
        assert!(matches!(
            parse_reply(&json!({})),
            Err(ApiError::Malformed(_))
        ));
    }

    #[test]
    fn model_list_is_filtered_and_sorted() {
        let v = json!({"models":[
            {"name":"models/gemini-2.5-flash","displayName":"Gemini 2.5 Flash","supportedGenerationMethods":["generateContent"]},
            {"name":"models/gemini-2.5-pro","displayName":"Gemini 2.5 Pro","supportedGenerationMethods":["generateContent","countTokens"]},
            {"name":"models/gemini-embedding-001","supportedGenerationMethods":["embedContent"]},
            {"name":"models/gemini-2.5-flash-preview-tts","supportedGenerationMethods":["generateContent"]},
            {"name":"models/gemma-3-27b","supportedGenerationMethods":["generateContent"]},
            {"name":"models/gemini-2.5-flash-lite","supportedGenerationMethods":["generateContent"]},
        ]});
        let ids: Vec<String> = sort_models(parse_models(&v))
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            ids,
            [
                "gemini-2.5-pro",
                "gemini-2.5-flash",
                "gemini-2.5-flash-lite"
            ]
        );
    }

    #[test]
    fn error_classification() {
        let e = ApiError::Http {
            status: 429,
            message: "quota".into(),
            retry_after: Some(3),
        };
        assert!(e.is_retryable());
        let e = ApiError::Http {
            status: 400,
            message: "API key not valid. Please pass a valid API key.".into(),
            retry_after: None,
        };
        assert!(!e.is_retryable());
        assert!(e.user_message().contains("Settings"));
        assert!(ApiError::Transport("x".into()).is_retryable());
    }

    #[test]
    fn daily_quota_is_not_retried() {
        assert_eq!(
            retry_hint_secs("Please retry in 22h30.633346646s."),
            Some(79230)
        );
        assert_eq!(retry_hint_secs("Please retry in 28.5s"), Some(28));
        assert_eq!(retry_hint_secs("Please retry in 1m5s"), Some(65));
        assert_eq!(retry_hint_secs("no hint here"), None);
        let daily = ApiError::Http {
            status: 429,
            message: "You exceeded your current quota. Please retry in 22h30.6s.".into(),
            retry_after: None,
        };
        assert!(!daily.is_retryable());
        assert!(daily.user_message().contains("another AI service"));
        let short = ApiError::Http {
            status: 429,
            message: "Resource exhausted. Please retry in 28s".into(),
            retry_after: None,
        };
        assert!(short.is_retryable());
        let plain = ApiError::Http {
            status: 429,
            message: "Too many requests".into(),
            retry_after: None,
        };
        assert!(plain.is_retryable());
    }

    #[test]
    fn talks_to_a_mock_server() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let mut req = server.recv().unwrap();
                let key = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("x-goog-api-key"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let mut body = String::new();
                std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
                seen.push((req.url().to_string(), key, body));
                let reply = if req.url().contains(":generateContent") {
                    r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#
                } else {
                    r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key."}}"#
                };
                let status = if req.url().contains(":generateContent") {
                    200
                } else {
                    400
                };
                req.respond(tiny_http::Response::from_string(reply).with_status_code(status))
                    .unwrap();
            }
            seen
        });
        let client = GeminiClient::new("KEY123", &format!("http://127.0.0.1:{port}/v1beta"));
        let reply = client
            .generate(
                "models/gemini-x",
                "system text",
                &[json!({"role":"user","parts":[{"text":"hello"}]})],
                &json!([{"name":"t","description":"d","parameters":{"type":"OBJECT","properties":{"a":{"type":"STRING"}}}}]),
            )
            .unwrap();
        assert_eq!(reply.text, "hi");
        let err = client.list_models().unwrap_err();
        assert!(err.user_message().contains("not valid"));
        let seen = handle.join().unwrap();
        assert_eq!(seen[0].0, "/v1beta/models/gemini-x:generateContent");
        assert_eq!(seen[0].1, "KEY123");
        let sent: Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(sent["systemInstruction"]["parts"][0]["text"], "system text");
        assert_eq!(sent["tools"][0]["functionDeclarations"][0]["name"], "t");
        assert_eq!(sent["contents"][0]["role"], "user");
        // The key must be sent as a header, never in the URL.
        assert!(!seen[0].0.contains("KEY123"));
    }
}
