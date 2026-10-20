//! Plain data types used by the UI.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::{AgentEvent, TurnEnd};
use crate::engine::Engine;
use crate::gemini::Usage;
use crate::sandbox::{DockerStatus, ExecResult};
use crate::session::{ChatItem, Session};
use crate::shadow::{ApplyReport, BackupInfo, Change};
use crate::team::{AgentSpec, Plan, Status, TeamConfig, TeamEvent};
use crate::util::now_secs;

/// Where a team project stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TeamPhase {
    /// Nothing is running.
    Idle,
    /// The lead is writing the plan.
    Planning,
    /// The plan waits for the user's yes.
    Review,
    /// The agents are doing the tasks.
    Running,
}

/// The user's answer to "start this plan?", waited for by the thread that runs the project.
#[derive(Default)]
pub struct Gate {
    decision: Mutex<Option<bool>>,
    changed: Condvar,
}

impl Gate {
    pub fn set(&self, yes: bool) {
        *self.decision.lock().unwrap_or_else(|p| p.into_inner()) = Some(yes);
        self.changed.notify_all();
    }

    /// Waits for the answer; `false` when the project was stopped meanwhile.
    pub fn wait(&self, cancel: &AtomicBool) -> bool {
        let mut decision = self.decision.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(yes) = *decision {
                return yes;
            }
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            decision = match self.changed.wait_timeout(decision, Duration::from_millis(150)) {
                Ok((guard, _)) => guard,
                Err(p) => p.into_inner().0,
            };
        }
    }
}

/// What the window remembers about a team project.
pub struct TeamUi {
    pub config: TeamConfig,
    pub phase: TeamPhase,
    pub plan: Option<Plan>,
    pub gate: Arc<Gate>,
    /// Tokens used by the whole team in this window.
    pub tokens: u64,
    /// Added to the tool ids of the current task, so that ids of different tasks never clash.
    pub tool_base: u32,
    /// Start planning as soon as the workspace is ready (a new project).
    pub start_when_ready: bool,
}

impl TeamUi {
    pub fn new(config: TeamConfig) -> Self {
        Self {
            config,
            phase: TeamPhase::Idle,
            plan: None,
            gate: Arc::new(Gate::default()),
            tokens: 0,
            tool_base: 0,
            start_when_ready: false,
        }
    }

    pub fn view(&self) -> TeamView {
        let current_model = if self.phase == TeamPhase::Planning {
            self.config.team().ok().map(|t| t.lead().model.clone())
        } else {
            self.plan
                .as_ref()
                .and_then(|p| p.tasks.iter().find(|t| t.status == Status::Doing))
                .and_then(|t| self.config.agents.iter().find(|a| a.name == t.owner))
                .map(|a| a.model.clone())
        };
        TeamView {
            current_model,
            goal: self.config.goal.clone(),
            agents: self.config.agents.clone(),
            phase: self.phase,
            plan: self.plan.clone(),
            tokens: self.tokens,
        }
    }
}

/// A copy of what the board shows.
#[derive(Clone)]
pub struct TeamView {
    pub goal: String,
    pub agents: Vec<AgentSpec>,
    pub phase: TeamPhase,
    pub plan: Option<Plan>,
    pub tokens: u64,
    /// The model that is answering now (the lead while planning, else the agent doing a task).
    pub current_model: Option<String>,
}

/// How a team project ended, sent back from the worker.
pub struct TeamSummary {
    pub plan: Plan,
    pub tokens: u64,
    pub cancelled: bool,
    pub declined: bool,
    pub auto_applied: Option<ApplyReport>,
}

/// Tabs of the right-hand panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Changes,
    Sandbox,
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
    /// The last time the agent did anything (a tool, an answer). A long silence means the
    /// model is slow or queued.
    pub last_event: Instant,
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
    /// Set for a team project.
    pub team: Option<TeamUi>,
}

impl ActiveSession {
    pub fn new(session: Session) -> Self {
        let team = session.meta.team.clone().map(TeamUi::new);
        Self {
            team,
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
    Info(String),
    Team {
        session: String,
        event: TeamEvent,
    },
    TeamDone {
        session: String,
        outcome: Result<TeamSummary, String>,
    },
}
