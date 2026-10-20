//! Choosing the model automatically.
//!
//! Every free service has its own limits: Gemini a small number of requests a day, NVIDIA
//! credits and 40 requests a minute for the whole account, Groq very fast answers but small
//! token limits. So the best model is not always the same one. This module ranks the models
//! the user can use right now (a key is saved, the quota is not used up, the model exists,
//! the request fits), and [`AutoClient`] uses the ranking: it starts with the best one and goes
//! on to the next when a model runs out of quota or fails.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::config::{Provider, Settings};
use crate::gemini::{ApiError, ModelInfo, ModelReply};
use crate::llm::ModelClient;
use crate::team::{AgentSpec, Role, DEFAULT_AGENT_STEPS};
use crate::{modelstats, models, profiles, vision};

/// What the next request needs from a model.
#[derive(Clone, Copy, Debug, Default)]
pub struct Need {
    /// There is a picture in the conversation.
    pub vision: bool,
    /// Estimated size of the request, in tokens.
    pub prompt_tokens: u32,
    /// The work is writing code.
    pub coding: bool,
}

/// A model that can be used now, with its score.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub provider: Provider,
    pub model: String,
    pub score: f32,
}

/// Is this model available? `(provider, model)` -> usable.
pub type Usable<'a> = &'a dyn Fn(Provider, &str) -> bool;

/// Models of a family that the presets do not know yet are worth trying when the service lists
/// them: the newest Gemini models, for example.
fn worth_adding(provider: Provider, model: &str) -> bool {
    let name = models::short_name(model).to_ascii_lowercase();
    match provider {
        Provider::Gemini => {
            name.starts_with("gemini-")
                && (name.contains("pro") || name.contains("flash"))
                && !name.contains("lite")
        }
        _ => false,
    }
}

/// The models of a service that are worth trying: its presets that the service still has, the
/// newest models of its families, and the one the user chose.
fn models_of(settings: &Settings, provider: Provider) -> Vec<String> {
    let mut s = settings.clone();
    s.provider = provider;
    let mut list = s.preset_models();
    if let Some(live) = modelstats::live_models(&host_of(settings, provider)) {
        // A preset the service does not list any more is gone.
        list.retain(|m| live.iter().any(|l| l == m || l.trim_start_matches("models/") == m));
        for m in &live {
            let m = m.trim_start_matches("models/");
            if worth_adding(provider, m) && !list.iter().any(|x| x == m) {
                list.push(m.to_string());
            }
        }
    }
    let chosen = s.active_model().trim().to_string();
    if !chosen.is_empty() && !list.contains(&chosen) {
        list.insert(0, chosen);
    }
    list.retain(|m| models::traits_of(m).chat);
    list
}

/// Asks every service that has a key which models it lists, and remembers the answer. Takes a
/// few seconds at most; a service that does not answer is left out.
pub fn refresh_live_models(settings: &Settings) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut asked = 0;
    for provider in Provider::ALL {
        if !settings.provider_ready(provider) {
            continue;
        }
        asked += 1;
        let tx = tx.clone();
        let mut s = settings.clone();
        s.provider = provider;
        std::thread::spawn(move || {
            let host = crate::ratelimit::host_of(&s.api_base_url());
            if let Ok(list) = crate::llm::make_direct(&s).list_models() {
                let ids: Vec<String> = list.into_iter().map(|m| m.id).collect();
                if !ids.is_empty() {
                    modelstats::set_live_models(&host, ids);
                }
            }
            let _ = tx.send(());
        });
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    for _ in 0..asked {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if rx.recv_timeout(left).is_err() {
            break;
        }
    }
}

fn host_of(settings: &Settings, provider: Provider) -> String {
    let mut s = settings.clone();
    s.provider = provider;
    crate::ratelimit::host_of(&s.api_base_url())
}

/// `true` when the model's quota is not used up and the service knows the model.
fn available_now(settings: &Settings, provider: Provider, model: &str) -> bool {
    let host = host_of(settings, provider);
    !modelstats::is_missing(&host, model) && modelstats::blocked_for(&host, model).is_none()
}

fn score(
    settings: &Settings,
    provider: Provider,
    model: &str,
    need: &Need,
    prefer: Option<(Provider, &str)>,
) -> Option<f32> {
    let t = models::traits_of(model);
    if !t.chat || (need.vision && !t.vision) {
        return None;
    }
    if profiles::max_prompt_tokens(provider).is_some_and(|max| need.prompt_tokens > max) {
        return None;
    }
    let host = host_of(settings, provider);
    let mut s = profiles::quality(provider, model);
    if need.coding && t.coding {
        s += 0.7;
    }
    match modelstats::typical_secs(model) {
        Some(secs) => s -= (secs / 20.0).min(4.0),
        None => s += profiles::speed_prior(provider, model),
    }
    // A model that has already done a lot today has less left than one that has not.
    s -= (modelstats::used_today(&host, model) as f32 / 150.0).min(2.0);
    // A model that often sends broken tool calls wastes steps (and a team's time).
    if let Some(rate) = modelstats::slip_rate(model) {
        s -= (rate * 10.0).min(3.0);
    }
    if prefer == Some((provider, model)) {
        s += 2.5;
    }
    Some(s)
}

/// The models to try for `need`, best first. Models whose quota is used up are left out.
pub fn rank(settings: &Settings, need: &Need, prefer: Option<(Provider, &str)>) -> Vec<Candidate> {
    rank_with(settings, need, prefer, &|p, m| available_now(settings, p, m))
}

/// Like [`rank`], with the check of availability given (for tests).
pub fn rank_with(
    settings: &Settings,
    need: &Need,
    prefer: Option<(Provider, &str)>,
    usable: Usable,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for provider in Provider::ALL {
        if !settings.provider_ready(provider) {
            continue;
        }
        for model in models_of(settings, provider) {
            if !usable(provider, &model) {
                continue;
            }
            if let Some(score) = score(settings, provider, &model, need, prefer) {
                out.push(Candidate {
                    provider,
                    model,
                    score,
                });
            }
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

fn spec(index: usize, provider: Provider, model: &str, role: Role, tokens: u64) -> AgentSpec {
    AgentSpec {
        name: format!("{}{}", role.label(), index + 1),
        provider,
        model: model.to_string(),
        role,
        token_budget: tokens,
        max_steps: DEFAULT_AGENT_STEPS,
    }
}

/// A team made from what the user can use now: a strong lead, two coders from different
/// services (so that their limits are separate) and a tester that can see images.
pub fn suggest_team(settings: &Settings) -> Vec<AgentSpec> {
    suggest_team_with(settings, &|p, m| available_now(settings, p, m))
}

pub fn suggest_team_with(settings: &Settings, usable: Usable) -> Vec<AgentSpec> {
    let plain = Need {
        prompt_tokens: 3_000,
        coding: false,
        vision: false,
    };
    let coding = Need { coding: true, ..plain };
    let seeing = Need { vision: true, ..plain };

    let lead_pool = rank_with(settings, &plain, None, usable);
    let Some(lead) = lead_pool.first().cloned() else {
        return Vec::new();
    };
    let mut team = vec![spec(0, lead.provider, &lead.model, Role::Lead, 150_000)];

    // Coders: good and fast, from a service other than the previous one when there is a choice.
    let mut coder_pool = rank_with(settings, &coding, None, usable);
    let mut last_provider = lead.provider;
    for _ in 0..2 {
        coder_pool.retain(|c| !team.iter().any(|a| a.model == c.model && a.provider == c.provider));
        let pick = coder_pool
            .iter()
            .max_by(|a, b| {
                let bonus = |c: &Candidate| if c.provider != last_provider { 1.5 } else { 0.0 };
                (a.score + bonus(a)).total_cmp(&(b.score + bonus(b)))
            })
            .cloned();
        if let Some(c) = pick {
            last_provider = c.provider;
            team.push(spec(team.len(), c.provider, &c.model, Role::Coder, 300_000));
        }
    }
    // The tester looks at the result, so it must see images when any model can.
    let tester_pool = rank_with(settings, &seeing, None, usable);
    let tester = tester_pool
        .iter()
        .find(|c| !team.iter().any(|a| a.model == c.model && a.provider == c.provider))
        .or_else(|| tester_pool.first())
        .cloned()
        .or_else(|| lead_pool.get(1).cloned());
    if let Some(c) = tester {
        team.push(spec(team.len(), c.provider, &c.model, Role::Tester, 200_000));
    }
    team
}

// ---------------------------------------------------------------------------
// The client that chooses
// ---------------------------------------------------------------------------

/// How [`AutoClient`] chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The best model for every request.
    Pick,
    /// The model the user chose, and another one only when it fails.
    Failover,
}

/// A model client that picks the model itself and goes on to the next one when a model runs
/// out of quota, does not exist, refuses the key or fails.
/// A model that has not answered after this long is not waited for alone: the next model is
/// asked as well, and whichever answers first is used.
pub const HEDGE_AFTER: Duration = Duration::from_secs(45);

pub struct AutoClient {
    settings: Settings,
    mode: Mode,
    hedge_after: Duration,
    /// The model used last (or the one the user chose, at the start).
    current: Mutex<Option<(Provider, String)>>,
    notices: Mutex<Vec<String>>,
}

impl AutoClient {
    pub fn new(settings: Settings, mode: Mode, start: Option<(Provider, String)>) -> Self {
        Self {
            settings,
            mode,
            hedge_after: HEDGE_AFTER,
            current: Mutex::new(start),
            notices: Mutex::new(Vec::new()),
        }
    }

    /// How long a model is waited for before the next one is asked too.
    pub fn with_hedge_after(mut self, wait: Duration) -> Self {
        self.hedge_after = wait;
        self
    }

    fn direct(&self, provider: Provider, model: &str) -> Arc<dyn ModelClient> {
        let mut s = self.settings.clone();
        s.provider = provider;
        s.set_active_model(model.to_string());
        crate::llm::make_direct(&s)
    }

    fn notice(&self, text: String) {
        self.notices.lock().unwrap_or_else(|p| p.into_inner()).push(text);
    }
}

/// What a request needs, read from the conversation.
pub fn need_of(system: &str, contents: &[Value], tool_declarations: &Value) -> Need {
    let has_picture = contents.iter().any(|m| {
        m["parts"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| p.get("inlineData").is_some()))
    });
    let chars = vision::history_chars(contents) + system.len() + tool_declarations.to_string().len();
    Need {
        vision: has_picture,
        prompt_tokens: (chars / 4) as u32,
        coding: true,
    }
}

impl ModelClient for AutoClient {
    fn generate(
        &self,
        _model: &str,
        system: &str,
        contents: &[Value],
        tool_declarations: &Value,
    ) -> Result<ModelReply, ApiError> {
        let need = need_of(system, contents, tool_declarations);
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let prefer = current.as_ref().map(|(p, m)| (*p, m.as_str()));
        let mut order: Vec<(Provider, String)> = Vec::new();
        if self.mode == Mode::Failover {
            if let Some((p, m)) = &current {
                if available_now(&self.settings, *p, m) && self.settings.provider_ready(*p) {
                    order.push((*p, m.clone()));
                }
            }
        }
        for c in rank(&self.settings, &need, prefer) {
            if !order.iter().any(|(p, m)| *p == c.provider && *m == c.model) {
                order.push((c.provider, c.model));
            }
        }
        if order.is_empty() {
            return Err(ApiError::Http {
                status: 429,
                message: "Every AI model that is set up is out of quota or unavailable right now. Please retry in 1h, or add another AI service in Settings.".into(),
                retry_after: Some(3600),
            });
        }
        // The candidates are asked one after the other. A candidate that is slow (not failing) is not
        // waited for alone: the next one is asked too, and the first good answer wins.
        let (tx, rx) = mpsc::channel::<(usize, Result<ModelReply, ApiError>)>();
        let launch = |index: usize| {
            let (provider, model) = order[index].clone();
            let client = self.direct(provider, &model);
            let (tx, system, contents, tools) = (
                tx.clone(),
                system.to_string(),
                contents.to_vec(),
                tool_declarations.clone(),
            );
            std::thread::spawn(move || {
                let _ = tx.send((index, client.generate(&model, &system, &contents, &tools)));
            });
        };
        let mut last_error: Option<ApiError> = None;
        // A rejected key or a rate limit of the whole account is no use trying a sister model for.
        let mut skipped: Vec<Provider> = Vec::new();
        let mut started = vec![false; order.len()];
        let (mut launched, mut pending) = (0usize, 0usize);
        let mut hedged = false;
        let first_started;
        {
            launch(0);
            started[0] = true;
            launched += 1;
            pending += 1;
            first_started = 0;
        }
        let next_free = |started: &[bool], skipped: &[Provider], avoid: Option<Provider>| {
            let free = |i: &usize| !started[*i] && !skipped.contains(&order[*i].0);
            (0..order.len())
                .filter(free)
                .find(|i| Some(order[*i].0) != avoid)
                .or_else(|| (0..order.len()).find(free))
        };
        loop {
            if pending == 0 {
                match next_free(&started, &skipped, None) {
                    Some(i) if launched < 5 => {
                        launch(i);
                        started[i] = true;
                        launched += 1;
                        pending += 1;
                    }
                    _ => break,
                }
            }
            let may_hedge = !hedged && launched < 5 && next_free(&started, &skipped, None).is_some();
            let wait = if may_hedge {
                self.hedge_after
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(wait) {
                Ok((i, Ok(reply))) => {
                    let (provider, model) = order[i].clone();
                    let new = (provider, model.clone());
                    let slow_winner = hedged && i != first_started;
                    if current.as_ref() != Some(&new) || slow_winner {
                        // The old model is "gone" when its quota is used up, it does not exist or its
                        // service has no key. A hiccup (a busy server) does not make us leave it.
                        let old_gone = current.as_ref().is_some_and(|(p, m)| {
                            !self.settings.provider_ready(*p) || !available_now(&self.settings, *p, m)
                        });
                        if self.mode == Mode::Pick || current.is_none() || old_gone || slow_winner {
                            *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(new);
                        }
                        if let (true, Some((p, m))) = (old_gone, &current) {
                            self.notice(format!(
                                "{} ({}) cannot be used right now, so {} ({}) answers instead.",
                                models::short_name(m),
                                p.label(),
                                models::short_name(&model),
                                provider.label()
                            ));
                        } else if slow_winner {
                            let (sp, sm) = &order[first_started];
                            self.notice(format!(
                                "{} ({}) did not answer for {} s, so {} ({}) answered instead and stays the first choice.",
                                models::short_name(sm),
                                sp.label(),
                                self.hedge_after.as_secs(),
                                models::short_name(&model),
                                provider.label()
                            ));
                        }
                    }
                    return Ok(reply);
                }
                Ok((i, Err(e))) => {
                    pending -= 1;
                    if !e.should_try_another() {
                        return Err(e);
                    }
                    let service_wide = matches!(
                        &e,
                        ApiError::Http { status: 401 | 403 | 500 | 502 | 503 | 504, .. }
                    ) || (matches!(&e, ApiError::Http { status: 429, .. })
                        && e.quota_block_secs().is_none());
                    if service_wide {
                        skipped.push(order[i].0);
                    }
                    last_error = Some(e);
                }
                Err(RecvTimeoutError::Timeout) => {
                    hedged = true;
                    // Ask the next model too, from another service when there is one.
                    let slow_provider = order[first_started].0;
                    if let Some(i) = next_free(&started, &skipped, Some(slow_provider)) {
                        launch(i);
                        started[i] = true;
                        launched += 1;
                        pending += 1;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Err(last_error.unwrap_or_else(|| ApiError::Transport("no model answered".into())))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        crate::llm::make_direct(&self.settings).list_models()
    }

    fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn serving_model(&self) -> Option<String> {
        self.current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|(_, m)| m.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        let mut s = Settings::default();
        s.api_key = "gemini-key".into();
        s.nvidia.api_key = "nvapi-key".into();
        s.groq.api_key = "gsk_key".into();
        s.normalize();
        s
    }

    fn all(_: Provider, _: &str) -> bool {
        true
    }

    fn need() -> Need {
        Need {
            vision: false,
            prompt_tokens: 3_000,
            coding: true,
        }
    }

    #[test]
    fn the_best_model_comes_first_and_a_service_without_a_key_is_skipped() {
        let ranked = rank_with(&settings(), &need(), None, &all);
        assert!(!ranked.is_empty());
        for pair in ranked.windows(2) {
            assert!(pair[0].score >= pair[1].score);
        }
        let providers: Vec<Provider> = ranked.iter().map(|c| c.provider).collect();
        assert!(providers.contains(&Provider::Gemini));
        assert!(providers.contains(&Provider::Nvidia));
        assert!(providers.contains(&Provider::Groq));

        let mut only_groq = Settings::default();
        only_groq.groq.api_key = "gsk_key".into();
        only_groq.normalize();
        let ranked = rank_with(&only_groq, &need(), None, &all);
        assert!(ranked.iter().all(|c| c.provider == Provider::Groq));
    }

    #[test]
    fn a_picture_needs_a_model_that_sees() {
        let seeing = Need { vision: true, ..need() };
        let ranked = rank_with(&settings(), &seeing, None, &all);
        assert!(!ranked.is_empty());
        assert!(ranked.iter().all(|c| models::traits_of(&c.model).vision), "{ranked:?}");
        let plain = rank_with(&settings(), &need(), None, &all);
        assert!(plain.len() > ranked.len());
    }

    #[test]
    fn a_big_request_does_not_go_to_a_service_with_a_small_token_limit() {
        let big = Need {
            prompt_tokens: 30_000,
            ..need()
        };
        let ranked = rank_with(&settings(), &big, None, &all);
        assert!(ranked.iter().all(|c| c.provider != Provider::Groq));
        let small = rank_with(&settings(), &need(), None, &all);
        assert!(small.iter().any(|c| c.provider == Provider::Groq));
    }

    #[test]
    fn a_model_without_quota_is_left_out_and_the_next_one_leads() {
        let s = settings();
        let first = rank_with(&s, &need(), None, &all)[0].clone();
        let without_first = |p: Provider, m: &str| !(p == first.provider && m == first.model);
        let second = rank_with(&s, &need(), None, &without_first);
        assert!(second.iter().all(|c| c.model != first.model || c.provider != first.provider));
        assert!(second[0].score <= first.score);
    }

    #[test]
    fn the_model_in_use_moves_up_the_list() {
        let s = settings();
        let plain = rank_with(&s, &need(), None, &all);
        let last = plain.last().unwrap().clone();
        let ranked = rank_with(&s, &need(), Some((last.provider, last.model.as_str())), &all);
        let place = |list: &[Candidate]| {
            list.iter()
                .position(|c| c.provider == last.provider && c.model == last.model)
                .unwrap()
        };
        assert!(place(&ranked) < place(&plain), "the model in use gets a bonus");
    }

    #[test]
    fn a_team_is_made_from_what_can_be_used_now() {
        let team = suggest_team_with(&settings(), &all);
        assert!(team.len() >= 4, "{team:?}");
        assert_eq!(team[0].role, Role::Lead);
        assert_eq!(team.iter().filter(|a| a.role == Role::Coder).count(), 2);
        let tester = team.iter().find(|a| a.role == Role::Tester).unwrap();
        assert!(models::traits_of(&tester.model).vision, "{tester:?}");
        // The two coders do not use the same service when there is a choice.
        let coders: Vec<&AgentSpec> = team.iter().filter(|a| a.role == Role::Coder).collect();
        assert_ne!(coders[0].provider, coders[1].provider);
        // Names are unique, so the team is valid.
        assert!(crate::team::Team::new(team.clone()).is_ok());

        // Nothing to use: no team.
        assert!(suggest_team_with(&Settings::default(), &all).is_empty());
        // One service only: still a full team, from its different models.
        let mut one = Settings::default();
        one.nvidia.api_key = "nvapi-key".into();
        one.normalize();
        let team = suggest_team_with(&one, &all);
        assert!(team.len() >= 4 && team.iter().all(|a| a.provider == Provider::Nvidia));
    }

    #[test]
    fn what_a_request_needs_is_read_from_the_conversation() {
        let plain = vec![serde_json::json!({"role":"user","parts":[{"text":"hello"}]})];
        let n = need_of("system", &plain, &serde_json::json!([]));
        assert!(!n.vision && n.prompt_tokens < 50);
        let with_picture = vec![serde_json::json!({"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"AAAA"}}]})];
        assert!(need_of("system", &with_picture, &serde_json::json!([])).vision);
    }
}
