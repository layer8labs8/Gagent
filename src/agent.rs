//! The agent loop: ask the model, run the tools it requests, feed the results
//! back, and repeat until the model gives a final answer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::gemini::{ApiError, FunctionCall, ModelReply, Usage};
use crate::llm::ModelClient;
use crate::tools::{self, ToolContext};
use crate::util::{sha256_hex, truncate_end};

/// How many times a failed API call is retried.
const MAX_RETRIES: u32 = 4;
/// How many times an empty model answer is re-requested.
const MAX_EMPTY_REPLIES: u32 = 2;
/// Old messages are shortened once the history grows beyond this many characters
/// (about 15 000 tokens). Every step sends the whole history again, so a long
/// history is paid for at every single step.
const COMPACT_THRESHOLD_CHARS: usize = 60_000;
/// The newest messages are never shortened.
const KEEP_RECENT_MESSAGES: usize = 8;
/// Text arguments of old tool calls (file contents) longer than this are replaced.
const MAX_OLD_ARG_CHARS: usize = 300;
/// Old answers of the model longer than this are cut.
const MAX_OLD_TEXT_CHARS: usize = 1500;
/// Hard stop for runaway repetition.
const MAX_IDENTICAL_REPEATS: u32 = 5;
/// Malformed tool calls in a row after which the turn is stopped.
const MAX_MALFORMED_CALLS: u32 = 8;

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
    /// Tokens one request used, and the company that answered it.
    Spent { company: String, tokens: u64 },
    /// Something the user should know: another model took over because one ran out of quota.
    Notice(String),
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    Finished,
    Cancelled,
    /// The model used up its step budget without finishing.
    StepLimit,
    /// The task used up its token budget (all requests of the task added up).
    TokenLimit,
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
    /// Stop when the task has used this many tokens; 0 means no limit.
    pub token_budget: u64,
    pub system_prompt: String,
    /// Wait before the first retry; doubles each time.
    pub retry_base_delay: Duration,
}

pub(crate) enum CallError {
    Api(ApiError),
    Cancelled,
}

/// Runs one user message to completion.
///
/// `history` holds the raw Gemini conversation and is extended in place, so a
/// later call continues the same conversation.
pub fn run_turn(
    client: &Arc<dyn ModelClient>,
    cfg: &AgentConfig,
    history: &mut Vec<Value>,
    user_text: &str,
    ctx: &ToolContext,
    emit: &dyn Fn(AgentEvent),
    cancel: &AtomicBool,
) -> TurnResult {
    let mut first_parts = vec![json!({ "text": user_text })];
    if ctx.vision {
        first_parts.extend(attached_pictures(user_text, ctx));
    }
    history.push(json!({ "role": "user", "parts": first_parts }));

    let declarations = tools::declarations_for(ctx.vision);
    let mut usage = Usage::default();
    let mut steps = 0;
    let mut final_text = String::new();
    let mut tool_counter: u32 = 0;
    let mut empty_replies = 0;
    let mut repeats: HashMap<String, (String, u32)> = HashMap::new();
    // Tool calls in a row that failed, and that were malformed.
    let mut fail_streak: u32 = 0;
    let mut slip_streak: u32 = 0;

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
        if cfg.token_budget > 0 && usage.total_tokens >= cfg.token_budget {
            return finish(TurnEnd::TokenLimit, final_text, usage, steps);
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
        if reply.usage.total_tokens > 0 {
            emit(AgentEvent::Spent {
                company: client.company(),
                tokens: reply.usage.total_tokens,
            });
        }

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

        let mut image_parts: Vec<Value> = Vec::new();
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
                name: tools::canonical_tool_name(&call.name),
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

            let who = client.serving_model().unwrap_or_else(|| cfg.model.clone());
            crate::modelstats::record_tool_call(&who, out.slip);
            fail_streak = if out.ok { 0 } else { fail_streak + 1 };
            slip_streak = if out.slip { slip_streak + 1 } else { 0 };
            if slip_streak >= 2 {
                out.text.push_str(&format!("\n[{slip_streak} malformed tool calls in a row. Call one of the listed tools by its exact name, with one JSON object that holds the parameters from its description.]"));
            } else if fail_streak >= 3 && fail_streak % 3 == 0 {
                out.text.push_str(&format!("\n[{fail_streak} tool calls in a row failed. Stop and read the error text; open the file with read_file to see what is really there; fix the cause before you run anything again. Do not repeat or guess.]"));
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
            image_parts.extend(out.images.iter().map(|i| i.part()));

            if slip_streak >= MAX_MALFORMED_CALLS {
                response_parts.append(&mut image_parts);
                history.push(json!({ "role": "user", "parts": response_parts }));
                return finish(
                    TurnEnd::Failed(
                        "The model keeps sending malformed tool calls and was stopped. Pick another model (Auto avoids models that do this).".into(),
                    ),
                    final_text,
                    usage,
                    steps,
                );
            }
            if seen >= MAX_IDENTICAL_REPEATS {
                response_parts.append(&mut image_parts);
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
        response_parts.append(&mut image_parts);
        history.push(json!({ "role": "user", "parts": response_parts }));
    }
}

/// Header the window puts in front of a message that has files attached.
const ATTACHED_HEADER: &str = "[Attached files in the workspace: ";
/// Pictures sent along with one message.
const MAX_ATTACHED_PICTURES: usize = 4;

/// The pictures among the files attached to a message, as parts of the message, so that a model
/// that can see looks at them at once (and the automatic choice picks one that can).
fn attached_pictures(user_text: &str, ctx: &ToolContext) -> Vec<Value> {
    let Some(rest) = user_text.strip_prefix(ATTACHED_HEADER) else {
        return Vec::new();
    };
    let Some((list, _)) = rest.split_once(']') else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    for rel in list.split(", ").map(str::trim) {
        let lower = rel.to_ascii_lowercase();
        if !["png", "jpg", "jpeg", "gif", "webp"]
            .iter()
            .any(|ext| lower.ends_with(&format!(".{ext}")))
        {
            continue;
        }
        let Ok(path) = ctx.work.resolve(rel) else {
            continue;
        };
        if let Ok(image) = crate::vision::load(&path) {
            parts.push(image.part());
        }
        if parts.len() >= MAX_ATTACHED_PICTURES {
            break;
        }
    }
    parts
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
pub(crate) fn call_model(
    client: &Arc<dyn ModelClient>,
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
        for note in client.take_notices() {
            emit(AgentEvent::Notice(note));
        }
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

/// Arguments of file tools that hold whole file contents.
const CONTENT_ARGS: &[(&str, &[&str])] = &[
    ("write_file", &["content", "contents", "text", "file_text"]),
    (
        "edit_file",
        &[
            "old_text",
            "old_string",
            "old",
            "new_text",
            "new_string",
            "new",
        ],
    ),
];

/// Shortens old messages so long sessions stay cheap and within the context window.
///
/// What is removed is also on disk or no longer needed:
/// * the contents of old `write_file` / `edit_file` calls (the files exist),
/// * old tool output (it was already read),
/// * the middle of very long old answers.
///
/// The newest messages and all user messages are left alone.
pub fn compact_history(history: &mut [Value]) {
    // Pictures cost a lot every time they are sent: only the newest messages keep theirs.
    crate::vision::drop_old_images(history, KEEP_RECENT_MESSAGES);
    let size = crate::vision::history_chars(history);
    if size < COMPACT_THRESHOLD_CHARS || history.len() <= KEEP_RECENT_MESSAGES {
        return;
    }
    let limit = history.len() - KEEP_RECENT_MESSAGES;
    for message in history.iter_mut().take(limit) {
        let is_model = message["role"] == "model";
        let Some(parts) = message["parts"].as_array_mut() else {
            continue;
        };
        for part in parts {
            if let Some(response) = part
                .get_mut("functionResponse")
                .and_then(|f| f.get_mut("response"))
            {
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
            if let Some(call) = part.get_mut("functionCall") {
                shrink_call(call);
            }
            if is_model {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if text.chars().count() > MAX_OLD_TEXT_CHARS {
                        let short = format!(
                            "{} [rest of this older answer removed to save space]",
                            truncate_end(text, 600)
                        );
                        part["text"] = json!(short);
                    }
                }
            }
        }
    }
}

/// Replaces long file contents in the arguments of an old tool call.
fn shrink_call(call: &mut Value) {
    let name = call["name"].as_str().unwrap_or_default().to_string();
    let Some((_, keys)) = CONTENT_ARGS.iter().find(|(n, _)| *n == name) else {
        return;
    };
    let Some(args) = call.get_mut("args").and_then(Value::as_object_mut) else {
        return;
    };
    for key in *keys {
        if let Some(text) = args.get(*key).and_then(Value::as_str) {
            let count = text.chars().count();
            if count > MAX_OLD_ARG_CHARS {
                args.insert(
                    (*key).to_string(),
                    json!(format!(
                        "[omitted: {count} characters; the file on disk has them]"
                    )),
                );
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

    fn big_history() -> Vec<Value> {
        let code = "fn line() { /* ... */ }\n".repeat(400); // about 9 000 characters
        let mut history = vec![json!({"role":"user","parts":[{"text":"Build me an editor"}]})];
        for i in 0..12 {
            history.push(json!({"role":"model","parts":[
                {"text": format!("Writing module {i}.")},
                {"functionCall":{"id":"x","name":"write_file","args":{"path":format!("src/m{i}.rs"),"content":code.clone()}}}
            ]}));
            history.push(json!({"role":"user","parts":[{"functionResponse":{"name":"write_file","response":{"output":format!("Created src/m{i}.rs")}}}]}));
        }
        history
    }

    #[test]
    fn old_file_contents_leave_the_history() {
        let mut history = big_history();
        let before: usize = history.iter().map(|m| m.to_string().len()).sum();
        compact_history(&mut history);
        let after: usize = history.iter().map(|m| m.to_string().len()).sum();
        assert!(after * 2 < before, "{before} -> {after} characters");

        // An old write keeps its path and a short placeholder instead of the code.
        let old = &history[1]["parts"][1]["functionCall"]["args"];
        assert_eq!(old["path"], "src/m0.rs");
        let content = old["content"].as_str().unwrap();
        assert!(
            content.starts_with("[omitted: 9600 characters"),
            "{content}"
        );
        // The newest calls keep their full content.
        let last_call = history[history.len() - 2]["parts"][1]["functionCall"]["args"]["content"]
            .as_str()
            .unwrap();
        assert!(last_call.contains("fn line()"));
        // The user's first message is never touched.
        assert_eq!(history[0]["parts"][0]["text"], "Build me an editor");
        // Compacting again changes nothing.
        let once = history.clone();
        compact_history(&mut history);
        assert_eq!(history, once);
    }

    #[test]
    fn short_arguments_and_small_histories_stay() {
        let mut history = big_history();
        history.insert(
            1,
            json!({"role":"model","parts":[{"functionCall":{"name":"edit_file","args":{"path":"a.rs","old_text":"x = 1","new_text":"x = 2"}}}]}),
        );
        compact_history(&mut history);
        let args = &history[1]["parts"][0]["functionCall"]["args"];
        assert_eq!(args["old_text"], "x = 1");
        assert_eq!(args["new_text"], "x = 2");

        let mut small = vec![
            json!({"role":"user","parts":[{"text":"hi"}]}),
            json!({"role":"model","parts":[{"functionCall":{"name":"write_file","args":{"path":"a","content":"c".repeat(2000)}}}]}),
        ];
        let copy = small.clone();
        compact_history(&mut small);
        assert_eq!(small, copy);
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
