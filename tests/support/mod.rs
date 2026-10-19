//! A scripted fake of the Gemini API for integration tests.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// One canned HTTP answer.
pub struct Canned {
    pub status: u16,
    pub body: Value,
    pub delay: Duration,
    pub headers: Vec<(String, String)>,
}

impl Canned {
    pub fn ok(body: Value) -> Self {
        Canned {
            status: 200,
            body,
            delay: Duration::ZERO,
            headers: vec![],
        }
    }

    pub fn status(status: u16, message: &str) -> Self {
        Canned {
            status,
            body: json!({"error": {"code": status, "message": message}}),
            delay: Duration::ZERO,
            headers: vec![],
        }
    }

    pub fn delayed(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// What the fake server received.
#[derive(Clone, Debug)]
pub struct Received {
    pub url: String,
    pub api_key: String,
    /// Value of the `Authorization` header (OpenAI-compatible APIs).
    pub auth: String,
    pub body: Value,
}

pub struct MockGemini {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<Received>>>,
}

impl MockGemini {
    /// Starts a server that answers requests with `script`, in order.
    /// Requests beyond the script get HTTP 500.
    pub fn start(script: Vec<Canned>) -> MockGemini {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        thread::spawn(move || {
            let mut script = script.into_iter();
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                let api_key = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("x-goog-api-key"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("authorization"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                seen.lock().unwrap().push(Received {
                    url: req.url().to_string(),
                    api_key,
                    auth,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let canned = script
                    .next()
                    .unwrap_or_else(|| Canned::status(500, "script exhausted"));
                thread::sleep(canned.delay);
                let mut resp = tiny_http::Response::from_string(canned.body.to_string())
                    .with_status_code(canned.status)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    );
                for (k, v) in canned.headers {
                    resp = resp.with_header(
                        tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap(),
                    );
                }
                let _ = req.respond(resp);
            }
        });
        MockGemini {
            base_url: format!("http://127.0.0.1:{port}/v1beta"),
            requests,
        }
    }

    pub fn received(&self) -> Vec<Received> {
        self.requests.lock().unwrap().clone()
    }
}

/// A reply in which the model calls tools.
pub fn calls(parts: Vec<Value>) -> Canned {
    Canned::ok(json!({
        "candidates": [{ "content": { "role": "model", "parts": parts }, "finishReason": "STOP" }],
        "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 10, "totalTokenCount": 110 }
    }))
}

pub fn call(name: &str, args: Value) -> Value {
    json!({ "functionCall": { "name": name, "args": args } })
}

/// A reply with only text.
pub fn text(t: &str) -> Canned {
    calls(vec![json!({ "text": t })])
}

// ---------------------------------------------------------------------------
// Docker helpers
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

/// Returns true when Docker works and the sandbox image exists (building it if needed).
/// Prints a notice and returns false otherwise, so the caller can skip the test.
pub fn docker_ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        use gagent::sandbox::{build_image, docker_binary, docker_status, DockerStatus};
        let image = gagent::config::DEFAULT_SANDBOX_IMAGE;
        match docker_status(image) {
            DockerStatus::Ready {
                image_present: true,
                ..
            } => true,
            DockerStatus::Ready {
                image_present: false,
                ..
            } => {
                eprintln!("building the sandbox image for the tests...");
                let progress: gagent::sandbox::Progress = Arc::new(|line| eprintln!("  {line}"));
                match build_image(&docker_binary(), image, &progress) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("SKIPPED: cannot build the sandbox image: {e:#}");
                        false
                    }
                }
            }
            other => {
                eprintln!("SKIPPED: {}", other.describe());
                false
            }
        }
    })
}
