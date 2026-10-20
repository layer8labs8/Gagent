//! The OpenAI-compatible client (NVIDIA, OpenAI, Ollama, ...) against a scripted server.

mod support;

use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use gagent::agent::{run_turn, AgentConfig, TurnEnd};
use gagent::config::{Provider, Settings};
use gagent::gemini::ApiError;
use gagent::llm::ModelClient;
use gagent::openai::OpenAiClient;
use gagent::sandbox::ScriptedSandbox;
use gagent::tools::ToolContext;
use gagent::workspace::{WalkOptions, Workspace};
use serde_json::{json, Value};
use support::*;

fn chat(message: Value, finish: &str) -> Canned {
    Canned::ok(json!({
        "id": "chatcmpl-1",
        "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
        "usage": { "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }
    }))
}

fn tool_call(id: &str, name: &str, args: Value) -> Value {
    json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
}

/// The mock serves `/v1beta/...`; OpenAI-style clients are pointed at `/v1`.
fn v1(mock: &MockGemini) -> String {
    mock.base_url.replace("/v1beta", "/v1")
}

fn client(mock: &MockGemini) -> Arc<dyn ModelClient> {
    Arc::new(OpenAiClient::new("nvapi-TEST", &v1(mock), 4096))
}

#[test]
fn agent_works_through_an_openai_compatible_server() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), "hello from the file\n").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = ToolContext {
        work: Workspace::new(dir.path()).unwrap(),
        sandbox: Arc::new(ScriptedSandbox::new(vec![])),
        opts: WalkOptions::default(),
        default_timeout_secs: 30,
        cancel: cancel.clone(),
        check: gagent::config::CheckLevel::Off,
        tools_note: std::sync::Arc::new(std::sync::Mutex::new(Some(String::new()))),
        approvals: std::sync::Arc::new(gagent::permission::Approvals::new(
            gagent::config::PermissionMode::Auto,
        )),
        reads: Default::default(),
        vision: false,
    };
    let mock = MockGemini::start(vec![
        // Reasoning models put their thoughts in `reasoning_content`; it must be ignored.
        chat(
            json!({
                "role": "assistant",
                "content": null,
                "reasoning_content": "I should read the file first.",
                "tool_calls": [tool_call("call_abc", "read_file", json!({"path": "notes.txt"}))]
            }),
            "tool_calls",
        ),
        chat(
            json!({
                "role": "assistant",
                "content": "<think>checking</think>The file says hello.",
                "tool_calls": [tool_call("call_def", "write_file", json!({"path": "out.txt", "content": "done\n"}))]
            }),
            "tool_calls",
        ),
        chat(json!({"role": "assistant", "content": "All done."}), "stop"),
    ]);
    let client = client(&mock);
    let cfg = AgentConfig {
        model: "nvidia/nemotron-3.5-super-vl-120b-a12b".into(),
        max_steps: 10,
        token_budget: 0,
        system_prompt: "You are Gagent (test).".into(),
        retry_base_delay: Duration::from_millis(1),
    };
    let mut history = Vec::new();
    let result = run_turn(
        &client,
        &cfg,
        &mut history,
        "Read notes.txt",
        &ctx,
        &|_| {},
        &cancel,
    );

    assert_eq!(result.end, TurnEnd::Finished, "{:?}", result.end);
    assert_eq!(result.final_text, "All done.");
    assert_eq!(result.steps, 3);
    assert_eq!(result.usage.total_tokens, 360);
    assert_eq!(
        fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "done\n"
    );

    let seen = mock.received();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].url, "/v1/chat/completions");
    assert_eq!(seen[0].auth, "Bearer nvapi-TEST");
    let first = &seen[0].body;
    assert_eq!(first["model"], "nvidia/nemotron-3.5-super-vl-120b-a12b");
    assert_eq!(first["max_tokens"], 4096);
    assert_eq!(first["messages"][0]["role"], "system");
    assert_eq!(first["messages"][1]["content"], "Read notes.txt");
    assert_eq!(first["tools"].as_array().unwrap().len(), 7);
    assert_eq!(first["tools"][1]["function"]["name"], "read_file");
    assert_eq!(
        first["tools"][1]["function"]["parameters"]["type"],
        "object"
    );

    // The second request carries the first tool call and its result.
    let second = seen[1].body["messages"].as_array().unwrap();
    assert_eq!(second[2]["role"], "assistant");
    assert_eq!(second[2]["tool_calls"][0]["id"], "call_abc");
    assert_eq!(second[3]["role"], "tool");
    assert_eq!(second[3]["tool_call_id"], "call_abc");
    assert!(second[3]["content"]
        .as_str()
        .unwrap()
        .contains("hello from the file"));

    // Reasoning was stripped from what is stored and sent back.
    let third = seen[2].body["messages"].as_array().unwrap();
    assert_eq!(third[4]["content"], "The file says hello.");
    assert_eq!(third[5]["tool_call_id"], "call_def");
}

#[test]
fn model_list_is_fetched_with_get_and_filtered() {
    let mock = MockGemini::start(vec![Canned::ok(json!({"object": "list", "data": [
        {"id": "meta/llama-3.3-70b-instruct"},
        {"id": "nvidia/nemotron-3.5-super-vl-120b-a12b"},
        {"id": "nvidia/nv-embedqa-e5-v5"},
    ]}))]);
    let models = client(&mock).list_models().unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "nvidia/nemotron-3.5-super-vl-120b-a12b",
            "meta/llama-3.3-70b-instruct"
        ]
    );
    let seen = mock.received();
    assert_eq!(seen[0].url, "/v1/models");
    assert_eq!(seen[0].auth, "Bearer nvapi-TEST");
}

#[test]
fn api_errors_are_readable() {
    let mock = MockGemini::start(vec![
        Canned {
            status: 401,
            body: json!({"status": 401, "title": "Unauthorized", "detail": "Authentication failed"}),
            delay: Duration::ZERO,
            headers: vec![],
        },
        Canned::status(404, "Function 'abc': Not found for account"),
        Canned::status(429, "Too Many Requests").header("Retry-After", "2"),
    ]);
    let c = client(&mock);
    let ask = |c: &Arc<dyn ModelClient>| {
        c.generate(
            "m",
            "sys",
            &[json!({"role": "user", "parts": [{"text": "hi"}]})],
            &json!([]),
        )
        .unwrap_err()
    };
    let e = ask(&c);
    assert!(
        matches!(&e, ApiError::Http { status: 401, message, .. } if message == "Authentication failed"),
        "{e:?}"
    );
    assert!(e.user_message().contains("Settings"));
    let e = ask(&c);
    assert!(matches!(&e, ApiError::Http { status: 404, .. }));
    assert!(!e.is_retryable());
    let e = ask(&c);
    assert!(e.is_retryable());
    assert!(matches!(
        e,
        ApiError::Http {
            retry_after: Some(2),
            ..
        }
    ));
}

#[test]
fn server_without_a_key_sends_no_authorization_header() {
    let mock = MockGemini::start(vec![chat(
        json!({"role": "assistant", "content": "hi"}),
        "stop",
    )]);
    let c = OpenAiClient::new("", &v1(&mock), 1024);
    let reply = c
        .generate(
            "llama",
            "sys",
            &[json!({"role": "user", "parts": [{"text": "hi"}]})],
            &json!([]),
        )
        .unwrap();
    assert_eq!(reply.text, "hi");
    let seen = mock.received();
    assert_eq!(seen[0].auth, "");
    assert!(seen[0].body.get("tools").is_none());
}

#[test]
fn settings_choose_the_provider_client() {
    let mock = MockGemini::start(vec![chat(
        json!({"role": "assistant", "content": "pong"}),
        "stop",
    )]);
    let mut s = Settings::default();
    s.provider = Provider::Openai;
    s.openai.base_url = v1(&mock);
    s.openai.model = "local-model".into();
    s.openai.api_key = "sk-local".into();
    s.normalize();
    assert!(s.is_configured());
    let client = gagent::llm::make_client(&s);
    let reply = client
        .generate(
            s.active_model(),
            "sys",
            &[json!({"role": "user", "parts": [{"text": "ping"}]})],
            &json!([]),
        )
        .unwrap();
    assert_eq!(reply.text, "pong");
    let seen = mock.received();
    assert_eq!(seen[0].auth, "Bearer sk-local");
    assert_eq!(seen[0].body["model"], "local-model");
}
