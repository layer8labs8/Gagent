//! Background jobs. Each function starts a thread and reports back with a [`Msg`].

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::state::*;
use super::{apply_summary, Notifier};
use crate::config::{AppDirs, SandboxSettings, Settings};
use crate::drive::{self, CloudStore, DriveClient, Endpoints};
use crate::engine::Engine;
use crate::gemini::GeminiClient;
use crate::sandbox::{self, DockerSandbox, ExecRequest, Progress};
use crate::util::{human_bytes, unique_id, write_atomic};
use crate::workspace::{normalize_rel, WalkOptions};

/// Largest file accepted as an attachment.
const MAX_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;

pub fn open_engine(
    notify: Notifier,
    dirs: AppDirs,
    settings: Settings,
    id: String,
    project: String,
    cloud: Option<Arc<dyn CloudStore>>,
) {
    thread::spawn(move || {
        let n = notify.clone();
        let progress: Progress = Arc::new(move |line| n.send(Msg::SandboxLog(line.to_string())));
        let result = catch_unwind(AssertUnwindSafe(|| {
            Engine::open(&dirs, &settings, Path::new(&project), &id, progress, cloud)
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"))
        }))
        .unwrap_or_else(|_| Err("internal error while preparing the workspace".to_string()));
        notify.send(Msg::EngineReady {
            session: id,
            engine: result,
        });
    });
}

pub fn check_docker(notify: Notifier, image: String) {
    thread::spawn(move || {
        notify.send(Msg::Docker(sandbox::docker_status(&image)));
    });
}

pub fn load_models(notify: Notifier, settings: Settings) {
    thread::spawn(move || {
        let key = settings.effective_api_key();
        let result = if key.is_empty() {
            Err("No API key set.".to_string())
        } else {
            GeminiClient::new(&key, &settings.gemini_base_url())
                .list_models()
                .map(|models| models.into_iter().map(|m| m.id).collect())
                .map_err(|e| e.user_message())
        };
        notify.send(Msg::Models(result));
    });
}

pub fn run_turn(
    notify: Notifier,
    session: String,
    engine: Arc<Engine>,
    history: Vec<Value>,
    text: String,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let backup = history.clone();
        let mut history = history;
        let id = session.clone();
        let n = notify.clone();
        let emit = move |event| {
            n.send(Msg::Agent {
                session: id.clone(),
                event,
            })
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            engine.run_message(&mut history, &text, &emit, &cancel)
        }));
        let (outcome, history) = match result {
            Ok(Ok(done)) => (
                Ok(TurnSummary {
                    usage: done.turn.usage,
                    end: done.turn.end,
                    auto_applied: done.auto_applied,
                }),
                history,
            ),
            Ok(Err(e)) => (Err(format!("{e:#}")), backup),
            Err(_) => (
                Err("The agent crashed unexpectedly. Your files are safe; try again.".to_string()),
                backup,
            ),
        };
        notify.send(Msg::TurnDone {
            session,
            outcome,
            history,
        });
    });
}

pub fn load_changes(notify: Notifier, session: String, engine: Arc<Engine>) {
    thread::spawn(move || {
        let changes = engine.changes().map_err(|e| format!("{e:#}"));
        notify.send(Msg::Changes { session, changes });
    });
}

pub fn load_diff(notify: Notifier, session: String, engine: Arc<Engine>, rel: String) {
    thread::spawn(move || {
        let text = engine
            .diff(&rel)
            .unwrap_or_else(|e| format!("Could not build the diff: {e:#}"));
        notify.send(Msg::Diff { session, rel, text });
    });
}

pub fn apply(
    notify: Notifier,
    session: String,
    engine: Arc<Engine>,
    rels: Vec<String>,
    force: bool,
) {
    thread::spawn(move || {
        let (text, ok) = match engine.apply(&rels, force) {
            Ok(report) => (apply_summary(&report), report.skipped.is_empty()),
            Err(e) => (format!("Could not apply the changes: {e:#}"), false),
        };
        notify.send(Msg::Applied { session, text, ok });
    });
}

pub fn discard(notify: Notifier, session: String, engine: Arc<Engine>, rels: Option<Vec<String>>) {
    thread::spawn(move || {
        let result = match rels {
            Some(list) => engine.discard(&list).map(|_| list.len()),
            None => engine.discard_all(),
        };
        let (text, ok) = match result {
            Ok(n) => (
                format!("Discarded {n} change(s) in the agent's copy."),
                true,
            ),
            Err(e) => (format!("Could not discard: {e:#}"), false),
        };
        notify.send(Msg::Applied { session, text, ok });
    });
}

pub fn revert(notify: Notifier, session: String, engine: Arc<Engine>, dir: PathBuf) {
    thread::spawn(move || {
        let (text, ok) = match engine.revert(&dir) {
            Ok(n) => (
                format!("Restored {n} file(s) in your project from the backup."),
                true,
            ),
            Err(e) => (format!("Could not undo: {e:#}"), false),
        };
        notify.send(Msg::Applied { session, text, ok });
    });
}

pub fn run_terminal(
    notify: Notifier,
    session: String,
    engine: Arc<Engine>,
    command: String,
    timeout_secs: u32,
) {
    thread::spawn(move || {
        let req = ExecRequest {
            command: command.clone(),
            cwd: String::new(),
            timeout_secs,
        };
        let result = sandbox::Sandbox::exec(engine.docker.as_ref(), &req, &AtomicBool::new(false))
            .map_err(|e| format!("{e:#}"));
        notify.send(Msg::Terminal {
            session,
            command,
            result,
        });
    });
}

pub fn build_image(notify: Notifier, image: String) {
    thread::spawn(move || {
        let n = notify.clone();
        let progress: Progress = Arc::new(move |line| n.send(Msg::SandboxLog(line.to_string())));
        let result = sandbox::build_image(&sandbox::docker_binary(), &image, &progress)
            .map_err(|e| format!("{e:#}"));
        notify.send(Msg::ImageBuilt(result));
    });
}

/// Removes everything stored for a deleted session.
pub fn cleanup_session_data(dirs: &AppDirs, id: &str) {
    remove_container(&SandboxSettings::default(), id);
    let _ = fs::remove_dir_all(dirs.shadow_dir(id));
    let _ = fs::remove_dir_all(dirs.backups_dir(id));
}

/// Stops the sandbox container of a session.
pub fn remove_container(settings: &SandboxSettings, id: &str) {
    DockerSandbox::new(settings.clone(), Path::new("."), id, Arc::new(|_| {})).shutdown();
}

/// Copies a file into the workspace under `attachments/`. Returns its workspace path.
pub fn attach_file(engine: &Engine, source: &Path) -> Result<String> {
    let meta = fs::metadata(source).with_context(|| format!("cannot read {}", source.display()))?;
    if !meta.is_file() {
        bail!("only files can be attached");
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        bail!("the file is too large ({})", human_bytes(meta.len()));
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .context("the file has no name")?;
    let work = engine.work_dir();
    let mut rel = normalize_rel(&format!("attachments/{name}"))?;
    if work.join(&rel).exists() {
        rel = normalize_rel(&format!("attachments/{}-{name}", unique_id()))?;
    }
    let data = fs::read(source)?;
    write_atomic(&work.join(&rel), &data)?;
    Ok(rel)
}

// ----- Drive ---------------------------------------------------------------

pub fn drive_account(notify: Notifier, client: Arc<DriveClient>) {
    thread::spawn(move || notify.send(Msg::DriveAccount(client.account_name())));
}

pub fn drive_list(notify: Notifier, client: Arc<DriveClient>, dir: String) {
    thread::spawn(move || {
        let result = client.list(&dir).map_err(|e| format!("{e:#}"));
        notify.send(Msg::DriveEntries { dir, result });
    });
}

pub fn drive_connect(
    notify: Notifier,
    settings: crate::config::DriveSettings,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let endpoints = Endpoints::default();
        let result = (|| -> Result<String> {
            let code = drive::start_device_flow(&endpoints, &settings.client_id)?;
            notify.send(Msg::DriveCode(code.clone()));
            let tokens = drive::wait_for_approval(
                &endpoints,
                &settings.client_id,
                &settings.client_secret,
                &code,
                &cancel,
            )?;
            Ok(tokens.refresh_token)
        })();
        notify.send(Msg::DriveConnected(result.map_err(|e| format!("{e:#}"))));
    });
}

#[allow(clippy::too_many_arguments)]
pub fn drive_job(
    notify: Notifier,
    client: Arc<DriveClient>,
    kind: DriveJobKind,
    project: String,
    work_dir: Option<PathBuf>,
    sessions_dir: PathBuf,
    opts: WalkOptions,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let label = kind.label().to_string();
        let n = notify.clone();
        let progress = move |line: &str| n.send(Msg::DriveProgress(line.to_string()));
        let result = catch_unwind(AssertUnwindSafe(|| {
            run_drive_job(
                &client,
                &kind,
                &project,
                work_dir.as_deref(),
                &sessions_dir,
                opts,
                &progress,
                &cancel,
            )
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error")));
        notify.send(Msg::DriveJob {
            label,
            result: result.map_err(|e| format!("{e:#}")),
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn run_drive_job(
    client: &DriveClient,
    kind: &DriveJobKind,
    project: &str,
    work_dir: Option<&Path>,
    sessions_dir: &Path,
    opts: WalkOptions,
    progress: &dyn Fn(&str),
    cancel: &AtomicBool,
) -> Result<String> {
    let name = super::GagentApp::project_name(project);
    let remote = format!("projects/{name}");
    match kind {
        DriveJobKind::UploadProject => {
            let (up, same) =
                client.upload_tree(Path::new(project), &remote, opts, progress, cancel)?;
            Ok(format!(
                "{up} file(s) uploaded, {same} already up to date (Drive: {remote})"
            ))
        }
        DriveJobKind::DownloadProject => {
            let work = work_dir.context("the workspace is not ready")?;
            let n = client.download_tree(&remote, work, progress, cancel)?;
            if n == 0 {
                bail!("nothing found in Drive under {remote}");
            }
            Ok(format!(
                "{n} file(s) downloaded into the agent's copy. Review them in Changes, then apply."
            ))
        }
        DriveJobKind::BackupSessions => {
            let mut count = 0;
            for entry in fs::read_dir(sessions_dir)?.flatten() {
                let file_name = entry.file_name().to_string_lossy().into_owned();
                if !file_name.ends_with(".json") {
                    continue;
                }
                progress(&format!("Uploading {file_name}"));
                client.upload(&format!("sessions/{file_name}"), &fs::read(entry.path())?)?;
                count += 1;
            }
            Ok(format!("{count} session file(s) saved to Drive"))
        }
        DriveJobKind::RestoreSessions => {
            let mut count = 0;
            for file in client.list("sessions")? {
                if file.is_dir || !file.name.ends_with(".json") || file.name.contains(['/', '\\']) {
                    continue;
                }
                let target = sessions_dir.join(&file.name);
                if target.exists() {
                    continue;
                }
                progress(&format!("Downloading {}", file.name));
                write_atomic(&target, &client.download(&file.path)?)?;
                count += 1;
            }
            Ok(format!("{count} session(s) restored from Drive"))
        }
        DriveJobKind::DownloadFile(path) => {
            let work = work_dir.context("the workspace is not ready")?;
            let data = client.download(path)?;
            let rel = normalize_rel(&format!("drive/{path}"))?;
            write_atomic(&work.join(&rel), &data)?;
            Ok(format!(
                "saved as {rel} in the workspace ({})",
                human_bytes(data.len() as u64)
            ))
        }
    }
}
