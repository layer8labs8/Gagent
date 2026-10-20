//! The client that goes on with another model when one runs out of quota, against two scripted
//! servers: a Gemini one and an NVIDIA (OpenAI-style) one.

mod support;

use std::sync::Mutex;

use gagent::config::{Provider, Settings};
use gagent::llm::make_client;
use gagent::modelstats;
use serde_json::{json, Value};
use support::*;

/// The tests share environment variables (the fake server addresses).
static SERIAL: Mutex<()> = Mutex::new(());

fn nvidia_chat(text: &str) -> Canned {
    Canned::ok(json!({
        "id": "x",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    }))
}

fn v1(mock: &MockGemini) -> String {
    mock.base_url.replace("/v1beta", "/v1")
}

fn host(mock: &MockGemini) -> String {
    gagent::ratelimit::host_of(&mock.base_url)
}

fn contents() -> Vec<Value> {
    vec![json!({"role":"user","parts":[{"text":"hi"}]})]
}

fn settings(gemini: &MockGemini, nvidia: &MockGemini) -> Settings {
    std::env::set_var("GAGENT_GEMINI_BASE_URL", &gemini.base_url);
    std::env::set_var("GAGENT_NVIDIA_BASE_URL", v1(nvidia));
    let mut s = Settings::default();
    // These tests are about going on with another model when the chosen one fails.
    s.auto_pick = false;
    s.provider = Provider::Gemini;
    s.api_key = "GEMINI-KEY".into();
    s.model = "gemini-2.5-pro".into();
    s.nvidia.api_key = "nvapi-KEY".into();
    s.normalize();
    // The mock Gemini has this one model; its siblings are not asked for.
    modelstats::set_live_models(
        &gagent::ratelimit::host_of(&gemini.base_url),
        vec!["gemini-2.5-pro".to_string()],
    );
    s
}

fn reply_text(r: &gagent::gemini::ModelReply) -> String {
    r.text.clone()
}

#[test]
fn a_model_that_ran_out_of_quota_is_left_and_the_next_service_answers() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let gemini = MockGemini::start(vec![Canned::status(
        429,
        "Quota exceeded for metric GenerateRequestsPerDayPerProjectPerModel-FreeTier. Please retry in 5h2.1s.",
    )]);
    let nvidia = MockGemini::start(vec![nvidia_chat("hello from NVIDIA"), nvidia_chat("and again")]);
    let client = make_client(&settings(&gemini, &nvidia));

    let first = client
        .generate("gemini-2.5-pro", "system", &contents(), &json!([]))
        .unwrap();
    assert_eq!(reply_text(&first), "hello from NVIDIA");
    assert_eq!(gemini.received().len(), 1);
    assert_eq!(nvidia.received().len(), 1);
    let model_sent = nvidia.received()[0].body["model"].as_str().unwrap().to_string();
    assert!(
        gagent::config::NVIDIA_PRESET_MODELS.contains(&model_sent.as_str()),
        "{model_sent}"
    );
    // The user is told what happened.
    let notes = client.take_notices();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("gemini-2.5-pro") && notes[0].contains("cannot be used"), "{notes:?}");
    // The quota is remembered for hours.
    let left = modelstats::blocked_for(&host(&gemini), "gemini-2.5-pro").unwrap();
    assert!(left > 4 * 3600, "{left}");

    // The next request does not knock on the closed door again.
    let second = client
        .generate("gemini-2.5-pro", "system", &contents(), &json!([]))
        .unwrap();
    assert_eq!(reply_text(&second), "and again");
    assert_eq!(gemini.received().len(), 1);
    assert!(client.take_notices().is_empty());
}

#[test]
fn a_busy_server_is_skipped_once_but_the_chosen_model_stays_the_first_choice() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let gemini = MockGemini::start(vec![Canned::status(503, "overloaded"), text("from Gemini")]);
    let nvidia = MockGemini::start(vec![nvidia_chat("from NVIDIA")]);
    let client = make_client(&settings(&gemini, &nvidia));

    let first = client.generate("gemini-2.5-pro", "s", &contents(), &json!([])).unwrap();
    assert_eq!(reply_text(&first), "from NVIDIA");
    // It was a hiccup, not a used-up quota: Gemini is asked first again.
    let second = client.generate("gemini-2.5-pro", "s", &contents(), &json!([])).unwrap();
    assert_eq!(reply_text(&second), "from Gemini");
    assert_eq!(gemini.received().len(), 2);
    assert_eq!(nvidia.received().len(), 1);
    assert!(client.take_notices().is_empty());
}

#[test]
fn a_model_that_is_slow_to_answer_is_not_waited_for_alone() {
    use gagent::router::{AutoClient, Mode};
    use std::time::{Duration, Instant};
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let gemini = MockGemini::start(vec![text("slow Gemini").delayed(Duration::from_secs(3))]);
    let nvidia = MockGemini::start(vec![nvidia_chat("quick NVIDIA"), nvidia_chat("quick again")]);
    let s = settings(&gemini, &nvidia);
    let client = AutoClient::new(
        s,
        Mode::Failover,
        Some((Provider::Gemini, "gemini-2.5-pro".to_string())),
    )
    .with_hedge_after(Duration::from_millis(300));
    let started = Instant::now();
    let reply = gagent::llm::ModelClient::generate(&client, "gemini-2.5-pro", "s", &contents(), &json!([])).unwrap();
    assert_eq!(reply_text(&reply), "quick NVIDIA");
    assert!(started.elapsed() < Duration::from_millis(2500), "{:?}", started.elapsed());
    let notes = gagent::llm::ModelClient::take_notices(&client);
    assert!(notes.iter().any(|n| n.contains("did not answer")), "{notes:?}");
    assert_eq!(gemini.received().len(), 1);
    // The slow model is not the first choice any more.
    let again = gagent::llm::ModelClient::generate(&client, "gemini-2.5-pro", "s", &contents(), &json!([])).unwrap();
    assert_eq!(reply_text(&again), "quick again");
    assert_eq!(gemini.received().len(), 1);
}

#[test]
fn a_rejected_key_does_not_waste_the_other_models_of_the_same_service() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let gemini = MockGemini::start(vec![Canned::status(403, "API key not valid")]);
    let nvidia = MockGemini::start(vec![nvidia_chat("from NVIDIA")]);
    let client = make_client(&settings(&gemini, &nvidia));
    let reply = client.generate("gemini-2.5-pro", "s", &contents(), &json!([])).unwrap();
    assert_eq!(reply_text(&reply), "from NVIDIA");
    assert_eq!(gemini.received().len(), 1, "one try at the service with the bad key");
}

#[test]
fn when_nothing_can_be_used_the_error_says_so_and_is_not_retried() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = Settings::default();
    s.normalize();
    let client = make_client(&s);
    let err = client
        .generate("gemini-2.5-flash", "s", &contents(), &json!([]))
        .unwrap_err();
    assert!(!err.is_retryable(), "{err}");
    assert!(err.user_message().to_lowercase().contains("out of quota"), "{err}");
}
