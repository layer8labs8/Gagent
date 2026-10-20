//! A test driver: runs a team project in the real window, with the real event loop, without
//! anyone clicking. It does what the buttons do (create the project, press Start) and writes
//! what happened to a file. Built only with `--features autopilot`; never part of the release.
//!
//! Environment: `GAGENT_AUTOPILOT_FOLDER`, `_GOAL`, `_AGENTS` (specs separated by `;`),
//! `_OUT` (result file) and `_SECONDS` (give up after this long). `_SHOTS` is a folder that gets a
//! picture of the window every `_SHOT_EVERY` seconds (taken by the window itself, so nothing in
//! front of it can get in), and `_OPEN=settings` opens the settings first.

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
    /// Only take pictures (no folder was given).
    Watch,
    /// The session of the unified flow is open; waiting for its workspace.
    Opened,
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
    /// Test the unified flow: open a session, type the goal and press Enter, like a person.
    unified: bool,
    /// A folder for pictures of the window.
    shots: Option<PathBuf>,
    shot_every: Duration,
    last_shot: Instant,
    shot_count: u32,
    open_settings: bool,
}

impl Autopilot {
    pub fn from_env() -> Option<Self> {
        let shots = std::env::var("GAGENT_AUTOPILOT_SHOTS").ok().map(PathBuf::from);
        let folder = match std::env::var("GAGENT_AUTOPILOT_FOLDER") {
            Ok(f) => f,
            Err(_) if shots.is_some() => String::new(),
            Err(_) => return None,
        };
        Some(Self {
            shots,
            shot_every: Duration::from_secs(
                std::env::var("GAGENT_AUTOPILOT_SHOT_EVERY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(20),
            ),
            last_shot: Instant::now(),
            shot_count: 0,
            open_settings: std::env::var("GAGENT_AUTOPILOT_OPEN").is_ok_and(|v| v == "settings"),
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
            unified: std::env::var_os("GAGENT_AUTOPILOT_UNIFIED").is_some(),
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

/// Asks the window for a picture of itself when it is time, and saves it as a PNG file.
fn take_pictures(app: &mut GagentApp, ap: &mut Autopilot) {
    let Some(dir) = ap.shots.clone() else {
        return;
    };
    if ap.last_shot.elapsed() < ap.shot_every || ap.started.elapsed() < Duration::from_secs(3) {
        return;
    }
    ap.last_shot = Instant::now();
    ap.shot_count += 1;
    let n = ap.shot_count;
    let ctx = app.notify.ctx.clone();
    ctx.request_screenshot(move |image| {
        let [w, h] = image.size;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for p in &image.pixels {
            rgba.extend_from_slice(&p.to_srgba_unmultiplied());
        }
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("shot{n:02}.png")), png(w, h, &rgba));
    });
    ctx.request_repaint();
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A PNG file of an RGBA picture, without compression (large, but simple and always right).
fn png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * 4 + 1) * height);
    for row in rgba.chunks(width * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
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
    take_pictures(app, &mut ap);
    match ap.stage {
        Stage::Watch => {}
        Stage::Start => {
            if ap.started.elapsed() > Duration::from_secs(2) && ap.folder.is_empty() {
                if ap.open_settings {
                    app.draft = Some(app.settings.clone());
                    app.settings_scroll = std::env::var("GAGENT_AUTOPILOT_SCROLL")
                        .ok()
                        .and_then(|v| v.parse().ok());
                }
                ap.stage = Stage::Watch;
                app.autopilot = Some(ap);
                return;
            } else if ap.started.elapsed() > Duration::from_secs(2) {
                if ap.open_settings {
                    app.draft = Some(app.settings.clone());
                }
                // The test folder is the driver's own: it answers the trust question with yes.
                if let Ok(path) = dunce::canonicalize(&ap.folder) {
                    app.settings.trust_folder(&path.to_string_lossy());
                }
            }
            if ap.started.elapsed() > Duration::from_secs(2) && ap.unified {
                let folder = ap.folder.clone();
                app.start_session(&folder);
                ap.stage = Stage::Opened;
            } else if ap.started.elapsed() > Duration::from_secs(2) {
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
        Stage::Opened => {
            if let Some(a) = app.active() {
                if let EngineState::Failed(e) = &a.engine {
                    let e = e.clone();
                    finish(app, &ap, &format!("workspace failed: {e}"), 2);
                }
            }
            if app.active().is_some_and(|a| matches!(a.engine, EngineState::Ready(_))) {
                if let Some(a) = app.active_mut() {
                    a.input = ap.goal.clone();
                }
                app.send_message();
                ap.stage = Stage::Created;
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
