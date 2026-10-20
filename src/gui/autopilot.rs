//! A test driver: runs a team project in the real window, with the real event loop, without
//! anyone clicking. It does what the buttons do (create the project, press Start) and writes
//! what happened to a file. Built only with `--features autopilot`; never part of the release.
//!
//! Environment: `GAGENT_AUTOPILOT_FOLDER`, `_GOAL`, `_AGENTS` (specs separated by `;`),
//! `_OUT` (result file) and `_SECONDS` (give up after this long).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;

use super::project::{AgentDraft, ProjectDraft};
use super::state::*;
use super::GagentApp;
use crate::session::ChatItem;
use crate::team::AgentSpec;

#[derive(PartialEq)]
enum Stage {
    Start,
    Created,
    Approved,
}

pub struct Autopilot {
    started: Instant,
    stage: Stage,
    folder: String,
    goal: String,
    agents: Vec<String>,
    out: PathBuf,
    limit: Duration,
    plan_ready: Option<f32>,
}

impl Autopilot {
    pub fn from_env() -> Option<Self> {
        let folder = std::env::var("GAGENT_AUTOPILOT_FOLDER").ok()?;
        Some(Self {
            started: Instant::now(),
            stage: Stage::Start,
            folder,
            goal: std::env::var("GAGENT_AUTOPILOT_GOAL").unwrap_or_default(),
            agents: std::env::var("GAGENT_AUTOPILOT_AGENTS")
                .unwrap_or_default()
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            out: PathBuf::from(std::env::var("GAGENT_AUTOPILOT_OUT").unwrap_or_else(|_| "autopilot.json".into())),
            limit: Duration::from_secs(
                std::env::var("GAGENT_AUTOPILOT_SECONDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(900),
            ),
            plan_ready: None,
        })
    }
}

fn finish(app: &GagentApp, ap: &Autopilot, verdict: &str, code: i32) -> ! {
    let mut tasks = Vec::new();
    let mut errors = Vec::new();
    let mut items = 0;
    let mut tokens = 0;
    if let Some(a) = app.active() {
        items = a.session.items.len();
        for i in &a.session.items {
            if let ChatItem::Error(e) = i {
                errors.push(e.clone());
            }
        }
        if let Some(t) = &a.team {
            tokens = t.tokens;
            if let Some(plan) = &t.plan {
                for task in &plan.tasks {
                    tasks.push(json!({
                        "id": task.id, "title": task.title, "owner": task.owner,
                        "status": task.status.label(), "summary": task.summary,
                    }));
                }
            }
        }
    }
    let result = json!({
        "verdict": verdict,
        "seconds": ap.started.elapsed().as_secs_f32(),
        "plan_ready_seconds": ap.plan_ready,
        "tokens": tokens,
        "feed_items": items,
        "errors": errors,
        "tasks": tasks,
    });
    let _ = std::fs::write(&ap.out, serde_json::to_vec_pretty(&result).unwrap_or_default());
    std::process::exit(code);
}

/// Called every frame.
pub fn step(app: &mut GagentApp) {
    let Some(mut ap) = app.autopilot.take() else {
        return;
    };
    app.notify
        .ctx
        .request_repaint_after(Duration::from_millis(200));
    if ap.started.elapsed() > ap.limit {
        finish(app, &ap, "timeout", 3);
    }
    match ap.stage {
        Stage::Start => {
            if ap.started.elapsed() > Duration::from_secs(2) {
                let mut draft = ProjectDraft::new(app);
                draft.folder = ap.folder.clone();
                draft.goal = ap.goal.clone();
                if ap.agents == ["auto"] {
                    crate::router::refresh_live_models(&app.settings);
                    let made = crate::router::suggest_team(&app.settings);
                    draft.agents = made
                        .into_iter()
                        .map(|s| AgentDraft {
                            provider: s.provider,
                            model: s.model,
                            role: s.role,
                            tokens_k: (s.token_budget / 1000) as u32,
                        })
                        .collect();
                } else if !ap.agents.is_empty() {
                    draft.agents.clear();
                    for (i, spec) in ap.agents.iter().enumerate() {
                        match AgentSpec::parse(spec, i) {
                            Ok(s) => draft.agents.push(AgentDraft {
                                provider: s.provider,
                                model: s.model,
                                role: s.role,
                                tokens_k: (s.token_budget / 1000) as u32,
                            }),
                            Err(e) => finish(app, &ap, &format!("bad agent: {e}"), 2),
                        }
                    }
                }
                match app.create_project(&draft) {
                    Ok(()) => ap.stage = Stage::Created,
                    Err(e) => finish(app, &ap, &format!("could not create the project: {e}"), 2),
                }
            }
        }
        Stage::Created => {
            if let Some(a) = app.active() {
                if let EngineState::Failed(e) = &a.engine {
                    let e = e.clone();
                    finish(app, &ap, &format!("workspace failed: {e}"), 2);
                }
                if a.team.as_ref().is_some_and(|t| t.phase == TeamPhase::Review) {
                    ap.plan_ready = Some(ap.started.elapsed().as_secs_f32());
                    app.team_decision(true);
                    ap.stage = Stage::Approved;
                } else if a.team.as_ref().is_some_and(|t| t.phase == TeamPhase::Idle)
                    && a.run.is_none()
                    && matches!(a.engine, EngineState::Ready(_))
                    && a.session.items.iter().any(|i| matches!(i, ChatItem::Error(_)))
                {
                    finish(app, &ap, "planning failed", 4);
                }
            }
        }
        Stage::Approved => {
            if let Some(a) = app.active() {
                if a.run.is_none() && a.team.as_ref().is_some_and(|t| t.phase == TeamPhase::Idle) {
                    let all_done = a
                        .team
                        .as_ref()
                        .and_then(|t| t.plan.as_ref())
                        .is_some_and(|p| p.tasks.iter().all(|t| t.status == crate::team::Status::Done));
                    finish(app, &ap, if all_done { "all tasks done" } else { "finished with problems" }, if all_done { 0 } else { 1 });
                }
            }
        }
    }
    app.autopilot = Some(ap);
}
