//! What happened the last times a model was used: how long it took to answer, how much of the
//! day's quota went, whether the quota is used up, and whether the model exists for this
//! account. Free endpoints queue requests, give each model its own limits (per minute, per day,
//! per month) and list models that cannot be used. The picker shows it and the automatic choice
//! of a model uses it, so nobody has to find out by waiting.
//!
//! Everything is kept per server and model (`host|model`): the same model name can exist at two
//! services with two different quotas.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Reply times kept per model.
const KEEP: usize = 5;
/// From this many seconds a model is called slow.
const SLOW_SECS: f32 = 30.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// The last reply times in seconds, newest last.
    pub secs: Vec<f32>,
    /// The server said "not found" for this model.
    pub missing: bool,
    /// The quota is used up until this moment (seconds since 1970); 0 when it is not.
    #[serde(default)]
    pub blocked_until: i64,
    #[serde(default)]
    pub block_reason: String,
    /// The day (days since 1970, UTC) that `used_today` and `tokens_today` count for.
    #[serde(default)]
    pub day: i64,
    #[serde(default)]
    pub used_today: u32,
    #[serde(default)]
    pub tokens_today: u64,
}

/// How often a model sent a tool call that could not be run (unknown tool, broken JSON,
/// missing argument).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Slips {
    calls: u32,
    slips: u32,
}

/// Tool calls needed before the rate counts.
const SLIP_MIN_CALLS: u32 = 12;
/// The counts are halved above this, so the last calls weigh more than old ones.
const SLIP_WINDOW: u32 = 80;
/// From this share of broken calls a model is called error-prone.
const SLIP_BAD_RATE: f32 = 0.2;

#[derive(Default)]
struct Store {
    file: Option<PathBuf>,
    slips: HashMap<String, Slips>,
    slips_file: Option<PathBuf>,
    models: HashMap<String, Entry>,
    /// The models each server lists for this account (names change: a preset can be gone).
    live: HashMap<String, Vec<String>>,
    live_file: Option<PathBuf>,
}

fn key(host: &str, model: &str) -> String {
    format!("{host}|{model}")
}

fn model_of(key: &str) -> &str {
    key.split_once('|').map_or(key, |(_, m)| m)
}

impl Store {
    fn entries_for<'a>(&'a self, model: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.models
            .iter()
            .filter(move |(k, _)| model_of(k) == model)
            .map(|(_, e)| e)
    }

    fn record_ok(&mut self, host: &str, model: &str, secs: f32, tokens: u64, now: i64) {
        let entry = self.models.entry(key(host, model)).or_default();
        entry.missing = false;
        entry.blocked_until = 0;
        entry.secs.push(secs);
        if entry.secs.len() > KEEP {
            entry.secs.remove(0);
        }
        let today = now.div_euclid(86_400);
        if entry.day != today {
            entry.day = today;
            entry.used_today = 0;
            entry.tokens_today = 0;
        }
        entry.used_today += 1;
        entry.tokens_today += tokens;
    }

    fn record_missing(&mut self, host: &str, model: &str) {
        self.models.entry(key(host, model)).or_default().missing = true;
    }

    fn record_limited(&mut self, host: &str, model: &str, seconds: u64, reason: &str, now: i64) {
        let entry = self.models.entry(key(host, model)).or_default();
        entry.blocked_until = now + seconds as i64;
        entry.block_reason = reason.chars().take(160).collect();
    }

    /// Seconds until the quota of the model is back, when it is used up.
    fn blocked_for(&self, host: &str, model: &str, now: i64) -> Option<u64> {
        let until = self.models.get(&key(host, model))?.blocked_until;
        (until > now).then(|| (until - now) as u64)
    }

    fn is_missing(&self, host: &str, model: &str) -> bool {
        self.models.get(&key(host, model)).is_some_and(|e| e.missing)
    }

    fn used_today(&self, host: &str, model: &str, now: i64) -> u32 {
        self.models
            .get(&key(host, model))
            .filter(|e| e.day == now.div_euclid(86_400))
            .map_or(0, |e| e.used_today)
    }

    /// The middle value of the last reply times, over every server that has the model.
    fn typical_secs(&self, model: &str) -> Option<f32> {
        let mut secs: Vec<f32> = self
            .entries_for(model)
            .flat_map(|e| e.secs.iter().copied())
            .collect();
        if secs.is_empty() {
            return None;
        }
        secs.sort_by(|a, b| a.total_cmp(b));
        Some(secs[secs.len() / 2])
    }

    fn record_tool(&mut self, model: &str, slip: bool) {
        let e = self.slips.entry(model.to_string()).or_default();
        e.calls += 1;
        if slip {
            e.slips += 1;
        }
        if e.calls > SLIP_WINDOW {
            e.calls /= 2;
            e.slips /= 2;
        }
    }

    /// The share of broken tool calls, once there are enough calls to say.
    fn slip_rate(&self, model: &str) -> Option<f32> {
        let e = self.slips.get(model)?;
        (e.calls >= SLIP_MIN_CALLS).then(|| e.slips as f32 / e.calls as f32)
    }

    fn hint(&self, model: &str, now: i64) -> String {
        let mut text = self.speed_hint(model, now);
        if self.slip_rate(model).is_some_and(|r| r >= SLIP_BAD_RATE) {
            text.push_str("  error-prone tool calls");
        }
        text
    }

    fn speed_hint(&self, model: &str, now: i64) -> String {
        let entries: Vec<&Entry> = self.entries_for(model).collect();
        if !entries.is_empty() && entries.iter().all(|e| e.missing) {
            return "  not available".to_string();
        }
        if !entries.is_empty() && entries.iter().all(|e| e.blocked_until > now) {
            let left = entries.iter().map(|e| e.blocked_until - now).min().unwrap_or(0);
            return if left >= 3600 {
                format!("  out of quota ~{}h", left / 3600)
            } else {
                format!("  out of quota ~{}min", (left / 60).max(1))
            };
        }
        match self.typical_secs(model) {
            Some(s) if s >= SLOW_SECS => format!("  slow ~{s:.0}s"),
            Some(s) if s >= 10.0 => format!("  ~{s:.0}s"),
            Some(s) => format!("  ~{s:.1}s"),
            None => String::new(),
        }
    }

    fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&self.models) {
            let _ = crate::util::write_atomic(file, &bytes);
        }
    }

    fn save_slips(&self) {
        let Some(file) = &self.slips_file else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&self.slips) {
            let _ = crate::util::write_atomic(file, &bytes);
        }
    }

    fn save_live(&self) {
        let Some(file) = &self.live_file else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&self.live) {
            let _ = crate::util::write_atomic(file, &bytes);
        }
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|p| p.into_inner())
}

fn now() -> i64 {
    crate::util::now_secs()
}

/// Loads the saved numbers from `dir` and keeps saving there.
pub fn init(dir: &Path) {
    let file = dir.join("model_stats.json");
    let models = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<HashMap<String, Entry>>(&t).ok())
        .unwrap_or_default();
    let live_file = dir.join("live_models.json");
    let live = std::fs::read_to_string(&live_file)
        .ok()
        .and_then(|t| serde_json::from_str::<HashMap<String, Vec<String>>>(&t).ok())
        .unwrap_or_default();
    let slips_file = dir.join("tool_slips.json");
    let slips = std::fs::read_to_string(&slips_file)
        .ok()
        .and_then(|t| serde_json::from_str::<HashMap<String, Slips>>(&t).ok())
        .unwrap_or_default();
    let mut s = lock();
    s.slips_file = Some(slips_file);
    s.slips = slips;
    s.file = Some(file);
    s.models = models;
    s.live_file = Some(live_file);
    s.live = live;
}

/// Remembers the models `host` lists for this account.
pub fn set_live_models(host: &str, models: Vec<String>) {
    let mut s = lock();
    s.live.insert(host.to_string(), models);
    s.save_live();
}

/// The models `host` listed the last time, when they are known.
pub fn live_models(host: &str) -> Option<Vec<String>> {
    lock().live.get(host).cloned()
}

/// A request to `host` for `model` was answered in `secs` seconds and used `tokens` tokens.
pub fn record_ok(host: &str, model: &str, secs: f32, tokens: u64) {
    let mut s = lock();
    s.record_ok(host, model, secs, tokens, now());
    s.save();
}

pub fn record_missing(host: &str, model: &str) {
    let mut s = lock();
    s.record_missing(host, model);
    s.save();
}

/// The quota of `model` at `host` is used up for `seconds`.
pub fn record_limited(host: &str, model: &str, seconds: u64, reason: &str) {
    let mut s = lock();
    s.record_limited(host, model, seconds, reason, now());
    s.save();
}

/// Seconds until the quota is back, when it is used up.
pub fn blocked_for(host: &str, model: &str) -> Option<u64> {
    lock().blocked_for(host, model, now())
}

pub fn is_missing(host: &str, model: &str) -> bool {
    lock().is_missing(host, model)
}

/// Requests made to this model today (counted since the last UTC midnight).
pub fn used_today(host: &str, model: &str) -> u32 {
    lock().used_today(host, model, now())
}

/// The usual reply time of a model, when it has been used.
pub fn typical_secs(model: &str) -> Option<f32> {
    lock().typical_secs(model)
}

/// `model` made a tool call; `slip` is true when the call itself was malformed.
pub fn record_tool_call(model: &str, slip: bool) {
    let mut s = lock();
    s.record_tool(model, slip);
    let calls = s.slips.get(model).map_or(0, |e| e.calls);
    if slip || calls % 10 == 0 {
        s.save_slips();
    }
}

/// The share of tool calls of `model` that were malformed, once there are enough calls.
pub fn slip_rate(model: &str) -> Option<f32> {
    lock().slip_rate(model)
}

/// A short note for menus (`  ~6.0s`, `  slow ~110s`, `  out of quota ~5h`, `  not available`), or empty.
pub fn hint(model: &str) -> String {
    lock().hint(model, now())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_middle_reply_time_decides() {
        let mut s = Store::default();
        assert_eq!(s.hint("m", NOW), "");
        for secs in [0.8, 0.9, 40.0] {
            s.record_ok("h", "m", secs, 10, NOW);
        }
        // One slow request among fast ones does not make a model slow.
        assert_eq!(s.hint("m", NOW), "  ~0.9s");
        for secs in [60.0, 70.0, 80.0, 90.0] {
            s.record_ok("h", "m", secs, 10, NOW);
        }
        assert!(s.hint("m", NOW).starts_with("  slow ~"), "{}", s.hint("m", NOW));
        assert_eq!(s.models["h|m"].secs.len(), KEEP);
    }

    #[test]
    fn a_model_with_many_broken_tool_calls_is_called_error_prone() {
        let mut s = Store::default();
        for _ in 0..8 {
            s.record_tool("m", true);
        }
        // Too few calls to say.
        assert_eq!(s.slip_rate("m"), None);
        for _ in 0..8 {
            s.record_tool("m", false);
        }
        assert_eq!(s.slip_rate("m"), Some(0.5));
        assert!(s.hint("m", NOW).contains("error-prone"), "{}", s.hint("m", NOW));
        // Old calls fade: a model that got better is not punished for ever.
        for _ in 0..200 {
            s.record_tool("m", false);
        }
        assert!(s.slip_rate("m").unwrap() < 0.1);
        assert!(!s.hint("m", NOW).contains("error-prone"));
    }

    #[test]
    fn a_missing_model_is_marked_until_it_answers_again() {
        let mut s = Store::default();
        s.record_missing("h", "gone");
        assert_eq!(s.hint("gone", NOW), "  not available");
        assert!(s.is_missing("h", "gone"));
        s.record_ok("h", "gone", 3.0, 5, NOW);
        assert_eq!(s.hint("gone", NOW), "  ~3.0s");
    }

    #[test]
    fn a_used_up_quota_blocks_the_model_until_it_passes() {
        let mut s = Store::default();
        s.record_ok("h", "m", 2.0, 5, NOW);
        s.record_limited("h", "m", 5 * 3600, "daily quota", NOW);
        assert_eq!(s.blocked_for("h", "m", NOW + 60), Some(5 * 3600 - 60));
        assert_eq!(s.hint("m", NOW + 60), "  out of quota ~4h");
        assert_eq!(s.blocked_for("h", "m", NOW + 6 * 3600), None);
        assert_eq!(s.hint("m", NOW + 6 * 3600), "  ~2.0s");
        // The same model name at another service is not blocked.
        assert_eq!(s.blocked_for("other", "m", NOW + 60), None);
        // A request that works again clears the block.
        s.record_limited("h", "m", 3600, "x", NOW);
        s.record_ok("h", "m", 1.0, 5, NOW + 10);
        assert_eq!(s.blocked_for("h", "m", NOW + 20), None);
    }

    #[test]
    fn requests_are_counted_per_day() {
        let mut s = Store::default();
        for _ in 0..3 {
            s.record_ok("h", "m", 1.0, 100, NOW);
        }
        assert_eq!(s.used_today("h", "m", NOW), 3);
        assert_eq!(s.models["h|m"].tokens_today, 300);
        assert_eq!(s.used_today("h", "m", NOW + 86_400), 0, "a new day starts at zero");
        s.record_ok("h", "m", 1.0, 100, NOW + 86_400);
        assert_eq!(s.used_today("h", "m", NOW + 86_400), 1);
    }

    #[test]
    fn the_numbers_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store {
            file: Some(dir.path().join("model_stats.json")),
            ..Store::default()
        };
        s.record_ok("h", "kept", 12.0, 7, NOW);
        s.record_limited("h", "kept", 600, "minute quota", NOW);
        s.save();
        let text = std::fs::read_to_string(dir.path().join("model_stats.json")).unwrap();
        let back: HashMap<String, Entry> = serde_json::from_str(&text).unwrap();
        assert_eq!(back["h|kept"].secs, vec![12.0]);
        assert_eq!(back["h|kept"].blocked_until, NOW + 600);
    }
}
