//! A request limit shared by everything that talks to the same server.
//!
//! NVIDIA's free endpoint allows about 40 requests a minute per account, whichever model is
//! used. Staying a little below that avoids answers of "too many requests" and the waiting
//! they cause. The limit is kept per server address, so every session and every agent that
//! uses the same key shares one budget.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const MINUTE: Duration = Duration::from_secs(60);
/// Longest pause between two looks at the clock while waiting.
const SLICE: Duration = Duration::from_millis(200);
/// A request never waits longer than this for its turn.
const MAX_WAIT: Duration = Duration::from_secs(120);

/// At most `per_window` requests in any `window`.
pub struct Limiter {
    per_window: u32,
    window: Duration,
    stamps: Mutex<VecDeque<Instant>>,
}

impl Limiter {
    pub fn new(per_minute: u32) -> Self {
        Self::with_window(per_minute, MINUTE)
    }

    pub fn with_window(per_window: u32, window: Duration) -> Self {
        Self {
            per_window: per_window.max(1),
            window,
            stamps: Mutex::new(VecDeque::new()),
        }
    }

    pub fn per_window(&self) -> u32 {
        self.per_window
    }

    /// `None` when a request may go now (and is counted), otherwise how long to wait.
    fn try_take(&self) -> Option<Duration> {
        let mut stamps = self.stamps.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        while let Some(&oldest) = stamps.front() {
            if now.duration_since(oldest) >= self.window {
                stamps.pop_front();
            } else {
                break;
            }
        }
        if (stamps.len() as u32) < self.per_window {
            stamps.push_back(now);
            None
        } else {
            let oldest = stamps.front().copied().unwrap_or(now);
            Some(self.window.saturating_sub(now.duration_since(oldest)))
        }
    }

    /// Blocks until a request may go. Returns how long it waited.
    pub fn wait_turn(&self) -> Duration {
        let started = Instant::now();
        loop {
            match self.try_take() {
                None => return started.elapsed(),
                Some(_) if started.elapsed() >= MAX_WAIT => return started.elapsed(),
                Some(wait) => thread::sleep(wait.clamp(Duration::from_millis(5), SLICE)),
            }
        }
    }
}

fn registry() -> &'static Mutex<HashMap<String, Arc<Limiter>>> {
    static REG: OnceLock<Mutex<HashMap<String, Arc<Limiter>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `https://integrate.api.nvidia.com/v1/chat` -> `integrate.api.nvidia.com`.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Sets the limit for a server. `0` removes it.
pub fn configure(host: &str, per_minute: u32) {
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    if per_minute == 0 {
        reg.remove(host);
        return;
    }
    // Same limit: keep the limiter, so the requests already counted still count.
    if reg.get(host).is_some_and(|l| l.per_window() == per_minute) {
        return;
    }
    reg.insert(host.to_string(), Arc::new(Limiter::new(per_minute)));
}

/// Waits for this request's turn when its server has a limit. Returns how long it waited.
pub fn wait_turn_for_url(url: &str) -> Duration {
    let limiter = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&host_of(url))
        .cloned();
    match limiter {
        Some(l) => l.wait_turn(),
        None => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_window_makes_the_next_request_wait() {
        let limiter = Limiter::with_window(3, Duration::from_millis(400));
        let started = Instant::now();
        for _ in 0..3 {
            assert!(limiter.wait_turn() < Duration::from_millis(100));
        }
        assert!(started.elapsed() < Duration::from_millis(200));
        let waited = limiter.wait_turn();
        assert!(
            waited >= Duration::from_millis(250) && waited < Duration::from_secs(2),
            "{waited:?}"
        );
    }

    #[test]
    fn limits_are_per_server_and_can_be_removed() {
        configure("limit-test-a.example", 2);
        for _ in 0..2 {
            assert!(wait_turn_for_url("https://limit-test-a.example/v1/chat") < Duration::from_millis(100));
        }
        // Another server is not limited.
        let started = Instant::now();
        for _ in 0..20 {
            wait_turn_for_url("https://limit-test-b.example/v1/chat");
        }
        assert!(started.elapsed() < Duration::from_millis(200));
        // Removing the limit lets everything through at once.
        configure("limit-test-a.example", 0);
        let started = Instant::now();
        for _ in 0..20 {
            wait_turn_for_url("https://limit-test-a.example/v1/chat");
        }
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn the_host_is_taken_from_the_address() {
        assert_eq!(host_of("https://integrate.api.nvidia.com/v1/chat/completions"), "integrate.api.nvidia.com");
        assert_eq!(host_of("http://127.0.0.1:8080/x?y=1"), "127.0.0.1:8080");
        assert_eq!(host_of("https://User@Example.COM/a"), "example.com");
        assert_eq!(host_of("localhost"), "localhost");
    }
}
