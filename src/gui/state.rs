//! Plain data types used by the UI.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use crate::agent::{AgentEvent, TurnEnd};
use crate::drive::{DeviceCode, DriveClient, RemoteFile};
use crate::engine::Engine;
use crate::gemini::Usage;
use crate::sandbox::{DockerStatus, ExecResult};
use crate::session::{ChatItem, Session};
use crate::shadow::{ApplyReport, BackupInfo, Change};
use crate::util::now_secs;

/// Tabs of the right-hand panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Activity,
    Changes,
    Sandbox,
    Drive,
}

/// A message shown briefly at the bottom of the window.
pub struct Banner {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// Where the session's workspace and sandbox stand.
pub enum EngineState {
    /// Not started (session loaded from disk but not opened yet).
    None,
    /// The project is being copied.
    Opening,
    Ready(Arc<Engine>),
    Failed(String),
}

/// A running agent task.
pub struct RunState {
    pub cancel: Arc<AtomicBool>,
    pub status: String,
    pub started: Instant,
    pub usage: Usage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Tool,
    Good,
    Bad,
    Output,
}

pub struct LogEntry {
    pub time: i64,
    pub kind: LogKind,
    pub text: String,
}

pub struct TermEntry {
    pub command: String,
    pub output: String,
    pub status: String,
    pub ok: bool,
}

#[derive(Default)]
pub struct Terminal {
    pub input: String,
    pub entries: Vec<TermEntry>,
    pub busy: bool,
    pub scroll: bool,
}

/// An open session plus everything the UI remembers about it.
pub struct ActiveSession {
    pub session: Session,
    pub engine: EngineState,
    pub engine_dirty: bool,
    pub run: Option<RunState>,
    pub last_usage: Option<Usage>,
    pub log: Vec<LogEntry>,
    pub input: String,
    pub attachments: Vec<String>,
    pub expanded: HashSet<u32>,
    pub scroll_to_bottom: bool,
    pub changes: Vec<Change>,
    pub changes_loading: bool,
    pub changes_dirty: bool,
    pub changes_dirty_at: Option<Instant>,
    pub changes_error: Option<String>,
    pub selected: HashSet<String>,
    pub diff_for: Option<String>,
    pub diff_text: String,
    pub backups: Vec<BackupInfo>,
    pub terminal: Terminal,
    pub focus_input: bool,
    pub confirm_discard: bool,
}

impl ActiveSession {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            engine: EngineState::None,
            engine_dirty: false,
            run: None,
            last_usage: None,
            log: Vec::new(),
            input: String::new(),
            attachments: Vec::new(),
            expanded: HashSet::new(),
            scroll_to_bottom: true,
            changes: Vec::new(),
            changes_loading: false,
            changes_dirty: false,
            changes_dirty_at: None,
            changes_error: None,
            selected: HashSet::new(),
            diff_for: None,
            diff_text: String::new(),
            backups: Vec::new(),
            terminal: Terminal::default(),
            focus_input: true,
            confirm_discard: false,
        }
    }

    fn push_log(&mut self, kind: LogKind, text: &str) {
        self.log.push(LogEntry {
            time: now_secs(),
            kind,
            text: text.to_string(),
        });
        if self.log.len() > 600 {
            self.log.drain(..100);
        }
    }

    pub fn log_info(&mut self, text: &str) {
        self.push_log(LogKind::Info, text);
    }

    pub fn log_error(&mut self, text: &str) {
        self.push_log(LogKind::Bad, text);
    }

    pub fn log_tool(&mut self, summary: &str) {
        self.push_log(LogKind::Tool, summary);
    }

    pub fn log_result(&mut self, ok: bool, summary: &str, output: &str) {
        self.push_log(
            if ok { LogKind::Good } else { LogKind::Bad },
            &format!("   {summary}"),
        );
        if summary.starts_with("exit") || summary.starts_with("timed out") {
            self.push_log(
                LogKind::Output,
                &crate::util::truncate_middle(output, 6000).0,
            );
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// The tool item with this id, for the expand/collapse state.
    pub fn tool_count(&self) -> usize {
        self.session
            .items
            .iter()
            .filter(|i| matches!(i, ChatItem::Tool { .. }))
            .count()
    }
}

/// How a finished task ended, sent back from the worker.
pub struct TurnSummary {
    pub end: TurnEnd,
    pub auto_applied: Option<ApplyReport>,
    pub usage: Usage,
}

pub struct ConnectState {
    pub code: Option<DeviceCode>,
    pub cancel: Arc<AtomicBool>,
}

pub struct DriveState {
    pub client: Option<Arc<DriveClient>>,
    pub account: Option<String>,
    pub connecting: Option<ConnectState>,
    pub cwd: String,
    pub entries: Vec<RemoteFile>,
    pub loading: bool,
    pub error: Option<String>,
    pub job: Option<String>,
    pub job_status: String,
    pub job_cancel: Arc<AtomicBool>,
    pub last_result: Option<(bool, String)>,
}

impl Default for DriveState {
    fn default() -> Self {
        Self {
            client: None,
            account: None,
            connecting: None,
            cwd: String::new(),
            entries: Vec::new(),
            loading: false,
            error: None,
            job: None,
            job_status: String::new(),
            job_cancel: Arc::new(AtomicBool::new(false)),
            last_result: None,
        }
    }
}

/// Bulk Drive operations started from the Drive tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveJobKind {
    UploadProject,
    DownloadProject,
    BackupSessions,
    RestoreSessions,
    DownloadFile(String),
}

impl DriveJobKind {
    pub fn label(&self) -> &'static str {
        match self {
            DriveJobKind::UploadProject => "Upload project",
            DriveJobKind::DownloadProject => "Download project",
            DriveJobKind::BackupSessions => "Back up sessions",
            DriveJobKind::RestoreSessions => "Restore sessions",
            DriveJobKind::DownloadFile(_) => "Download file",
        }
    }

    pub fn needs_project(&self) -> bool {
        !matches!(
            self,
            DriveJobKind::BackupSessions | DriveJobKind::RestoreSessions
        )
    }
}

/// Results from worker threads.
pub enum Msg {
    EngineReady {
        session: String,
        engine: Result<Arc<Engine>, String>,
    },
    Agent {
        session: String,
        event: AgentEvent,
    },
    TurnDone {
        session: String,
        outcome: Result<TurnSummary, String>,
        history: Vec<Value>,
    },
    Changes {
        session: String,
        changes: Result<Vec<Change>, String>,
    },
    Diff {
        session: String,
        rel: String,
        text: String,
    },
    Applied {
        session: String,
        text: String,
        ok: bool,
    },
    Docker(DockerStatus),
    SandboxLog(String),
    ImageBuilt(Result<(), String>),
    Models(Result<Vec<String>, String>),
    Terminal {
        session: String,
        command: String,
        result: Result<ExecResult, String>,
    },
    DriveCode(DeviceCode),
    DriveConnected(Result<String, String>),
    DriveAccount(Option<String>),
    DriveEntries {
        dir: String,
        result: Result<Vec<RemoteFile>, String>,
    },
    DriveProgress(String),
    DriveJob {
        label: String,
        result: Result<String, String>,
    },
    Info(String),
}
