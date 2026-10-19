//! The native desktop application (egui/eframe).
//!
//! The UI thread only draws and reacts to input. Everything slow (copying the
//! project, calling Gemini, running Docker, talking to the AI) happens on worker
//! threads that report back through a channel of [`Msg`] values.

mod markdown;
mod panels;
mod state;
mod theme;
mod views;
mod worker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::ThemePreference;

use crate::config::{AppDirs, Settings};
use crate::permission::Approvals;
use crate::sandbox::DockerStatus;
use crate::session::{Session, SessionMeta, SessionStore};

pub use state::*;

/// Starts the desktop app and blocks until the window is closed.
/// Maps the setting to egui's theme choice.
fn preference(appearance: crate::config::Appearance) -> ThemePreference {
    match appearance {
        crate::config::Appearance::Auto => ThemePreference::System,
        crate::config::Appearance::Light => ThemePreference::Light,
        crate::config::Appearance::Dark => ThemePreference::Dark,
    }
}

pub fn run(dirs: AppDirs) -> eframe::Result {
    crate::engine::install_crash_log(&dirs);
    // Held until the window closes; see `claim_instance`.
    let _instance = crate::engine::claim_instance(&dirs);
    let appearance = Settings::load(&dirs).appearance;
    let icon = egui::IconData {
        rgba: theme::icon_rgba(128),
        width: 128,
        height: 128,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::APP_NAME)
            .with_inner_size([1320.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        crate::APP_NAME,
        options,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, preference(appearance));
            Ok(Box::new(GagentApp::new(dirs, cc.egui_ctx.clone())))
        }),
    )
}

/// Sends messages to the UI thread and wakes it up.
#[derive(Clone)]
pub struct Notifier {
    tx: mpsc::Sender<Msg>,
    ctx: egui::Context,
}

impl Notifier {
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

pub struct GagentApp {
    pub(crate) dirs: AppDirs,
    pub(crate) settings: Settings,
    /// Settings being edited; `Some` while the settings window is open.
    pub(crate) draft: Option<Settings>,
    pub(crate) store: SessionStore,
    pub(crate) sessions: Vec<SessionMeta>,
    pub(crate) open: HashMap<String, ActiveSession>,
    pub(crate) current: Option<String>,
    pub(crate) notify: Notifier,
    rx: mpsc::Receiver<Msg>,
    pub(crate) tab: Tab,
    pub(crate) right_open: bool,
    pub(crate) docker: Option<DockerStatus>,
    pub(crate) docker_checking: bool,
    pub(crate) building_image: bool,
    pub(crate) sandbox_log: Vec<String>,
    pub(crate) models: Vec<String>,
    pub(crate) models_loading: bool,
    pub(crate) key_status: Option<(bool, String)>,
    pub(crate) show_key: bool,
    pub(crate) banner: Option<Banner>,
    pub(crate) new_project_path: String,
    pub(crate) confirm_delete: Option<String>,
    started_engines: HashSet<String>,
    /// The questions of the agent ("may I install this?") wait here for the user.
    pub(crate) approvals: Arc<Approvals>,
}

impl GagentApp {
    pub fn new(dirs: AppDirs, ctx: egui::Context) -> Self {
        let _ = dirs.ensure();
        let settings = Settings::load(&dirs);
        let store = SessionStore::new(&dirs.sessions_dir());
        let (tx, rx) = mpsc::channel();
        let approvals = Arc::new(Approvals::new(settings.permission));
        {
            let ctx = ctx.clone();
            approvals.set_wake(move || {
                ctx.request_repaint();
                // Blinks the taskbar button when the window is not in front.
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            });
        }
        let mut app = GagentApp {
            sessions: store.list(),
            store,
            draft: None,
            open: HashMap::new(),
            current: None,
            notify: Notifier { tx, ctx },
            rx,
            tab: Tab::Changes,
            right_open: false,
            docker: None,
            docker_checking: false,
            building_image: false,
            sandbox_log: Vec::new(),
            models: settings.preset_models(),
            models_loading: false,
            key_status: None,
            show_key: false,
            banner: None,
            new_project_path: settings.last_project.clone(),
            confirm_delete: None,
            started_engines: HashSet::new(),
            approvals,
            settings,
            dirs,
        };
        app.check_docker();
        if app.settings.is_configured() {
            app.load_models();
        }
        app
    }

    // ----- small helpers -------------------------------------------------

    pub(crate) fn show_banner(&mut self, text: impl Into<String>, error: bool) {
        self.banner = Some(Banner {
            text: text.into(),
            error,
            until: Instant::now() + Duration::from_secs(if error { 12 } else { 5 }),
        });
    }

    pub(crate) fn active(&self) -> Option<&ActiveSession> {
        self.current.as_ref().and_then(|id| self.open.get(id))
    }

    pub(crate) fn active_mut(&mut self) -> Option<&mut ActiveSession> {
        let id = self.current.clone()?;
        self.open.get_mut(&id)
    }

    pub(crate) fn save_settings(&mut self) {
        self.settings.normalize();
        if let Err(e) = self.settings.save(&self.dirs) {
            self.show_banner(format!("Could not save settings: {e:#}"), true);
        }
    }

    pub(crate) fn project_name(path: &str) -> String {
        PathBuf::from(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| path.to_string())
    }

    // ----- sessions ------------------------------------------------------

    /// Creates a new session for a project folder and opens it.
    pub(crate) fn start_session(&mut self, project: &str) {
        let project = project.trim();
        if project.is_empty() {
            self.show_banner("Choose a project folder first.", true);
            return;
        }
        let path = PathBuf::from(project);
        if !path.is_dir() {
            self.show_banner(format!("This folder does not exist: {project}"), true);
            return;
        }
        let canonical = dunce::canonicalize(&path).unwrap_or(path);
        let project = canonical.to_string_lossy().into_owned();
        let mut session = Session::new(&project);
        let _ = self.store.save(&mut session);
        self.settings.last_project = project.clone();
        self.new_project_path = project;
        self.save_settings();
        let id = session.meta.id.clone();
        self.open.insert(id.clone(), ActiveSession::new(session));
        self.sessions = self.store.list();
        self.current = Some(id.clone());
        self.open_engine(&id);
    }

    /// Switches to a saved session, loading it from disk if needed.
    pub(crate) fn select_session(&mut self, id: &str) {
        if !self.open.contains_key(id) {
            match self.store.load(id) {
                Ok(session) => {
                    self.open
                        .insert(id.to_string(), ActiveSession::new(session));
                }
                Err(e) => {
                    self.show_banner(format!("Could not open the session: {e:#}"), true);
                    return;
                }
            }
        }
        self.current = Some(id.to_string());
        let needs_engine = matches!(
            self.open.get(id).map(|a| &a.engine),
            Some(EngineState::None)
        );
        if needs_engine {
            self.open_engine(id);
        }
        if let Some(a) = self.open.get_mut(id) {
            a.scroll_to_bottom = true;
        }
    }

    pub(crate) fn delete_session(&mut self, id: &str) {
        let engine_id = id.to_string();
        self.open.remove(id);
        if self.current.as_deref() == Some(id) {
            self.current = None;
        }
        self.store.delete(id);
        self.sessions = self.store.list();
        self.started_engines.remove(id);
        let dirs = self.dirs.clone();
        std::thread::spawn(move || worker::cleanup_session_data(&dirs, &engine_id));
    }

    pub(crate) fn save_session(&mut self, id: &str) {
        if let Some(a) = self.open.get_mut(id) {
            if let Err(e) = self.store.save(&mut a.session) {
                let text = format!("Could not save the session: {e:#}");
                self.show_banner(text, true);
            }
        }
        self.sessions = self.store.list();
    }

    // ----- background work -------------------------------------------------

    pub(crate) fn open_engine(&mut self, id: &str) {
        let Some(active) = self.open.get_mut(id) else {
            return;
        };
        active.engine = EngineState::Opening;
        let project = active.session.meta.project.clone();
        self.started_engines.insert(id.to_string());
        worker::open_engine(
            self.notify.clone(),
            self.dirs.clone(),
            self.settings.clone(),
            id.to_string(),
            project,
            self.approvals.clone(),
        );
    }

    pub(crate) fn check_docker(&mut self) {
        if self.docker_checking {
            return;
        }
        self.docker_checking = true;
        worker::check_docker(self.notify.clone(), self.settings.sandbox.clone());
    }

    pub(crate) fn load_models(&mut self) {
        if self.models_loading {
            return;
        }
        self.models_loading = true;
        worker::load_models(self.notify.clone(), self.settings.clone());
    }

    /// Sends the composer text to the agent.
    pub(crate) fn send_message(&mut self) {
        let Some(id) = self.current.clone() else {
            return;
        };
        let settings = self.settings.clone();
        let Some(active) = self.open.get_mut(&id) else {
            return;
        };
        if active.run.is_some() {
            return;
        }
        let EngineState::Ready(engine) = &active.engine else {
            return;
        };
        let typed = active.input.trim().to_string();
        if typed.is_empty() {
            return;
        }
        if !settings.is_configured() {
            self.show_banner(settings.setup_hint(), true);
            self.draft = Some(self.settings.clone());
            return;
        }
        let engine = engine.clone();

        let mut text = typed.clone();
        if !active.attachments.is_empty() {
            text = format!(
                "[Attached files in the workspace: {}]\n\n{}",
                active.attachments.join(", "),
                typed
            );
            active.attachments.clear();
        }
        active.input.clear();
        active.session.set_title_from(&typed);
        active
            .session
            .items
            .push(crate::session::ChatItem::User(typed));
        active.scroll_to_bottom = true;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        active.run = Some(RunState {
            cancel: cancel.clone(),
            status: "Starting...".into(),
            started: Instant::now(),
            usage: Default::default(),
        });
        active.log_info("Task started");
        let history = active.session.history.clone();
        self.save_session(&id);
        worker::run_turn(self.notify.clone(), id, engine, history, text, cancel);
    }

    pub(crate) fn stop_run(&mut self) {
        if let Some(run) = self.active_mut().and_then(|a| a.run.as_mut()) {
            run.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            run.status = "Stopping...".into();
        }
    }

    pub(crate) fn refresh_changes(&mut self) {
        let Some(id) = self.current.clone() else {
            return;
        };
        self.refresh_changes_for(&id);
    }

    pub(crate) fn refresh_changes_for(&mut self, id: &str) {
        let notify = self.notify.clone();
        let Some(a) = self.open.get_mut(id) else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        if a.changes_loading {
            a.changes_dirty = true;
            return;
        }
        a.changes_loading = true;
        worker::load_changes(notify, id.to_string(), engine.clone());
    }

    pub(crate) fn show_diff(&mut self, rel: &str) {
        let notify = self.notify.clone();
        let Some(a) = self.active_mut() else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        a.diff_for = Some(rel.to_string());
        a.diff_text = "Loading...".into();
        let id = a.session.meta.id.clone();
        worker::load_diff(notify, id, engine.clone(), rel.to_string());
    }

    pub(crate) fn apply_changes(&mut self, rels: Vec<String>, force: bool) {
        let notify = self.notify.clone();
        let Some(a) = self.active_mut() else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        let id = a.session.meta.id.clone();
        worker::apply(notify, id, engine.clone(), rels, force);
    }

    pub(crate) fn discard_changes(&mut self, rels: Option<Vec<String>>) {
        let notify = self.notify.clone();
        let Some(a) = self.active_mut() else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        let id = a.session.meta.id.clone();
        worker::discard(notify, id, engine.clone(), rels);
    }

    pub(crate) fn revert_backup(&mut self, dir: PathBuf) {
        let notify = self.notify.clone();
        let Some(a) = self.active_mut() else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        let id = a.session.meta.id.clone();
        worker::revert(notify, id, engine.clone(), dir);
    }

    pub(crate) fn run_in_sandbox(&mut self, command: String) {
        let notify = self.notify.clone();
        let timeout = self.settings.sandbox.command_timeout_secs;
        let Some(a) = self.active_mut() else {
            return;
        };
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        if a.terminal.busy || command.trim().is_empty() {
            return;
        }
        a.terminal.busy = true;
        a.terminal.input.clear();
        let id = a.session.meta.id.clone();
        worker::run_terminal(notify, id, engine.clone(), command, timeout);
    }

    pub(crate) fn build_image(&mut self) {
        if self.building_image {
            return;
        }
        self.building_image = true;
        self.sandbox_log.clear();
        worker::build_image(self.notify.clone(), self.settings.sandbox.image.clone());
    }

    pub(crate) fn reset_sandbox(&mut self) {
        if let Some(EngineState::Ready(engine)) = self.active().map(|a| &a.engine) {
            let engine = engine.clone();
            std::thread::spawn(move || engine.shutdown());
            self.show_banner(
                "Sandbox container removed. It starts again on the next command.",
                false,
            );
        }
    }

    /// Applies edited settings and rebuilds engines so they pick the changes up.
    pub(crate) fn commit_settings(&mut self, new: Settings) {
        let provider_changed = new.provider != self.settings.provider;
        let key_changed = provider_changed
            || new.effective_api_key() != self.settings.effective_api_key()
            || new.api_base_url() != self.settings.api_base_url();
        let appearance_changed = new.appearance != self.settings.appearance;
        let image_changed = new.sandbox.image != self.settings.sandbox.image
            || new.sandbox.backend != self.settings.sandbox.backend
            || new.sandbox.wsl_distro != self.settings.sandbox.wsl_distro;
        self.settings = new;
        self.approvals.set_mode(self.settings.permission);
        if provider_changed {
            self.models = self.settings.preset_models();
        }
        self.save_settings();
        if appearance_changed {
            theme::apply(&self.notify.ctx, preference(self.settings.appearance));
        }
        for a in self.open.values_mut() {
            a.engine_dirty = true;
        }
        self.refresh_engines_if_idle();
        if key_changed {
            self.key_status = None;
            if self.settings.is_configured() {
                self.load_models();
            }
        }
        if image_changed {
            self.check_docker();
        }
    }

    /// Re-creates engines whose settings changed, once no task is running.
    pub(crate) fn refresh_engines_if_idle(&mut self) {
        let ids: Vec<String> = self
            .open
            .iter()
            .filter(|(_, a)| {
                a.engine_dirty && a.run.is_none() && matches!(a.engine, EngineState::Ready(_))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(a) = self.open.get_mut(&id) {
                a.engine_dirty = false;
            }
            self.open_engine(&id);
        }
    }

    // ----- message handling ---------------------------------------------

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::EngineReady { session, engine } => {
                let Some(a) = self.open.get_mut(&session) else {
                    return;
                };
                match engine {
                    Ok(e) => {
                        a.engine = EngineState::Ready(e);
                        a.log_info("Workspace ready");
                        self.refresh_changes_for(&session);
                    }
                    Err(e) => {
                        a.log_error(&e);
                        a.engine = EngineState::Failed(e);
                    }
                }
            }
            Msg::Agent { session, event } => self.handle_agent_event(&session, event),
            Msg::TurnDone {
                session,
                outcome,
                history,
            } => {
                if let Some(a) = self.open.get_mut(&session) {
                    a.run = None;
                    match outcome {
                        Ok(done) => {
                            a.session.history = history;
                            use crate::agent::TurnEnd;
                            match done.end {
                                TurnEnd::Finished => a.log_info("Task finished"),
                                TurnEnd::Cancelled => {
                                    a.session
                                        .items
                                        .push(crate::session::ChatItem::Notice("Stopped.".into()));
                                    a.log_info("Task stopped by the user");
                                }
                                TurnEnd::StepLimit => {
                                    let n = self.settings.max_steps;
                                    a.session.items.push(crate::session::ChatItem::Notice(format!(
                                        "Reached the step limit ({n}). Send \"continue\" to let the agent go on."
                                    )));
                                    a.log_info("Step limit reached");
                                }
                                TurnEnd::TokenLimit => {
                                    let n = self.settings.max_task_tokens;
                                    a.session.items.push(crate::session::ChatItem::Notice(format!(
                                        "Reached the token budget ({n} tokens). Send \"continue\" to let the agent go on, or raise the budget in Settings."
                                    )));
                                    a.log_info("Token budget reached");
                                }
                                TurnEnd::Failed(msg) => {
                                    a.log_error(&msg);
                                    a.session.items.push(crate::session::ChatItem::Error(msg));
                                }
                            }
                            if let Some(report) = done.auto_applied {
                                let text = apply_summary(&report);
                                a.session.items.push(crate::session::ChatItem::Notice(text));
                            }
                            a.last_usage = Some(done.usage);
                        }
                        Err(e) => {
                            a.log_error(&e);
                            a.session.items.push(crate::session::ChatItem::Error(e));
                        }
                    }
                    a.scroll_to_bottom = true;
                }
                self.save_session(&session);
                self.refresh_changes_for(&session);
                self.refresh_engines_if_idle();
            }
            Msg::Changes { session, changes } => {
                let mut again = false;
                if let Some(a) = self.open.get_mut(&session) {
                    a.changes_loading = false;
                    match changes {
                        Ok(list) => {
                            a.selected.retain(|r| list.iter().any(|c| &c.rel == r));
                            a.changes = list;
                            a.changes_error = None;
                        }
                        Err(e) => a.changes_error = Some(e),
                    }
                    if let EngineState::Ready(engine) = &a.engine {
                        a.backups = crate::shadow::Shadow::list_backups(
                            &engine.dirs.backups_dir(&engine.session_id),
                        );
                    }
                    again = std::mem::take(&mut a.changes_dirty);
                }
                if again {
                    self.refresh_changes_for(&session);
                }
            }
            Msg::Diff { session, rel, text } => {
                if let Some(a) = self.open.get_mut(&session) {
                    if a.diff_for.as_deref() == Some(rel.as_str()) {
                        a.diff_text = text;
                    }
                }
            }
            Msg::Applied { session, text, ok } => {
                if let Some(a) = self.open.get_mut(&session) {
                    a.log_info(&text);
                    a.session
                        .items
                        .push(crate::session::ChatItem::Notice(text.clone()));
                    a.scroll_to_bottom = true;
                    a.diff_for = None;
                }
                self.show_banner(text, !ok);
                self.save_session(&session);
                self.refresh_changes_for(&session);
            }
            Msg::Docker(status) => {
                self.docker_checking = false;
                self.docker = Some(status);
            }
            Msg::SandboxLog(line) => {
                self.sandbox_log.push(line);
                if self.sandbox_log.len() > 400 {
                    self.sandbox_log.drain(..100);
                }
            }
            Msg::ImageBuilt(result) => {
                self.building_image = false;
                match result {
                    Ok(()) => self.show_banner("Sandbox image built.", false),
                    Err(e) => self.show_banner(format!("Image build failed: {e}"), true),
                }
                self.check_docker();
            }
            Msg::Models(result) => {
                self.models_loading = false;
                match result {
                    Ok(list) if !list.is_empty() => {
                        self.key_status =
                            Some((true, format!("Key works. {} models available.", list.len())));
                        self.models = list;
                    }
                    Ok(_) => {
                        self.key_status =
                            Some((true, "Key works, but no chat models were listed.".into()))
                    }
                    Err(e) => self.key_status = Some((false, e)),
                }
            }
            Msg::Terminal {
                session,
                command,
                result,
            } => {
                if let Some(a) = self.open.get_mut(&session) {
                    a.terminal.busy = false;
                    let entry = match result {
                        Ok(r) => TermEntry {
                            command,
                            output: r.output.trim_end().to_string(),
                            status: if r.cancelled {
                                "cancelled".into()
                            } else if r.timed_out {
                                "timed out".into()
                            } else {
                                format!(
                                    "exit {}",
                                    r.exit_code
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "?".into())
                                )
                            },
                            ok: r.success(),
                        },
                        Err(e) => TermEntry {
                            command,
                            output: e,
                            status: "error".into(),
                            ok: false,
                        },
                    };
                    a.terminal.entries.push(entry);
                    a.terminal.scroll = true;
                }
                self.refresh_changes_for(&session);
            }
            Msg::Info(text) => self.show_banner(text, false),
        }
    }

    fn handle_agent_event(&mut self, session: &str, event: crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        use crate::session::ChatItem;
        let Some(a) = self.open.get_mut(session) else {
            return;
        };
        match event {
            AgentEvent::Status(s) => {
                if let Some(run) = a.run.as_mut() {
                    run.status = s;
                }
            }
            AgentEvent::Text(t) => {
                a.session.items.push(ChatItem::Assistant(t));
                a.scroll_to_bottom = true;
            }
            AgentEvent::ToolStart { id, name, summary } => {
                a.log_tool(&summary);
                a.session.items.push(ChatItem::Tool {
                    id,
                    name,
                    summary,
                    ok: None,
                    result: String::new(),
                    output: String::new(),
                    millis: 0,
                });
                a.scroll_to_bottom = true;
            }
            AgentEvent::ToolEnd {
                id,
                ok,
                summary,
                output,
                millis,
            } => {
                for item in a.session.items.iter_mut().rev() {
                    if let ChatItem::Tool {
                        id: item_id,
                        ok: item_ok,
                        result,
                        output: item_output,
                        millis: item_ms,
                        ..
                    } = item
                    {
                        if *item_id == id && item_ok.is_none() {
                            *item_ok = Some(ok);
                            *result = summary.clone();
                            *item_output = output.clone();
                            *item_ms = millis;
                            break;
                        }
                    }
                }
                a.log_result(ok, &summary, &output);
            }
            AgentEvent::FilesChanged => {
                // Refreshing after every tool call would hash files constantly;
                // `tick` refreshes shortly after the activity settles.
                a.changes_dirty_at = Some(Instant::now());
            }
            AgentEvent::Usage(u) => {
                if let Some(run) = a.run.as_mut() {
                    run.usage = u;
                }
            }
        }
    }

    /// Periodic housekeeping, called every frame.
    fn tick(&mut self) {
        if let Some(b) = &self.banner {
            if Instant::now() > b.until {
                self.banner = None;
            }
        }
        let due: Vec<String> = self
            .open
            .iter()
            .filter(|(_, a)| {
                a.changes_dirty_at
                    .map(|t| t.elapsed() > Duration::from_millis(700))
                    .unwrap_or(false)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if let Some(a) = self.open.get_mut(&id) {
                a.changes_dirty_at = None;
            }
            self.refresh_changes_for(&id);
        }
        let busy = self
            .open
            .values()
            .any(|a| a.run.is_some() || a.changes_dirty_at.is_some() || a.terminal.busy)
            || self.docker_checking
            || self.building_image
            || self.models_loading
            || self.banner.is_some();
        if busy {
            self.notify
                .ctx
                .request_repaint_after(Duration::from_millis(120));
        }
    }

    /// Called when the window closes: stops every sandbox container.
    fn shutdown(&mut self) {
        let image_ids: Vec<String> = self.started_engines.iter().cloned().collect();
        let handles: Vec<_> = image_ids
            .into_iter()
            .map(|id| {
                let settings = self.settings.sandbox.clone();
                std::thread::spawn(move || worker::remove_container(&settings, &id))
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
        self.save_settings();
    }
}

/// One-line summary of an apply operation.
pub fn apply_summary(report: &crate::shadow::ApplyReport) -> String {
    let mut text = format!("Applied {} file(s) to your project.", report.applied.len());
    if !report.skipped.is_empty() {
        text.push_str(&format!(
            " Skipped {}: {}",
            report.skipped.len(),
            report
                .skipped
                .iter()
                .take(3)
                .map(|(f, why)| format!("{f} ({why})"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    text
}

impl eframe::App for GagentApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::sync(ui.ctx());
        self.drain_messages();
        self.tick();
        self.handle_dropped_files(ui.ctx());

        views::sidebar(self, ui);
        if self.current.is_some() && self.right_open {
            panels::side_panel(self, ui);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::bg()))
            .show(ui, |ui| {
                if self.current.is_some() {
                    views::chat(self, ui);
                } else {
                    views::welcome(self, ui);
                }
            });
        panels::settings_window(self, ui.ctx());
        views::banner(self, ui.ctx());
        views::approval_card(self, ui.ctx());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

impl GagentApp {
    /// Files dropped onto the window are added to the workspace as attachments.
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        self.attach_files(dropped);
    }

    pub(crate) fn attach_files(&mut self, files: Vec<PathBuf>) {
        let mut error = None;
        if let Some(a) = self.active_mut() {
            let EngineState::Ready(engine) = &a.engine else {
                error = Some("The workspace is not ready yet.".to_string());
                return self.show_banner(error.unwrap_or_default(), true);
            };
            for file in files {
                match worker::attach_file(engine, &file) {
                    Ok(rel) => a.attachments.push(rel),
                    Err(e) => error = Some(format!("Could not attach {}: {e:#}", file.display())),
                }
            }
            a.attachments.sort();
            a.attachments.dedup();
        }
        if let Some(e) = error {
            self.show_banner(e, true);
        } else {
            self.refresh_changes();
        }
    }
}
