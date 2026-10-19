//! The agent loop against a scripted Gemini server and a scripted sandbox.

mod support;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gagent::agent::{run_turn, AgentConfig, AgentEvent, TurnEnd};
use gagent::config::CheckLevel;
use gagent::gemini::GeminiClient;
use gagent::llm::ModelClient;
use gagent::sandbox::{ExecResult, ScriptedSandbox};
use gagent::tools::ToolContext;
use gagent::workspace::{WalkOptions, Workspace};
use serde_json::{json, Value};
use support::*;

struct Rig {
    _dir: tempfile::TempDir,
    ctx: ToolContext,
    sandbox: Arc<ScriptedSandbox>,
    cancel: Arc<AtomicBool>,
}

fn rig(results: Vec<ExecResult>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = Arc::new(ScriptedSandbox::new(results));
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = ToolContext {
        work: Workspace::new(dir.path()).unwrap(),
        sandbox: sandbox.clone(),
        opts: WalkOptions::default(),
        default_timeout_secs: 30,
        cancel: cancel.clone(),
        check: CheckLevel::Off,
        tools_note: std::sync::Arc::new(std::sync::Mutex::new(Some(String::new()))),
        approvals: std::sync::Arc::new(gagent::permission::Approvals::new(
            gagent::config::PermissionMode::Auto,
        )),
        reads: Default::default(),
    };
    Rig {
        _dir: dir,
        ctx,
        sandbox,
        cancel,
    }
}

fn cfg(max_steps: u32) -> AgentConfig {
    AgentConfig {
        model: "gemini-test".into(),
        max_steps,
        token_budget: 0,
        system_prompt: "You are Gagent (test).".into(),
        retry_base_delay: Duration::from_millis(1),
    }
}

fn client(mock: &MockGemini) -> Arc<dyn ModelClient> {
    Arc::new(GeminiClient::new("TEST-KEY", &mock.base_url))
}

fn collect() -> (Arc<Mutex<Vec<AgentEvent>>>, impl Fn(AgentEvent)) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (events, move |e| sink.lock().unwrap().push(e))
}

fn exec(code: i32, output: &str) -> ExecResult {
    ExecResult {
        exit_code: Some(code),
        output: output.into(),
        duration: Duration::from_millis(250),
        ..Default::default()
    }
}

#[test]
fn finds_and_fixes_a_bug_using_logs() {
    let rig = rig(vec![exec(1, "Traceback (most recent call last):\n  File \"app.py\", line 2\nZeroDivisionError: division by zero\n"), exec(0, "5.0\n")]);
    let mock = MockGemini::start(vec![
        calls(vec![
            json!({"text": "I'll start by looking around."}),
            json!({"functionCall": {"name": "list_files", "args": {}}, "thoughtSignature": "SIG-1"}),
        ]),
        calls(vec![call(
            "write_file",
            json!({"path": "app.py", "content": "def avg(a, b):\n    return (a + b) / 0\nprint(avg(4, 6))\n"}),
        )]),
        calls(vec![call(
            "run_command",
            json!({"command": "python3 app.py"}),
        )]),
        calls(vec![call(
            "edit_file",
            json!({"path": "app.py", "old_text": "/ 0", "new_text": "/ 2"}),
        )]),
        calls(vec![call(
            "run_command",
            json!({"command": "python3 app.py"}),
        )]),
        text("Fixed the division by zero; the script now prints 5.0."),
    ]);
    let (events, emit) = collect();
    let mut history = Vec::new();
    let result = run_turn(
        &client(&mock),
        &cfg(20),
        &mut history,
        "Make app.py work",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );

    assert_eq!(result.end, TurnEnd::Finished);
    assert_eq!(
        result.final_text,
        "Fixed the division by zero; the script now prints 5.0."
    );
    assert_eq!(result.steps, 6);
    assert_eq!(result.usage.total_tokens, 660);
    assert_eq!(
        fs::read_to_string(rig.ctx.work.root().join("app.py")).unwrap(),
        "def avg(a, b):\n    return (a + b) / 2\nprint(avg(4, 6))\n"
    );
    assert_eq!(
        rig.sandbox.commands.lock().unwrap().as_slice(),
        ["python3 app.py", "python3 app.py"]
    );

    // Activity events.
    let events = events.lock().unwrap();
    let starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [
            "List files in .",
            "Write app.py",
            "Run `python3 app.py`",
            "Edit app.py",
            "Run `python3 app.py`"
        ]
    );
    let ends: Vec<(bool, String)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolEnd { ok, summary, .. } => Some((*ok, summary.clone())),
            _ => None,
        })
        .collect();
    assert!(!ends[2].0, "the first run failed");
    assert!(ends[4].0, "the second run passed");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::FilesChanged)));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Text(t) if t.contains("looking around"))));

    // What was sent to the API.
    let received = mock.received();
    assert_eq!(received.len(), 6);
    for r in &received {
        assert_eq!(r.api_key, "TEST-KEY");
        assert_eq!(r.url, "/v1beta/models/gemini-test:generateContent");
        assert_eq!(
            r.body["systemInstruction"]["parts"][0]["text"],
            "You are Gagent (test)."
        );
        assert!(
            r.body["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .len()
                >= 7
        );
    }
    // The thought signature came back unchanged on the next request.
    assert_eq!(
        received[1].body["contents"][1]["parts"][1]["thoughtSignature"],
        "SIG-1"
    );
    // The failing log reached the model.
    let last = received.last().unwrap().body["contents"]
        .as_array()
        .unwrap()
        .clone();
    let all = Value::Array(last).to_string();
    assert!(all.contains("ZeroDivisionError"));
    assert!(all.contains("exit code: 1"));
    // Every function call has a matching response, in order.
    let calls_n = count_parts(&history, "model", "functionCall");
    let resp_n = count_parts(&history, "user", "functionResponse");
    assert_eq!(calls_n, 5);
    assert_eq!(calls_n, resp_n);
    assert_eq!(history.last().unwrap()["role"], "model");
}

fn count_parts(history: &[Value], role: &str, key: &str) -> usize {
    history
        .iter()
        .filter(|m| m["role"] == role)
        .flat_map(|m| m["parts"].as_array().cloned().unwrap_or_default())
        .filter(|p| p.get(key).is_some())
        .count()
}

#[test]
fn conversation_continues_across_turns() {
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![text("First answer."), text("Second answer.")]);
    let (_events, emit) = collect();
    let mut history = Vec::new();
    let c = client(&mock);
    run_turn(
        &c,
        &cfg(5),
        &mut history,
        "one",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    run_turn(
        &c,
        &cfg(5),
        &mut history,
        "two",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    let second = &mock.received()[1].body["contents"];
    assert_eq!(second.as_array().unwrap().len(), 3);
    assert_eq!(second[0]["parts"][0]["text"], "one");
    assert_eq!(second[1]["parts"][0]["text"], "First answer.");
    assert_eq!(second[2]["parts"][0]["text"], "two");
}

#[test]
fn parallel_calls_are_answered_in_order() {
    let rig = rig(vec![]);
    fs::write(rig.ctx.work.root().join("a.txt"), "AAA").unwrap();
    fs::write(rig.ctx.work.root().join("b.txt"), "BBB").unwrap();
    let mock = MockGemini::start(vec![
        calls(vec![
            json!({"functionCall": {"id": "c1", "name": "read_file", "args": {"path": "a.txt"}}}),
            json!({"functionCall": {"id": "c2", "name": "read_file", "args": {"path": "b.txt"}}}),
        ]),
        text("done"),
    ]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "read both",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Finished);
    let sent = &mock.received()[1].body["contents"][2];
    assert_eq!(sent["role"], "user");
    let parts = sent["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["functionResponse"]["id"], "c1");
    assert!(parts[0]["functionResponse"]["response"]["output"]
        .as_str()
        .unwrap()
        .contains("AAA"));
    assert_eq!(parts[1]["functionResponse"]["id"], "c2");
    assert!(parts[1]["functionResponse"]["response"]["output"]
        .as_str()
        .unwrap()
        .contains("BBB"));
}

#[test]
fn tool_errors_go_back_to_the_model() {
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![
        calls(vec![call("read_file", json!({"path": "../../etc/passwd"}))]),
        text("I cannot read that."),
    ]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "go",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Finished);
    let fr = &mock.received()[1].body["contents"][2]["parts"][0]["functionResponse"];
    assert!(fr["response"]["error"]
        .as_str()
        .unwrap()
        .contains("'..' is not allowed"));
}

#[test]
fn temporary_api_errors_are_retried() {
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![
        Canned::status(503, "The model is overloaded."),
        Canned::status(429, "Quota exceeded").header("Retry-After", "0"),
        text("Recovered."),
    ]);
    let (events, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "hi",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Finished);
    assert_eq!(r.final_text, "Recovered.");
    assert_eq!(mock.received().len(), 3);
    let retries = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::Status(s) if s.contains("Retrying")))
        .count();
    assert_eq!(retries, 2);
}

#[test]
fn permanent_api_errors_stop_the_turn() {
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![Canned::status(
        400,
        "API key not valid. Please pass a valid API key.",
    )]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "hi",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    match r.end {
        TurnEnd::Failed(msg) => assert!(msg.contains("Settings"), "{msg}"),
        other => panic!("unexpected end {other:?}"),
    }
    assert_eq!(mock.received().len(), 1, "no retry for a bad key");
}

#[test]
fn gives_up_after_repeated_server_errors() {
    let rig = rig(vec![]);
    let script = (0..10).map(|_| Canned::status(503, "overloaded")).collect();
    let mock = MockGemini::start(script);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "hi",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert!(matches!(r.end, TurnEnd::Failed(_)));
    assert_eq!(mock.received().len(), 5, "one try plus four retries");
}

#[test]
fn cancel_interrupts_a_slow_request() {
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![text("too late").delayed(Duration::from_secs(5))]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let flag = rig.cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "hi",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn step_limit_is_enforced() {
    let rig = rig(vec![]);
    fs::write(rig.ctx.work.root().join("f.txt"), "x").unwrap();
    let script = (0..5)
        .map(|i| {
            calls(vec![call(
                "search_files",
                json!({"pattern": format!("p{i}")}),
            )])
        })
        .collect();
    let mock = MockGemini::start(script);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(3),
        &mut history,
        "loop",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::StepLimit);
    assert_eq!(r.steps, 3);
    assert_eq!(mock.received().len(), 3);
    // History stays valid: calls and responses are paired.
    assert_eq!(
        count_parts(&history, "model", "functionCall"),
        count_parts(&history, "user", "functionResponse")
    );
}

#[test]
fn identical_repeated_calls_are_stopped() {
    let rig = rig(vec![]);
    let script = (0..10)
        .map(|_| calls(vec![call("list_files", json!({}))]))
        .collect();
    let mock = MockGemini::start(script);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(30),
        &mut history,
        "loop",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    match r.end {
        TurnEnd::Failed(m) => assert!(m.contains("repeating"), "{m}"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(mock.received().len(), 5);
    // The model was warned before the hard stop.
    let warned = mock.received()[3]
        .body
        .to_string()
        .contains("Change your approach");
    assert!(warned);
}

#[test]
fn empty_answers_are_requested_again() {
    let rig = rig(vec![]);
    let empty = || Canned::ok(json!({"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL"}]}));
    let mock = MockGemini::start(vec![empty(), empty(), text("Finally.")]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "hi",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Finished);
    assert_eq!(r.final_text, "Finally.");

    let rig2 = self::rig(vec![]);
    let mock2 = MockGemini::start(vec![empty(), empty(), empty(), text("never")]);
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock2),
        &cfg(5),
        &mut history,
        "hi",
        &rig2.ctx,
        &emit,
        &rig2.cancel,
    );
    assert!(matches!(r.end, TurnEnd::Failed(m) if m.contains("empty answer")));
}

#[test]
fn sandbox_errors_are_reported_not_fatal() {
    // No scripted results: exec fails, like Docker being unavailable.
    let rig = rig(vec![]);
    let mock = MockGemini::start(vec![
        calls(vec![call("run_command", json!({"command": "ls"}))]),
        text("The sandbox is unavailable."),
    ]);
    let (_e, emit) = collect();
    let mut history = Vec::new();
    let r = run_turn(
        &client(&mock),
        &cfg(5),
        &mut history,
        "run it",
        &rig.ctx,
        &emit,
        &rig.cancel,
    );
    assert_eq!(r.end, TurnEnd::Finished);
    let fr = &mock.received()[1].body["contents"][2]["parts"][0]["functionResponse"]["response"];
    assert!(fr["error"].as_str().unwrap().starts_with("Error:"));
}

#[test]
fn a_task_stops_when_its_token_budget_is_used_up() {
    let rig = rig(vec![]);
    // Every scripted answer reports 110 tokens.
    let mock = MockGemini::start(vec![
        calls(vec![call("list_files", json!({}))]),
        calls(vec![call("list_files", json!({"path": "."}))]),
        calls(vec![call("list_files", json!({"depth": 1}))]),
        text("never reached"),
    ]);
    let mut config = cfg(20);
    config.token_budget = 200;
    let mut history = Vec::new();
    let result = run_turn(
        &client(&mock),
        &config,
        &mut history,
        "list things",
        &rig.ctx,
        &|_| {},
        &rig.cancel,
    );
    assert_eq!(result.end, TurnEnd::TokenLimit, "{:?}", result.end);
    assert_eq!(result.steps, 2, "stopped before the third request");
    assert_eq!(result.usage.total_tokens, 220);
    assert_eq!(mock.received().len(), 2);
}

#[test]
fn the_size_of_a_request_does_not_grow_with_the_length_of_the_task() {
    // 60 steps, each writes a 8 000 character file. Without compaction the last
    // request would carry about 500 000 characters.
    let rig = rig(vec![]);
    let code = "let value = compute(1, 2, 3); // filler\n".repeat(200);
    let mut script: Vec<Canned> = (0..60)
        .map(|i| {
            calls(vec![call(
                "write_file",
                json!({"path": format!("src/m{i}.rs"), "content": code.clone()}),
            )])
        })
        .collect();
    script.push(text("All modules written."));
    let mock = MockGemini::start(script);
    let mut history = Vec::new();
    let result = run_turn(
        &client(&mock),
        &cfg(100),
        &mut history,
        "Write sixty modules",
        &rig.ctx,
        &|_| {},
        &rig.cancel,
    );
    assert_eq!(result.end, TurnEnd::Finished, "{:?}", result.end);
    let sizes: Vec<usize> = mock
        .received()
        .iter()
        .map(|r| r.body.to_string().len())
        .collect();
    assert_eq!(sizes.len(), 61);
    let biggest = *sizes.iter().max().unwrap();
    assert!(biggest < 100_000, "largest request: {biggest} characters");
    assert!(
        sizes[60] < sizes[25] + 25_000,
        "request 61 has {} characters, request 26 has {}",
        sizes[60],
        sizes[25]
    );
    // The files themselves were written in full.
    let last = std::fs::read_to_string(rig.ctx.work.root().join("src/m59.rs")).unwrap();
    assert_eq!(last.len(), code.len());
    // Everything sent in total stays far below the quadratic amount.
    let total: usize = sizes.iter().sum();
    assert!(
        total < 61 * 100_000,
        "{total} characters were sent in total"
    );
}
