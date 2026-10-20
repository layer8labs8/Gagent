//! Client for OpenAI-compatible chat APIs (`/chat/completions` with tool calling).
//!
//! This covers NVIDIA's hosted models (build.nvidia.com), OpenAI, OpenRouter,
//! Ollama, LM Studio, vLLM and every other server that speaks the same protocol.
//!
//! The rest of Gagent keeps the conversation in Gemini's format (`role` +
//! `parts` with `text`, `functionCall` and `functionResponse`). This module
//! converts that history into chat messages for each request and converts the
//! answer back, so a session can switch providers at any time.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::ToolMode;
use crate::gemini::{ApiError, FunctionCall, ModelInfo, ModelReply, Usage};
use crate::http::Http;
use crate::llm::ModelClient;

/// NVIDIA's hosted API (keys start with `nvapi-`, create one at build.nvidia.com).
pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";

/// Groq's hosted models (keys start with `gsk_`, create one at console.groq.com/keys).
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

/// Model ids that are not chat models and are hidden from the model list.
const NOT_CHAT: &[&str] = &[
    "embed",
    "rerank",
    "reward",
    "guard",
    "safety",
    "parse",
    "retriever",
    "tts",
    "whisper",
    "asr",
    "diffusion",
    "dall-e",
    "moderation",
    "clip",
    "ocr",
];

/// Models (`base_url|model`) that rejected native tool calling and are driven
/// through the text format instead. Remembered for the life of the program.
fn text_only_models() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

pub struct OpenAiClient {
    http: Http,
    base_url: String,
    api_key: String,
    max_tokens: u32,
    tool_mode: ToolMode,
}

impl OpenAiClient {
    /// `api_key` may be empty for local servers. `max_tokens` limits one answer.
    pub fn new(api_key: &str, base_url: &str, max_tokens: u32) -> Self {
        Self {
            http: Http::new(Duration::from_secs(900), 64 << 20),
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            max_tokens,
            tool_mode: ToolMode::Auto,
        }
    }

    /// Chooses how tools are offered to the model (default: automatic).
    pub fn with_tool_mode(mut self, mode: ToolMode) -> Self {
        self.tool_mode = mode;
        self
    }

    /// One `/chat/completions` request.
    fn request(
        &self,
        model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
        text_mode: bool,
    ) -> Result<ModelReply, ApiError> {
        let url = format!("{}/chat/completions", self.base_url);
        let has_tools = tool_declarations.as_array().is_some_and(|a| !a.is_empty());
        let (system, tools) = if text_mode && has_tools {
            (
                format!("{system}\n\n{}", text_tool_prompt(tool_declarations)),
                Value::Null,
            )
        } else {
            (system.to_string(), to_tools(tool_declarations))
        };
        let mut body = json!({
            "model": model.trim(),
            "messages": to_messages(&system, contents, text_mode),
            "max_tokens": self.max_tokens,
            "stream": false,
        });
        if tools.as_array().is_some_and(|a| !a.is_empty()) {
            body["tools"] = tools;
        }
        let auth = self.auth_header();
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("Authorization", a.as_str())).collect();
        // `GAGENT_DEBUG=1` prints what is sent and how long the server took (never the key).
        let debug = std::env::var_os("GAGENT_DEBUG").is_some();
        let started = std::time::Instant::now();
        if debug {
            eprintln!(
                "[debug] POST {url} model={} messages={} tools={} max_tokens={} text_mode={text_mode}",
                model.trim(),
                body["messages"].as_array().map_or(0, Vec::len),
                body["tools"].as_array().map_or(0, Vec::len),
                self.max_tokens,
            );
        }
        let response = self
            .http
            .post_json(&url, &headers, &body)
            .map_err(|e| {
                if debug {
                    eprintln!("[debug] failed after {:.1}s: {}", started.elapsed().as_secs_f32(), e.0);
                }
                ApiError::Transport(e.0)
            })?;
        if debug {
            eprintln!(
                "[debug] status {} after {:.1}s, {} bytes{}",
                response.status,
                started.elapsed().as_secs_f32(),
                response.body.len(),
                if response.is_success() {
                    String::new()
                } else {
                    format!(": {}", crate::util::truncate_end(response.text().trim(), 300))
                }
            );
        }
        let host = crate::ratelimit::host_of(&url);
        if !response.is_success() {
            let error = http_error(&response);
            crate::gemini::note_failure(&host, model.trim(), &error);
            return Err(error);
        }
        let value = response.json().map_err(ApiError::Malformed)?;
        let reply = parse_reply(&value)?;
        crate::modelstats::record_ok(
            &host,
            model.trim(),
            started.elapsed().as_secs_f32(),
            reply.usage.total_tokens,
        );
        Ok(reply)
    }

    fn auth_header(&self) -> Option<String> {
        if self.api_key.is_empty() {
            None
        } else {
            Some(format!("Bearer {}", self.api_key))
        }
    }

    fn get(&self, path: &str) -> Result<crate::http::Response, ApiError> {
        let url = format!("{}{path}", self.base_url);
        let auth = self.auth_header();
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("Authorization", a.as_str())).collect();
        self.http
            .get(&url, &headers)
            .map_err(|e| ApiError::Transport(e.0))
    }
}

impl ModelClient for OpenAiClient {
    fn generate(
        &self,
        model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
    ) -> Result<ModelReply, ApiError> {
        let key = format!("{}|{}", self.base_url, model.trim());
        let learned = text_only_models()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&key);
        let text_mode = match self.tool_mode {
            ToolMode::Text => true,
            ToolMode::Native => false,
            ToolMode::Auto => learned,
        };
        let result = self.request(model, system, contents, tool_declarations, text_mode);
        match result {
            // The model or server does not understand `tools`: use the text format instead.
            Err(ApiError::Http {
                status,
                ref message,
                ..
            }) if self.tool_mode == ToolMode::Auto
                && !text_mode
                && rejects_tools(status, message) =>
            {
                text_only_models()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(key);
                self.request(model, system, contents, tool_declarations, true)
            }
            other => other,
        }
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        let response = self.get("/models")?;
        if !response.is_success() {
            return Err(http_error(&response));
        }
        let value = response.json().map_err(ApiError::Malformed)?;
        Ok(parse_models(&value))
    }
}

fn http_error(response: &crate::http::Response) -> ApiError {
    let parsed = serde_json::from_slice::<Value>(&response.body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .or_else(|| v["detail"].as_str())
                .or_else(|| v["message"].as_str())
        })
        .map(str::to_string)
        .unwrap_or_else(|| crate::util::truncate_end(response.text().trim(), 300));
    ApiError::Http {
        status: response.status,
        message,
        retry_after: response.retry_after,
    }
}

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

/// Turns Gemini-style tool declarations into OpenAI `tools`.
pub fn to_tools(declarations: &Value) -> Value {
    let list = declarations.as_array().cloned().unwrap_or_default();
    Value::Array(
        list.into_iter()
            .map(|d| {
                let mut function = json!({
                    "name": d["name"],
                    "description": d["description"],
                });
                if d.get("parameters").is_some() {
                    function["parameters"] = lower_types(d["parameters"].clone());
                }
                json!({ "type": "function", "function": function })
            })
            .collect(),
    )
}

/// Gemini writes schema types in upper case (`OBJECT`); JSON Schema wants lower case.
fn lower_types(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = match (&k[..], &v) {
                        ("type", Value::String(s)) => Value::String(s.to_ascii_lowercase()),
                        _ => lower_types(v),
                    };
                    (k, v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(lower_types).collect()),
        other => other,
    }
}

/// Converts the stored conversation into chat messages.
///
/// With `text_mode` the messages contain no native tool calls: calls are written
/// into the assistant text as `<tool_call>` blocks and results come back as
/// `<tool_response>` blocks in user messages.
pub fn to_messages(system: &str, contents: &[Value], text_mode: bool) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system })];
    // Tool calls of the latest assistant message that have not been answered yet.
    let mut pending: VecDeque<(String, String)> = VecDeque::new();
    let mut counter = 0usize;

    for message in contents {
        let parts = message["parts"].as_array().cloned().unwrap_or_default();
        if message["role"] == "model" {
            let mut text = String::new();
            let mut calls = Vec::new();
            pending.clear();
            for part in &parts {
                if part["thought"].as_bool() == Some(true) {
                    continue;
                }
                if let Some(t) = part["text"].as_str() {
                    text.push_str(t);
                }
                if let Some(call) = part.get("functionCall").filter(|c| c.is_object()) {
                    counter += 1;
                    let name = call["name"].as_str().unwrap_or_default().to_string();
                    let id = call["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{counter}"));
                    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": args.to_string() },
                    }));
                    pending.push_back((id, name));
                }
            }
            if text_mode {
                let mut content = text.trim().to_string();
                for call in &calls {
                    let args: Value = serde_json::from_str(
                        call["function"]["arguments"].as_str().unwrap_or("{}"),
                    )
                    .unwrap_or_else(|_| json!({}));
                    let block = json!({ "name": call["function"]["name"], "arguments": args });
                    content.push_str(&format!("\n<tool_call>{block}</tool_call>"));
                }
                if !content.trim().is_empty() {
                    out.push(json!({ "role": "assistant", "content": content.trim() }));
                }
                continue;
            }
            if text.trim().is_empty() && calls.is_empty() {
                continue;
            }
            let mut assistant = json!({
                "role": "assistant",
                "content": if text.trim().is_empty() { Value::Null } else { Value::String(text) },
            });
            if !calls.is_empty() {
                assistant["tool_calls"] = Value::Array(calls);
            }
            out.push(assistant);
        } else {
            let mut user_text = String::new();
            let mut pictures: Vec<Value> = Vec::new();
            for part in &parts {
                if let Some(image) = part.get("inlineData").filter(|i| i.is_object()) {
                    let mime = image["mimeType"].as_str().unwrap_or("image/png");
                    let data = image["data"].as_str().unwrap_or_default();
                    pictures.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{mime};base64,{data}") },
                    }));
                } else if let Some(response) = part.get("functionResponse").filter(|r| r.is_object()) {
                    let name = response["name"].as_str().unwrap_or_default();
                    let content = response_text(&response["response"]);
                    if text_mode {
                        user_text.push_str(&format!(
                            "<tool_response name=\"{name}\">\n{content}\n</tool_response>\n"
                        ));
                        continue;
                    }
                    let id = response["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            let pos = pending.iter().position(|(_, n)| n == name).or(Some(0))?;
                            pending.remove(pos).map(|(id, _)| id)
                        });
                    match id {
                        Some(id) => {
                            pending.retain(|(p, _)| p != &id);
                            out.push(
                                json!({ "role": "tool", "tool_call_id": id, "content": content }),
                            );
                        }
                        // An answer without a matching call cannot be sent as a tool message.
                        None => user_text.push_str(&format!("[result of {name}]\n{content}\n")),
                    }
                } else if let Some(t) = part["text"].as_str() {
                    user_text.push_str(t);
                }
            }
            if !user_text.trim().is_empty() {
                out.push(json!({ "role": "user", "content": user_text }));
            }
            // A tool message holds only text, so the pictures of a tool result follow as a
            // message of the user.
            if !pictures.is_empty() {
                let mut content = vec![json!({
                    "type": "text",
                    "text": "The picture(s) from the tool result above:",
                })];
                content.extend(pictures);
                out.push(json!({ "role": "user", "content": content }));
            }
        }
    }
    out
}

/// The text a tool result is shown as to the model.
fn response_text(response: &Value) -> String {
    if let Some(s) = response["output"].as_str() {
        return s.to_string();
    }
    if let Some(s) = response["error"].as_str() {
        return format!("ERROR: {s}");
    }
    match response {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

/// Converts a `/chat/completions` response into a [`ModelReply`].
pub fn parse_reply(value: &Value) -> Result<ModelReply, ApiError> {
    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
        let message = err["message"]
            .as_str()
            .or_else(|| err.as_str())
            .unwrap_or("unknown error");
        return Err(ApiError::Malformed(message.to_string()));
    }
    let choice = value["choices"]
        .as_array()
        .and_then(|c| c.first())
        .ok_or_else(|| ApiError::Malformed("the reply contains no choices".into()))?;
    let message = &choice["message"];

    let raw_text = match &message["content"] {
        Value::String(s) => s.clone(),
        // Some servers return a list of content parts.
        Value::Array(items) => items
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    let mut text = strip_reasoning(&raw_text);

    let mut parts: Vec<Value> = Vec::new();
    if !text.trim().is_empty() {
        parts.push(json!({ "text": text }));
    }
    let mut calls = Vec::new();
    if let Some(list) = message["tool_calls"].as_array() {
        for (i, tc) in list.iter().enumerate() {
            let name = tc["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            // `run_command<|channel|>commentary` and the like: keep the tool's own name.
            let name = crate::tools::canonical_tool_name(&name);
            let id = tc["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{}", i + 1));
            let args = match &tc["function"]["arguments"] {
                Value::String(s) => lenient_args(s),
                Value::Object(_) => tc["function"]["arguments"].clone(),
                _ => json!({}),
            };
            parts.push(json!({ "functionCall": { "id": id, "name": name, "args": args } }));
            calls.push(FunctionCall {
                id: Some(id),
                name,
                args,
            });
        }
    }

    // Models without native tool calling (or a server that does not parse them)
    // write the calls into their text.
    if calls.is_empty() && text.contains("<tool_call>") {
        let (clean, found) = extract_text_calls(&text);
        if !found.is_empty() {
            text = clean;
            parts.clear();
            if !text.trim().is_empty() {
                parts.push(json!({ "text": text }));
            }
            for (i, (name, args)) in found.into_iter().enumerate() {
                let id = format!("call_t{}", i + 1);
                parts.push(
                    json!({ "functionCall": { "id": id, "name": name, "args": args.clone() } }),
                );
                calls.push(FunctionCall {
                    id: Some(id),
                    name,
                    args,
                });
            }
        }
    }

    let usage = &value["usage"];
    Ok(ModelReply {
        content: json!({ "role": "model", "parts": parts }),
        text,
        calls,
        finish_reason: choice["finish_reason"].as_str().map(str::to_string),
        usage: Usage {
            prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
            total_tokens: usage["total_tokens"].as_u64().unwrap_or(0),
        },
    })
}

/// True when an error says the model cannot do native tool calling.
fn rejects_tools(status: u16, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    matches!(status, 400 | 422 | 501) && (lower.contains("tool") || lower.contains("function call"))
}

/// Describes the tools and the `<tool_call>` format for models that cannot take a `tools` field.
pub fn text_tool_prompt(declarations: &Value) -> String {
    let mut out = String::from(
        "# Tools\n\
You can call tools. To call a tool, write a block exactly like this (you may write several blocks in one answer):\n\
<tool_call>{\"name\": \"TOOL_NAME\", \"arguments\": {\"parameter\": \"value\"}}</tool_call>\n\
The arguments must be valid JSON. After the last block stop writing: the results arrive in the next message as \
<tool_response name=\"TOOL_NAME\"> blocks. When the task is done, answer normally without any block.\n\n\
Available tools:\n",
    );
    for d in declarations.as_array().into_iter().flatten() {
        let params = lower_types(d["parameters"].clone());
        out.push_str(&format!(
            "- {}: {}\n  parameters: {}\n",
            d["name"].as_str().unwrap_or_default(),
            d["description"].as_str().unwrap_or_default(),
            params
        ));
    }
    out
}

/// Finds `<tool_call>{...}</tool_call>` blocks. Returns the text without the
/// blocks and the calls as (name, arguments).
pub fn extract_text_calls(text: &str) -> (String, Vec<(String, Value)>) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut rest = text;
    let mut clean = String::new();
    let mut calls = Vec::new();
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        // A model that stops at the closing tag may leave it out.
        let (body, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            None => (after, ""),
        };
        match parse_call_body(body) {
            Some(call) => {
                clean.push_str(&rest[..start]);
                calls.push(call);
            }
            // Not a valid call: leave the text as it is.
            None => clean.push_str(&rest[..start + OPEN.len() + body.len()]),
        }
        rest = next;
    }
    clean.push_str(rest);
    (clean.trim().to_string(), calls)
}

fn parse_call_body(body: &str) -> Option<(String, Value)> {
    let body = body.trim();
    let body = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .unwrap_or(body);
    let body = body.strip_suffix("```").unwrap_or(body).trim();
    let value = lenient_value(body)?;
    let name = value["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(crate::tools::canonical_tool_name)?;
    let args = ["arguments", "args", "parameters"]
        .iter()
        .find_map(|k| value.get(*k))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args = match args {
        // Some models quote the arguments object.
        Value::String(s) => lenient_args(&s),
        other => other,
    };
    Some((name, args))
}

/// The arguments of a tool call as an object. Models send almost-JSON: raw line breaks inside
/// strings, a code fence around it, text after it, or the object quoted a second time. What
/// cannot be read at all comes back as `{INVALID_ARGS: <what was sent>}`, so the tool can say
/// so (instead of running with no arguments and complaining about a missing one).
pub fn lenient_args(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return json!({});
    }
    match lenient_value(raw) {
        Some(Value::Object(map)) => Value::Object(map),
        Some(Value::Null) => json!({}),
        Some(Value::String(inner)) => lenient_args(&inner),
        _ => {
            let mut map = serde_json::Map::new();
            map.insert(
                crate::tools::INVALID_ARGS.to_string(),
                Value::String(raw.chars().take(300).collect()),
            );
            Value::Object(map)
        }
    }
}

/// The first JSON value in `text`, also when it is not quite valid JSON.
pub fn lenient_value(text: &str) -> Option<Value> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let fixed = escape_raw_control_chars(t);
    if let Ok(v) = serde_json::from_str::<Value>(&fixed) {
        return Some(v);
    }
    // Text around the object: take the first value and ignore the rest.
    let start = fixed.find('{')?;
    serde_json::Deserializer::from_str(&fixed[start..])
        .into_iter::<Value>()
        .next()?
        .ok()
}

/// JSON does not allow raw line breaks and tabs inside strings, but models write them there.
fn escape_raw_control_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if !in_string {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        } else if escaped {
            escaped = false;
            out.push(c);
        } else {
            match c {
                '\\' => {
                    escaped = true;
                    out.push(c);
                }
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
    }
    out
}

/// Removes the model's visible reasoning (`<think>...</think>`) from an answer.
pub fn strip_reasoning(text: &str) -> String {
    let mut out = text.to_string();
    // A template may drop the opening tag and leave only `...</think>`.
    if let Some(end) = out.rfind("</think>") {
        if !out[..end].contains("<think>") {
            out = out[end + "</think>".len()..].to_string();
        }
    }
    while let Some(start) = out.find("<think>") {
        match out[start..].find("</think>") {
            Some(rel_end) => out.replace_range(start..start + rel_end + "</think>".len(), ""),
            // Unfinished reasoning (the answer was cut off): drop the rest.
            None => out.truncate(start),
        }
    }
    out.trim().to_string()
}

/// Extracts usable chat models from a `/models` response.
pub fn parse_models(value: &Value) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = value["data"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| m["id"].as_str())
                .filter(|id| {
                    let lower = id.to_ascii_lowercase();
                    !NOT_CHAT.iter().any(|skip| lower.contains(skip))
                })
                .map(|id| ModelInfo {
                    id: id.to_string(),
                    display_name: id.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    // NVIDIA's own models first, then alphabetical.
    models.sort_by(|a, b| {
        let na = a.id.starts_with("nvidia/");
        let nb = b.id.starts_with("nvidia/");
        nb.cmp(&na).then_with(|| a.id.cmp(&b.id))
    });
    models.dedup_by(|a, b| a.id == b.id);
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_of_a_tool_result_follow_as_a_user_message() {
        let history = vec![
            json!({"role":"user","parts":[{"text":"look at the page"}]}),
            json!({"role":"model","parts":[
                {"functionCall":{"name":"screenshot","args":{"path":"index.html"}}}
            ]}),
            json!({"role":"user","parts":[
                {"functionResponse":{"name":"screenshot","response":{"output":"Screenshot saved."}}},
                {"inlineData":{"mimeType":"image/png","data":"QUJD"}}
            ]}),
        ];
        let messages = to_messages("sys", &history, false);
        let tool = messages.iter().position(|m| m["role"] == "tool").unwrap();
        assert_eq!(messages[tool]["content"], "Screenshot saved.");
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][1]["type"], "image_url");
        assert_eq!(last["content"][1]["image_url"]["url"], "data:image/png;base64,QUJD");
        assert!(messages.len() > tool + 1, "the picture comes after the tool message");
    }

    #[test]
    fn nearly_valid_tool_arguments_are_read() {
        // A raw line break inside a string.
        let a = lenient_args("{\"path\": \"a.py\", \"content\": \"x = 1\ny = 2\"}");
        assert_eq!(a["content"], "x = 1\ny = 2");
        // A code fence, text after the object, the object quoted a second time.
        assert_eq!(lenient_args("```json\n{\"path\": \"a\"}\n```")["path"], "a");
        assert_eq!(lenient_args("{\"path\": \"a\"} and that is all")["path"], "a");
        assert_eq!(lenient_args("\"{\\\"path\\\": \\\"a\\\"}\"")["path"], "a");
        assert_eq!(lenient_args("  "), json!({}));
        assert_eq!(lenient_args("null"), json!({}));
        // Cut off in the middle: said so, not run with no arguments.
        let bad = lenient_args("{\"path\": \"a.py\", \"content\": \"print(");
        assert!(bad[crate::tools::INVALID_ARGS].as_str().unwrap().contains("a.py"));
        assert!(lenient_args("not json at all")[crate::tools::INVALID_ARGS].is_string());
    }

    #[test]
    fn a_tool_call_in_a_foreign_format_is_cleaned() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [
                        {"id": "c1", "function": {"name": "run_command<|channel|>commentary", "arguments": "{\"command\": \"ls\"}"}},
                        {"id": "c2", "function": {"name": "functions.write_file", "arguments": "{\"path\": \"a.txt\", \"content\": \"1\n2\"}"}},
                        {"id": "c3", "function": {"name": "read_file", "arguments": "{\"path\": "}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let r = parse_reply(&v).unwrap();
        assert_eq!(r.calls[0].name, "run_command");
        assert_eq!(r.calls[0].args["command"], "ls");
        assert_eq!(r.calls[1].name, "write_file");
        assert_eq!(r.calls[1].args["content"], "1\n2");
        assert_eq!(r.calls[2].name, "read_file");
        assert!(r.calls[2].args.get(crate::tools::INVALID_ARGS).is_some());
        // The text format is read the same way.
        let (_, found) = extract_text_calls(
            "<tool_call>{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}} trailing</tool_call>",
        );
        assert_eq!(found[0].0, "run_command");
    }

    #[test]
    fn tool_schemas_are_converted() {
        let decl = json!([{
            "name": "read_file",
            "description": "Read",
            "parameters": {
                "type": "OBJECT",
                "properties": { "path": {"type": "STRING"}, "n": {"type": "INTEGER"} },
                "required": ["path"]
            }
        }]);
        let tools = to_tools(&decl);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(tools[0]["function"]["parameters"]["required"][0], "path");
    }

    #[test]
    fn history_becomes_chat_messages() {
        let history = vec![
            json!({"role":"user","parts":[{"text":"fix it"}]}),
            json!({"role":"model","parts":[
                {"text":"Looking."},
                {"functionCall":{"name":"read_file","args":{"path":"a.py"}},"thoughtSignature":"sig"},
                {"functionCall":{"id":"x9","name":"list_files","args":{}}}
            ]}),
            json!({"role":"user","parts":[
                {"functionResponse":{"name":"read_file","response":{"output":"print(1)"}}},
                {"functionResponse":{"id":"x9","name":"list_files","response":{"error":"boom"}}}
            ]}),
            json!({"role":"model","parts":[{"text":"Done."}]}),
        ];
        let m = to_messages("SYS", &history, false);
        assert_eq!(m[0], json!({"role":"system","content":"SYS"}));
        assert_eq!(m[1], json!({"role":"user","content":"fix it"}));
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "Looking.");
        let calls = m[2]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["function"]["name"], "read_file");
        assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"a.py\"}");
        // The call without an id got one, and its answer points back to it.
        let first_id = calls[0]["id"].as_str().unwrap();
        assert!(!first_id.is_empty());
        assert_eq!(calls[1]["id"], "x9");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], first_id);
        assert_eq!(m[3]["content"], "print(1)");
        assert_eq!(m[4]["tool_call_id"], "x9");
        assert_eq!(m[4]["content"], "ERROR: boom");
        assert_eq!(m[5], json!({"role":"assistant","content":"Done."}));
        assert_eq!(m.len(), 6);
    }

    #[test]
    fn assistant_with_only_calls_has_null_content() {
        let history = vec![json!({"role":"model","parts":[
            {"functionCall":{"id":"c1","name":"list_files","args":{}}}]})];
        let m = to_messages("S", &history, false);
        assert!(m[1]["content"].is_null());
        assert_eq!(m[1]["tool_calls"][0]["id"], "c1");
    }

    #[test]
    fn reply_with_tool_calls_is_parsed() {
        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {"id":"call_a","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.py\"}"}},
                        {"id":"call_b","type":"function","function":{"name":"list_files","arguments":""}}
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        });
        let r = parse_reply(&v).unwrap();
        assert_eq!(r.calls.len(), 2);
        assert_eq!(r.calls[0].id.as_deref(), Some("call_a"));
        assert_eq!(r.calls[0].args["path"], "a.py");
        assert_eq!(r.calls[1].args, json!({}));
        assert_eq!(r.usage.total_tokens, 10);
        assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
        assert!(r.has_parts());
        // The stored form round-trips into the next request.
        let again = to_messages("S", &[r.content.clone()], false);
        assert_eq!(again[1]["tool_calls"][0]["id"], "call_a");
    }

    #[test]
    fn text_mode_history_has_no_native_tool_calls() {
        let history = vec![
            json!({"role":"user","parts":[{"text":"go"}]}),
            json!({"role":"model","parts":[
                {"text":"Reading."},
                {"functionCall":{"id":"c1","name":"read_file","args":{"path":"a.py"}}}
            ]}),
            json!({"role":"user","parts":[
                {"functionResponse":{"id":"c1","name":"read_file","response":{"output":"print(1)"}}}
            ]}),
        ];
        let m = to_messages("S", &history, true);
        assert_eq!(m.len(), 4);
        assert!(m[2].get("tool_calls").is_none());
        let a = m[2]["content"].as_str().unwrap();
        assert!(a.starts_with("Reading."));
        assert!(a.contains("<tool_call>"));
        assert!(a.contains("\"name\":\"read_file\""));
        assert_eq!(m[3]["role"], "user");
        let u = m[3]["content"].as_str().unwrap();
        assert!(u.contains("<tool_response name=\"read_file\">"));
        assert!(u.contains("print(1)"));
    }

    #[test]
    fn calls_written_as_text_are_found() {
        let text = "Let me look.\n<tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"a.py\"}}</tool_call>\n<tool_call>\n```json\n{\"name\":\"list_files\",\"args\":\"{\\\"path\\\":\\\"src\\\"}\"}\n```\n</tool_call>";
        let (clean, calls) = extract_text_calls(text);
        assert_eq!(clean, "Let me look.");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "read_file");
        assert_eq!(calls[0].1["path"], "a.py");
        assert_eq!(calls[1].0, "list_files");
        assert_eq!(calls[1].1["path"], "src");

        // A missing closing tag and broken JSON.
        let (_, calls) = extract_text_calls("<tool_call>{\"name\":\"x\",\"arguments\":{}}");
        assert_eq!(calls.len(), 1);
        let (clean, calls) = extract_text_calls("see <tool_call>not json</tool_call> here");
        assert!(calls.is_empty());
        assert!(clean.contains("not json"));
    }

    #[test]
    fn native_reply_with_calls_in_text_is_understood() {
        let v = json!({"choices":[{"message":{"role":"assistant",
            "content":"<think>x</think>Ok <tool_call>{\"name\":\"list_files\",\"arguments\":{}}</tool_call>"},
            "finish_reason":"stop"}]});
        let r = parse_reply(&v).unwrap();
        assert_eq!(r.text, "Ok");
        assert_eq!(r.calls.len(), 1);
        assert_eq!(r.calls[0].name, "list_files");
        assert!(r.calls[0].id.is_some());
    }

    #[test]
    fn tool_prompt_lists_every_tool() {
        let decl = json!([{"name":"read_file","description":"Read a file","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"}}}}]);
        let p = text_tool_prompt(&decl);
        assert!(p.contains("<tool_call>"));
        assert!(p.contains("- read_file: Read a file"));
        assert!(p.contains("\"type\":\"object\""));
    }

    #[test]
    fn tool_rejections_are_recognized() {
        assert!(rejects_tools(
            400,
            "\"auto\" tool choice requires --enable-auto-tool-choice"
        ));
        assert!(rejects_tools(
            422,
            "This model does not support function calling"
        ));
        assert!(!rejects_tools(401, "tool"));
        assert!(!rejects_tools(400, "max_tokens is too large"));
    }

    #[test]
    fn reasoning_is_removed_from_answers() {
        assert_eq!(strip_reasoning("<think>hmm</think>Answer"), "Answer");
        assert_eq!(
            strip_reasoning("hidden thoughts</think>\n\nAnswer"),
            "Answer"
        );
        assert_eq!(
            strip_reasoning("A<think>x</think>B<think>y</think>C"),
            "ABC"
        );
        assert_eq!(strip_reasoning("Answer<think>cut off"), "Answer");
        assert_eq!(strip_reasoning("plain"), "plain");
    }

    #[test]
    fn content_parts_and_errors() {
        let v = json!({"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]});
        assert_eq!(parse_reply(&v).unwrap().text, "ab");
        assert!(matches!(
            parse_reply(&json!({})),
            Err(ApiError::Malformed(_))
        ));
        let e = json!({"error":{"message":"bad"}});
        assert!(matches!(parse_reply(&e), Err(ApiError::Malformed(m)) if m == "bad"));
        let empty = json!({"choices":[{"message":{"content":""},"finish_reason":"length"}]});
        assert!(!parse_reply(&empty).unwrap().has_parts());
    }

    #[test]
    fn model_list_is_filtered_and_sorted() {
        let v = json!({"data":[
            {"id":"meta/llama-3.3-70b-instruct"},
            {"id":"nvidia/nemotron-3.5-super-vl-120b-a12b"},
            {"id":"nvidia/nv-embedqa-e5-v5"},
            {"id":"nvidia/llama-3.1-nemotron-safety-guard-8b-v3"},
            {"id":"baai/bge-m3-embed"},
            {"id":"nvidia/nemotron-3-super-120b-a12b"},
        ]});
        let ids: Vec<String> = parse_models(&v).into_iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            [
                "nvidia/nemotron-3-super-120b-a12b",
                "nvidia/nemotron-3.5-super-vl-120b-a12b",
                "meta/llama-3.3-70b-instruct"
            ]
        );
    }
}
