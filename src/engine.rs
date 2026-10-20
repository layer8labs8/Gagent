//! Ties the pieces together for one session: shadow workspace, sandbox,
//! optional Drive storage and the agent loop. Used by the GUI and the CLI.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::Value;

use crate::agent::{self, AgentConfig, AgentEvent, TurnEnd, TurnResult};
use crate::config::{AppDirs, Settings};
use crate::prompt::{system_prompt, PromptInfo};
use crate::host::HostSandbox;
use crate::permission::Approvals;
use crate::sandbox::{
    docker_status, resolve_backend, Backend, DockerSandbox, Progress, Sandbox,
};
use crate::shadow::{ApplyReport, Change, Shadow, SyncReport};
use crate::tools::ToolContext;
use crate::workspace::{WalkOptions, Workspace};
use crate::wsl::WslSandbox;

/// Everything needed to work on one project in one session.
pub struct Engine {
    pub dirs: AppDirs,
    pub settings: Settings,
    pub session_id: String,
    pub shadow: Arc<Mutex<Shadow>>,
    /// Runs the agent's commands: a Docker container or WSL Ubuntu.
    pub sandbox: Arc<dyn Sandbox>,
    pub backend: Backend,
    /// What the sandbox has installed, learned by the first command of the session.
    pub tools_note: Arc<Mutex<Option<String>>>,
    /// Questions for the user ("may I install this?"). The app shares one between sessions.
    pub approvals: Arc<Approvals>,
}

/// What happened after a message was processed.
pub struct MessageOutcome {
    pub turn: TurnResult,
    /// Present when changes were applied automatically.
    pub auto_applied: Option<ApplyReport>,
}

impl Engine {
    /// Opens (or creates) the private copy of `project` for a session.
    pub fn open(
        dirs: &AppDirs,
        settings: &Settings,
        project: &Path,
        session_id: &str,
        progress: Progress,
    ) -> Result<Engine> {
        let opts = walk_options(settings);
        let shadow = Shadow::open(project, &dirs.shadow_dir(session_id), opts)?;
        let backend = resolve_backend(&settings.sandbox);
        let sandbox: Arc<dyn Sandbox> = match &backend {
            Backend::Docker => Arc::new(DockerSandbox::new(
                settings.sandbox.clone(),
                shadow.work_dir(),
                session_id,
                progress,
            )),
            Backend::Wsl(distro) => Arc::new(WslSandbox::new(distro, shadow.work_dir())),
            Backend::Host(shell) => Arc::new(HostSandbox::new(shell, shadow.work_dir())),
        };
        Ok(Engine {
            dirs: dirs.clone(),
            settings: settings.clone(),
            session_id: session_id.to_string(),
            shadow: Arc::new(Mutex::new(shadow)),
            sandbox,
            backend,
            tools_note: Default::default(),
            approvals: Arc::new(Approvals::new(settings.permission)),
        })
    }

    /// Uses `approvals` (shared with the app, which shows the questions) from now on.
    pub fn set_approvals(&mut self, approvals: Arc<Approvals>) {
        self.approvals = approvals;
    }

    pub fn walk_options(&self) -> WalkOptions {
        walk_options(&self.settings)
    }

    fn lock_shadow(&self) -> std::sync::MutexGuard<'_, Shadow> {
        self.shadow.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn work_dir(&self) -> std::path::PathBuf {
        self.lock_shadow().work_dir().to_path_buf()
    }

    pub fn project_dir(&self) -> std::path::PathBuf {
        self.lock_shadow().project_dir().to_path_buf()
    }

    pub fn changes(&self) -> Result<Vec<Change>> {
        self.lock_shadow().changes()
    }

    pub fn diff(&self, rel: &str) -> Result<String> {
        self.lock_shadow().diff(rel)
    }

    pub fn sync_from_project(&self) -> Result<SyncReport> {
        self.lock_shadow().sync_from_project()
    }

    pub fn apply(&self, rels: &[String], force: bool) -> Result<ApplyReport> {
        let backups = self.dirs.backups_dir(&self.session_id);
        self.lock_shadow().apply(rels, &backups, force)
    }

    pub fn apply_all(&self) -> Result<ApplyReport> {
        let rels: Vec<String> = self.changes()?.into_iter().map(|c| c.rel).collect();
        self.apply(&rels, false)
    }

    pub fn discard(&self, rels: &[String]) -> Result<()> {
        self.lock_shadow().discard(rels)
    }

    pub fn discard_all(&self) -> Result<usize> {
        self.lock_shadow().discard_all()
    }

    pub fn revert(&self, backup: &Path) -> Result<usize> {
        self.lock_shadow().revert(backup)
    }

    pub fn tool_context(&self, cancel: Arc<AtomicBool>) -> Result<ToolContext> {
        let work = Workspace::new(self.work_dir())?;
        Ok(ToolContext {
            work,
            sandbox: self.sandbox.clone(),
            opts: self.walk_options(),
            default_timeout_secs: self.settings.sandbox.command_timeout_secs,
            cancel,
            check: self.settings.auto_check,
            tools_note: self.tools_note.clone(),
            approvals: self.approvals.clone(),
            reads: Default::default(),
            // With an automatic choice the model that sees is picked once a picture is in the chat.
            vision: self.settings.auto_pick || crate::models::sees_images(self.settings.active_model()),
        })
    }

    /// The system prompt for a model that works with `ctx`.
    pub fn prompt_for(&self, ctx: &ToolContext, model: &str) -> String {
        let (sandbox_ready, wsl, host) = match &self.backend {
            Backend::Docker => (
                docker_status(&self.settings.sandbox.image).is_ready(),
                false,
                false,
            ),
            Backend::Wsl(distro) => (crate::wsl::wsl_status(distro).is_ok(), true, false),
            Backend::Host(shell) => (shell.is_file(), false, true),
        };
        let tools = self
            .tools_note
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_default();
        system_prompt(&PromptInfo {
            model,
            workspace: &ctx.work,
            opts: ctx.opts,
            network: self.settings.sandbox.network,
            sandbox_available: sandbox_ready,
            wsl,
            host,
            vision: ctx.vision,
            tools: &tools,
        })
    }

    /// Runs a team project: the lead plans, then the agents do the tasks one by one.
    /// Blocks until the project ends; run it on a worker thread.
    #[allow(clippy::too_many_arguments)]
    pub fn run_team(
        &self,
        goal: &str,
        team: &crate::team::Team,
        resume: bool,
        review: &dyn Fn(&crate::team::Plan) -> bool,
        emit: &dyn Fn(crate::team::TeamEvent),
        cancel: &Arc<AtomicBool>,
    ) -> Result<(crate::team::TeamOutcome, Option<ApplyReport>)> {
        let outcome = crate::team::run_team(self, goal, team, resume, review, emit, cancel)?;
        let mut auto_applied = None;
        if self.settings.auto_apply && !outcome.cancelled && !outcome.declined {
            let report = self.apply_all()?;
            if !report.applied.is_empty() || !report.skipped.is_empty() {
                auto_applied = Some(report);
            }
        }
        Ok((outcome, auto_applied))
    }

    /// Sends a user message through the agent and returns how it ended.
    ///
    /// Blocks until the agent is done; run it on a worker thread.
    pub fn run_message(
        &self,
        history: &mut Vec<Value>,
        text: &str,
        emit: &dyn Fn(AgentEvent),
        cancel: &Arc<AtomicBool>,
    ) -> Result<MessageOutcome> {
        if !self.settings.is_configured() {
            bail!("{}", self.settings.setup_hint());
        }

        emit(AgentEvent::Status("Syncing project...".into()));
        let sync = self.sync_from_project()?;
        if !sync.conflicts.is_empty() {
            emit(AgentEvent::Text(format!(
                "Note: {} file(s) changed both in your project and in the agent's copy; the agent's version is kept: {}",
                sync.conflicts.len(),
                sync.conflicts.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            )));
        }

        let ctx = self.tool_context(cancel.clone())?;
        let prompt = self.prompt_for(&ctx, self.settings.active_model());
        let cfg = AgentConfig {
            model: self.settings.active_model().to_string(),
            max_steps: self.settings.max_steps,
            token_budget: self.settings.max_task_tokens,
            system_prompt: prompt,
            retry_base_delay: Duration::from_secs(2),
        };
        let client = crate::llm::make_client(&self.settings);
        let turn = agent::run_turn(&client, &cfg, history, text, &ctx, emit, cancel);

        let mut auto_applied = None;
        if self.settings.auto_apply
            && matches!(
                turn.end,
                TurnEnd::Finished | TurnEnd::StepLimit | TurnEnd::TokenLimit
            )
        {
            let report = self.apply_all()?;
            if !report.applied.is_empty() || !report.skipped.is_empty() {
                auto_applied = Some(report);
            }
        }
        Ok(MessageOutcome { turn, auto_applied })
    }

    /// Stops the sandbox container.
    pub fn shutdown(&self) {
        let _ = self.sandbox.reset();
    }
}

/// Marks this process as the one Gagent instance that owns the sandbox containers.
///
/// The first instance holds an exclusive lock on a file in the data folder for
/// as long as it runs. Because it is the only owner, it can safely remove
/// containers left behind by a previous crash. Further instances get `None`
/// and leave the containers alone. Keep the returned file alive.
pub fn claim_instance(dirs: &AppDirs) -> Option<std::fs::File> {
    dirs.ensure().ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dirs.lock_file())
        .ok()?;
    file.try_lock().ok()?;
    std::thread::spawn(|| {
        let _ = crate::sandbox::remove_all_gagent_containers();
    });
    Some(file)
}

/// Writes panics to `logs/crash.log` so problems are visible even when the
/// app has no console window.
pub fn install_crash_log(dirs: &AppDirs) {
    let path = dirs.logs_dir().join("crash.log");
    let _ = std::fs::create_dir_all(dirs.logs_dir());
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(
                f,
                "[{}] Gagent {} panic: {info}\n{}\n",
                crate::util::now_secs(),
                crate::VERSION,
                std::backtrace::Backtrace::force_capture()
            );
        }
        previous(info);
    }));
}

/// File walking options derived from the settings.
pub fn walk_options(settings: &Settings) -> WalkOptions {
    WalkOptions {
        include_secrets: settings.share_secret_files,
        ..Default::default()
    }
}
