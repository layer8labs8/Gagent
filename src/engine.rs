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
use crate::drive::CloudStore;
use crate::gemini::GeminiClient;
use crate::prompt::{system_prompt, PromptInfo};
use crate::sandbox::{docker_status, DockerSandbox, Progress, Sandbox};
use crate::shadow::{ApplyReport, Change, Shadow, SyncReport};
use crate::tools::ToolContext;
use crate::workspace::{WalkOptions, Workspace};

/// Everything needed to work on one project in one session.
pub struct Engine {
    pub dirs: AppDirs,
    pub settings: Settings,
    pub session_id: String,
    pub shadow: Arc<Mutex<Shadow>>,
    pub docker: Arc<DockerSandbox>,
    pub cloud: Option<Arc<dyn CloudStore>>,
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
        cloud: Option<Arc<dyn CloudStore>>,
    ) -> Result<Engine> {
        let opts = walk_options(settings);
        let shadow = Shadow::open(project, &dirs.shadow_dir(session_id), opts)?;
        let docker = Arc::new(DockerSandbox::new(
            settings.sandbox.clone(),
            shadow.work_dir(),
            session_id,
            progress,
        ));
        Ok(Engine {
            dirs: dirs.clone(),
            settings: settings.clone(),
            session_id: session_id.to_string(),
            shadow: Arc::new(Mutex::new(shadow)),
            docker,
            cloud,
        })
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
        let sandbox: Arc<dyn Sandbox> = self.docker.clone();
        Ok(ToolContext {
            work,
            sandbox,
            cloud: self.cloud.clone(),
            opts: self.walk_options(),
            default_timeout_secs: self.settings.sandbox.command_timeout_secs,
            cancel,
        })
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
        let key = self.settings.effective_api_key();
        if key.is_empty() {
            bail!("No Gemini API key is set. Open Settings and paste your key (get one at https://aistudio.google.com/apikey).");
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
        let sandbox_ready = docker_status(&self.settings.sandbox.image).is_ready();
        let prompt = system_prompt(&PromptInfo {
            workspace: &ctx.work,
            opts: ctx.opts,
            drive: ctx.cloud.is_some(),
            network: self.settings.sandbox.network,
            sandbox_available: sandbox_ready,
        });
        let cfg = AgentConfig {
            model: self.settings.model.clone(),
            max_steps: self.settings.max_steps,
            system_prompt: prompt,
            retry_base_delay: Duration::from_secs(2),
        };
        let client = Arc::new(GeminiClient::new(&key, &self.settings.gemini_base_url()));
        let turn = agent::run_turn(&client, &cfg, history, text, &ctx, emit, cancel);

        let mut auto_applied = None;
        if self.settings.auto_apply && matches!(turn.end, TurnEnd::Finished | TurnEnd::StepLimit) {
            let report = self.apply_all()?;
            if !report.applied.is_empty() || !report.skipped.is_empty() {
                auto_applied = Some(report);
            }
        }
        Ok(MessageOutcome { turn, auto_applied })
    }

    /// Stops the sandbox container.
    pub fn shutdown(&self) {
        self.docker.shutdown();
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
