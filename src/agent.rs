//! The agent loop: ask Gemini, run the tools it requests, feed the results
//! back, and repeat until the model gives a final answer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::gemini::{ApiError, FunctionCall, GeminiClient, ModelReply, Usage};
use crate::tools::{self, ToolContext};
use crate::util::{sha256_hex, truncate_end};

/// How many times a failed API call is retried.
const MAX_RETRIES: u32 = 4;
/// How many times an empty model answer is re-requested.
const MAX_EMPTY_REPLIES: u32 = 2;
/// Old tool output is shortened once the history grows beyond this many characters.
const COMPACT_THRESHOLD_CHARS: usize = 350_000;
/// The newest messages are never shortened.
const KEEP_RECENT_MESSAGES: usize = 10;
/// Hard stop for runaway repetition.
const MAX_IDENTICAL_REPEATS: u32 = 5;

/// Progress reports for the UI or the terminal.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// A short status line such as "Thinking...".
    Status(String),
    /// Text written by the model.
    Text(String),
    ToolStart {
        id: u32,
        name: String,
        summary: String,
    },
    ToolEnd {
        id: u32,
        ok: bool,
        summary: String,
        output: String,
        millis: u64,
    },
    /// Files in the workspace may have changed.
    FilesChanged,
    /// Total token usage of the current turn so far.
    Usage(Usage),
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    Finished,
    Cancelled,
    /// The model used up its step budget without finishing.
    StepLimit,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct TurnResult {
    pub end: TurnEnd,
    /// The model's last text.
    pub final_text: String,
    pub usage: Usage,
    pub steps: u32,
}

pub struct AgentConfig {
    pub model: String,
    pub max_steps: u32,
    pub system_prompt: String,
    /// Wait before the first retry; doubles each time.
    pub retry_base_delay: Duration,
}

enum CallError {
    Api(ApiError),
    Cancelled,
}

/// Runs one user message to completion.
///
/// `history` holds the raw Gemini conversation and is extended in place, so a
/// later call continues the same conversation.
pub fn run_turn(
    client: &Arc<GeminiClient>,
    cfg: &AgentConfig,
    history: &mut Vec<Value>,
    user_text: &str,
    ctx: &ToolContext,
    emit: &dyn Fn(AgentEvent),
    cancel: &AtomicBool,
) -> TurnResult {
    history.push(json!({ "role": "user", "parts": [ { "text": user_text } ] }));

    let declarations = tools::declarations(ctx.cloud.is_some());
    let mut usage = Usage::default();
    let mut steps = 0;
    let mut final_text = String::new();
    let mut tool_counter: u32 = 0;
    let mut empty_replies = 0;
    let mut repeats: HashMap<String, (String, u32)> = HashMap::new();

    let finish = |end: TurnEnd, final_text: String, usage: Usage, steps: u32| TurnResult {
        end,
        final_text,
        usage,
        steps,
    };

    loop {
        if cancel.load(Ordering::Relaxed) {
            return finish(TurnEnd::Cancelled, final_text, usage, steps);
        }
        if steps >= cfg.max_steps {
            return finish(TurnEnd::StepLimit, final_text, usage, steps);
        }
        steps += 1;

        compact_history(history);
        emit(AgentEvent::Status("Thinking...".into()));
        let reply = match call_model(client, cfg, history, &declarations, emit, cancel) {
            Ok(r) => r,
            Err(CallError::Cancelled) => {
                return finish(TurnEnd::Cancelled, final_text, usage, steps)
            }
            Err(CallError::Api(e)) => {
                return finish(TurnEnd::Failed(e.user_message()), final_text, usage, steps);
            }
        };
        usage.add(reply.usage);
        emit(AgentEvent::Usage(usage));

        let empty = !reply.has_parts() || (reply.text.trim().is_empty() && reply.calls.is_empty());
        if empty {
            empty_replies += 1;
            if empty_replies > MAX_EMPTY_REPLIES {
                let why = reply
                    .finish_reason
                    .unwrap_or_else(|| "no reason given".into());
                return finish(
                    TurnEnd::Failed(format!("The model returned an empty answer ({why}). Try again or pick another model.")),
                    final_text,
                    usage,
                    steps,
                );
            }
            steps -= 1;
            continue;
        }
        empty_replies = 0;

        history.push(reply.content.clone());
        if !reply.text.trim().is_empty() {
            final_text = reply.text.trim().to_string();
            emit(AgentEvent::Text(final_text.clone()));
        }
        if reply.calls.is_empty() {
            return finish(TurnEnd::Finished, final_text, usage, steps);
        }

        let mut response_parts = Vec::new();
        for call in &reply.calls {
            if cancel.load(Ordering::Relaxed) {
                // Every function call must be answered, otherwise the history becomes invalid.
                response_parts.push(function_response(call, false, "Cancelled by the user."));
                continue;
            }
            tool_counter += 1;
            let id = tool_counter;
            emit(AgentEvent::ToolStart {
                id,
                name: call.name.clone(),
                summary: tools::describe_call(&call.name, &call.args),
            });
            emit(AgentEvent::Status(format!(
                "{}...",
                tools::describe_call(&call.name, &call.args)
            )));
            let started = Instant::now();
            let mut out = tools::execute(ctx, &call.name, &call.args);
            let millis = started.elapsed().as_millis() as u64;

            let sig = sha256_hex(format!("{}:{}", call.name, call.args).as_bytes());
            let digest = sha256_hex(out.text.as_bytes());
            let entry = repeats.entry(sig).or_insert((digest.clone(), 0));
            if entry.0 == digest {
                entry.1 += 1;
            } else {
                *entry = (digest, 1);
            }
            let seen = entry.1;
            if seen >= 3 {
                out.text.push_str("\n[You already made this exact call and got this same result. Change your approach instead of repeating it.]");
            }

            emit(AgentEvent::ToolEnd {
                id,
                ok: out.ok,
                summary: out.summary.clone(),
                output: out.text.clone(),
                millis,
            });
            if out.mutated {
                emit(AgentEvent::FilesChanged);
            }
            response_parts.push(function_response(call, out.ok, &out.text));

            if seen >= MAX_IDENTICAL_REPEATS {
                history.push(json!({ "role": "user", "parts": response_parts }));
                return finish(
                    TurnEnd::Failed(
                        "The agent kept repeating the same action and was stopped.".into(),
                    ),
                    final_text,
                    usage,
                    steps,
                );
            }
        }
        history.push(json!({ "role": "user", "parts": response_parts }));
    }
}

fn function_response(call: &FunctionCall, ok: bool, text: &str) -> Value {
    let key = if ok { "output" } else { "error" };
    let mut fr = json!({ "name": call.name, "response": { key: text } });
    if let Some(id) = &call.id {
        fr["id"] = json!(id);
    }
    json!({ "functionResponse": fr })
}

/// Calls the API without blocking cancellation, retrying temporary failures.
fn call_model(
    client: &Arc<GeminiClient>,
    cfg: &AgentConfig,
    history: &[Value],
    declarations: &Value,
    emit: &dyn Fn(AgentEvent),
    cancel: &AtomicBool,
) -> Result<ModelReply, CallError> {
    let mut attempt = 0;
    loop {
        let (tx, rx) = mpsc::channel();
        {
            let client = client.clone();
            let model = cfg.model.clone();
            let system = cfg.system_prompt.clone();
            let contents = history.to_vec();
            let declarations = declarations.clone();
            std::thread::spawn(move || {
                let _ = tx.send(client.generate(&model, &system, &contents, &declarations));
            });
        }
        let result = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => break r,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(CallError::Cancelled);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(ApiError::Transport(
                        "the request thread stopped unexpectedly".into(),
                    ));
                }
            }
        };
        match result {
            Ok(reply) => return Ok(reply),
            Err(e) if e.is_retryable() && attempt < MAX_RETRIES => {
                let wait = match &e {
                    ApiError::Http {
                        retry_after: Some(s),
                        ..
                    } => Duration::from_secs((*s).min(120)),
                    _ => cfg.retry_base_delay * 2u32.pow(attempt),
                }
                .min(Duration::from_secs(120));
                emit(AgentEvent::Status(format!(
                    "{} Retrying in {}s ({}/{})...",
                    truncate_end(&e.user_message(), 120),
                    wait.as_secs().max(1),
                    attempt + 1,
                    MAX_RETRIES
                )));
                attempt += 1;
                let end = Instant::now() + wait;
                while Instant::now() < end {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(CallError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
                }
            }
            Err(e) => return Err(CallError::Api(e)),
        }
    }
}

/// Shortens old tool output so long sessions stay within the context window.
pub fn compact_history(history: &mut [Value]) {
    let size: usize = history.iter().map(|m| m.to_string().len()).sum();
    if size < COMPACT_THRESHOLD_CHARS || history.len() <= KEEP_RECENT_MESSAGES {
        return;
    }
    let limit = history.len() - KEEP_RECENT_MESSAGES;
    for message in history.iter_mut().take(limit) {
        let Some(parts) = message["parts"].as_array_mut() else {
            continue;
        };
        for part in parts {
            let Some(response) = part
                .get_mut("functionResponse")
                .and_then(|f| f.get_mut("response"))
            else {
                continue;
            };
            for key in ["output", "error"] {
                if let Some(text) = response.get(key).and_then(Value::as_str) {
                    if text.chars().count() > 600 {
                        let short = format!(
                            "{} [older output removed to save space]",
                            truncate_end(text, 400)
                        );
                        response[key] = json!(short);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_shortens_only_old_large_outputs() {
        let big = "x".repeat(2_000);
        let mut history = Vec::new();
        for i in 0..40 {
            history.push(json!({"role":"model","parts":[{"functionCall":{"name":"read_file","args":{"path":format!("f{i}")}}}]}));
            history.push(json!({"role":"user","parts":[{"functionResponse":{"name":"read_file","response":{"output": big.repeat(5)}}}]}));
        }
        let before = history.last().unwrap().to_string();
        compact_history(&mut history);
        let first = history[1]["parts"][0]["functionResponse"]["response"]["output"]
            .as_str()
            .unwrap();
        assert!(
            first.chars().count() < 600,
            "old output should be shortened"
        );
        assert!(first.contains("older output removed"));
        assert_eq!(
            history.last().unwrap().to_string(),
            before,
            "recent output stays intact"
        );
    }

    #[test]
    fn small_histories_are_untouched() {
        let mut history = vec![json!({"role":"user","parts":[{"text":"hi"}]})];
        let copy = history.clone();
        compact_history(&mut history);
        assert_eq!(history, copy);
    }

    #[test]
    fn function_responses_echo_ids() {
        let call = FunctionCall {
            id: Some("c1".into()),
            name: "x".into(),
            args: json!({}),
        };
        let v = function_response(&call, false, "bad");
        assert_eq!(v["functionResponse"]["id"], "c1");
        assert_eq!(v["functionResponse"]["response"]["error"], "bad");
        let call = FunctionCall {
            id: None,
            name: "x".into(),
            args: json!({}),
        };
        let v = function_response(&call, true, "good");
        assert!(v["functionResponse"].get("id").is_none());
        assert_eq!(v["functionResponse"]["response"]["output"], "good");
    }
}
