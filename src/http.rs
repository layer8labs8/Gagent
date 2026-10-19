//! A small blocking HTTP layer shared by the Gemini and Google Drive clients.
//!
//! Uses the operating system's certificate store (so corporate proxies work)
//! and honours the standard `HTTPS_PROXY` / `NO_PROXY` environment variables.

use std::time::Duration;

use serde_json::Value;
use ureq::config::Config;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::Agent;

/// A completed HTTP exchange. Non-2xx statuses are returned, not raised.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Value of the `Retry-After` header in seconds, when present.
    pub retry_after: Option<u64>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("invalid JSON in response: {e}"))
    }
}

/// The request could not be completed (DNS, TLS, connection, timeout).
#[derive(Debug, Clone)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

#[derive(Clone)]
pub struct Http {
    agent: Agent,
    max_body: u64,
}

impl Http {
    /// Creates a client. `timeout` limits a whole request, including the body.
    pub fn new(timeout: Duration, max_body: u64) -> Self {
        let tls = TlsConfig::builder().root_certs(root_certs()).build();
        let config = Config::builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .timeout_connect(Some(Duration::from_secs(20)))
            .user_agent(format!("Gagent/{}", crate::VERSION))
            .tls_config(tls)
            .build();
        Self {
            agent: Agent::new_with_config(config),
            max_body,
        }
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, TransportError> {
        self.send("GET", url, headers, None, &[])
    }

    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<Response, TransportError> {
        let bytes = serde_json::to_vec(body).map_err(|e| TransportError(e.to_string()))?;
        self.send(
            "POST",
            url,
            headers,
            Some("application/json; charset=utf-8"),
            &bytes,
        )
    }

    pub fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> Result<Response, TransportError> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(
            "POST",
            url,
            &[],
            Some("application/x-www-form-urlencoded"),
            body.as_bytes(),
        )
    }

    /// Sends any request with a raw body.
    pub fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Response, TransportError> {
        let mut builder = ureq::http::Request::builder().method(method).uri(url);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        if let Some(ct) = content_type {
            builder = builder.header("Content-Type", ct);
        }
        let request = builder
            .body(body.to_vec())
            .map_err(|e| TransportError(format!("invalid request: {e}")))?;
        let mut response = self.agent.run(request).map_err(describe_error)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = response
            .body_mut()
            .with_config()
            .limit(self.max_body)
            .read_to_vec()
            .map_err(describe_error)?;
        Ok(Response {
            status,
            body,
            retry_after,
        })
    }
}

/// The operating system's certificate store, or the bundled Mozilla roots on a
/// Linux system that has no certificate store at all (minimal containers, WSL
/// images without `ca-certificates`).
fn root_certs() -> RootCerts {
    #[cfg(target_os = "linux")]
    {
        let has_store = [
            "/etc/ssl/certs",
            "/etc/pki/tls/certs",
            "/etc/ssl/cert.pem",
            "/etc/pki/ca-trust",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
            || std::env::var_os("SSL_CERT_FILE").is_some()
            || std::env::var_os("SSL_CERT_DIR").is_some();
        if !has_store {
            return RootCerts::WebPki;
        }
    }
    RootCerts::PlatformVerifier
}

fn describe_error(e: ureq::Error) -> TransportError {
    use ureq::Error::*;
    let text = match &e {
        Timeout(t) => format!("the request timed out ({t})"),
        HostNotFound => {
            "the server name could not be resolved (no internet connection?)".to_string()
        }
        ConnectionFailed => "could not connect to the server".to_string(),
        Io(io) => format!("network error: {io}"),
        other => format!("network error: {other}"),
    };
    TransportError(text)
}

/// Percent-encodes a string for use in a URL query or form body.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(url_encode("a b&c=d/\u{e9}"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(url_encode("safe-_.~09AZaz"), "safe-_.~09AZaz");
    }

    #[test]
    fn talks_to_a_local_server() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut req = server.recv().unwrap();
            let mut body = String::new();
            std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
            let reply = format!(
                "{{\"method\":\"{}\",\"body\":{}}}",
                req.method(),
                serde_json::to_string(&body).unwrap()
            );
            let resp = tiny_http::Response::from_string(reply)
                .with_status_code(429)
                .with_header(
                    tiny_http::Header::from_bytes(&b"Retry-After"[..], &b"7"[..]).unwrap(),
                );
            req.respond(resp).unwrap();
        });
        let http = Http::new(Duration::from_secs(10), 1 << 20);
        let r = http
            .post_json(
                &format!("http://127.0.0.1:{port}/x"),
                &[("X-Test", "1")],
                &serde_json::json!({"a": 1}),
            )
            .unwrap();
        handle.join().unwrap();
        assert_eq!(r.status, 429);
        assert_eq!(r.retry_after, Some(7));
        assert!(!r.is_success());
        let v = r.json().unwrap();
        assert_eq!(v["method"], "POST");
        assert_eq!(v["body"], "{\"a\":1}");
    }

    #[test]
    fn reports_connection_errors() {
        let http = Http::new(Duration::from_secs(5), 1 << 20);
        // Port 1 on localhost is essentially never open.
        let err = http.get("http://127.0.0.1:1/", &[]).unwrap_err();
        assert!(!err.0.is_empty());
    }
}
