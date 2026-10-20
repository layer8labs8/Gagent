//! Running a team project: the lead plans, the agents do the tasks one by one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{assign_owner, AgentSpec, Plan, Status, Task, Team};
use crate::agent::{self, AgentConfig, AgentEvent, CallError, TurnEnd};
use crate::config::Settings;
use crate::engine::Engine;
use crate::llm::ModelClient;
use crate::prompt::project_overview;
use crate::util::truncate_end;
use crate::models;

/// What happens in a project, for the screen or the terminal.
pub enum TeamEvent {
    /// The lead is writing the plan.
    Planning { lead: String, model: String },
    /// The plan is ready for review.
    PlanReady(Plan),
    /// An earlier plan is continued.
    Resumed(Plan),
    TaskStart {
        id: String,
        owner: String,
        title: String,
    },
    /// Something an agent did while it worked on a task (`task` is `plan` while planning).
    Agent {
        task: String,
        owner: String,
        event: AgentEvent,
    },
    TaskEnd {
        id: String,
        status: Status,
        summary: String,
        tokens: u64,
    },
    Note(String),
}

pub struct TeamOutcome {
    pub plan: Plan,
    /// Tokens used by everybody.
    pub tokens: u64,
    /// The user stopped the project.
    pub cancelled: bool,
    /// The user did not approve the plan.
    pub declined: bool,
}

/// The client of an agent: its provider and model, with the keys of the settings.
fn client_for(settings: &Settings, spec: &AgentSpec) -> Result<Arc<dyn ModelClient>> {
    let mut s = settings.clone();
    s.provider = spec.provider;
    s.set_active_model(spec.model.clone());
    if !s.is_configured() {
        bail!("agent {}: {}", spec.name, s.setup_hint());
    }
    // The agent starts with its own model; when that one runs out of quota or fails, another
    // model of the same kind takes over (unless the user switched that off).
    if s.auto_failover || s.auto_pick {
        let mode = if s.auto_pick {
            crate::router::Mode::Pick
        } else {
            crate::router::Mode::Failover
        };
        let start = Some((spec.provider, spec.model.clone()));
        return Ok(Arc::new(crate::router::AutoClient::new(s, mode, start)));
    }
    Ok(crate::llm::make_direct(&s))
}

fn lead_prompt(team: &Team) -> String {
    let mut s = String::from(
        "You are the lead of a team of AI agents that build a software project together. You do not write code now: you write the plan.\n\
Reply with ONE JSON object and nothing else:\n\
{\"conventions\":\"...\",\"tasks\":[{\"id\":\"t1\",\"title\":\"...\",\"description\":\"...\",\"files\":[\"path\"],\"deps\":[],\"owner\":\"agent name\",\"accept\":\"how to tell it is done\"}]}\n\
Rules:\n\
- 3 to 15 tasks. Each task is small enough for one agent to finish in about 20 steps and names the files it creates or changes.\n\
- \"conventions\" is the contract between the parts: file names, element ids and class names, function names and their arguments, data formats, code style. Be specific: every agent reads it and the parts must fit together.\n\
- Put the tasks that others need first and list them in \"deps\".\n\
- End with a task that tests the result by running it, and a task that reviews it.\n\
- Give every task an \"owner\" from the team below. Big or tricky tasks go to strong, large models; small, clear tasks to fast ones.\n\
- Files that already exist are listed under \"Project now\": build on them.\n\
Team:\n",
    );
    for a in &team.agents {
        s.push_str(&format!(
            "- {} (role {}): {}\n",
            a.name,
            a.role.label(),
            crate::profiles::blurb(a.provider, &a.model)
        ));
    }
    s.push_str(
        "Give every task to the agent whose model fits it best, and spread the work, because each service has its own limits.\n",
    );
    s
}

/// Asks the lead for a plan. When the lead cannot make one (its model is blocked or out of quota,
/// or the plan is never usable), the other agents are asked in turn, so a model that does not
/// work does not stop the project. `None` when the user stopped meanwhile.
fn make_plan(
    engine: &Engine,
    goal: &str,
    team: &Team,
    tokens: &mut u64,
    emit: &dyn Fn(TeamEvent),
    cancel: &Arc<AtomicBool>,
) -> Result<Option<Plan>> {
    let first = team.lead();
    let mut order: Vec<&AgentSpec> = vec![first];
    for a in &team.agents {
        if !order.iter().any(|o| o.provider == a.provider && o.model == a.model) {
            order.push(a);
        }
    }
    let mut last = String::new();
    for (n, lead) in order.iter().enumerate() {
        match make_plan_with(engine, goal, team, lead, tokens, emit, cancel) {
            Ok(plan) => return Ok(plan),
            Err(e) => {
                last = format!("{e:#}");
                if let Some(next) = order.get(n + 1) {
                    emit(TeamEvent::Note(format!(
                        "{} could not make the plan ({}). {} tries instead.",
                        lead.name,
                        truncate_end(&last, 160),
                        next.name
                    )));
                }
            }
        }
    }
    bail!("{last}")
}

/// Asks one agent for a plan; a plan that does not hold together is sent back (twice at most).
fn make_plan_with(
    engine: &Engine,
    goal: &str,
    team: &Team,
    lead: &AgentSpec,
    tokens: &mut u64,
    emit: &dyn Fn(TeamEvent),
    cancel: &Arc<AtomicBool>,
) -> Result<Option<Plan>> {
    emit(TeamEvent::Planning {
        lead: lead.name.clone(),
        model: lead.model.clone(),
    });
    let client = client_for(&engine.settings, lead)?;
    let ctx = engine.tool_context(cancel.clone())?;
    let overview = project_overview(&ctx.work, ctx.opts);
    let cfg = AgentConfig {
        model: lead.model.clone(),
        max_steps: 1,
        token_budget: 0,
        system_prompt: lead_prompt(team),
        retry_base_delay: Duration::from_secs(2),
    };
    let mut contents = vec![json!({
        "role": "user",
        "parts": [{ "text": format!("Goal: {goal}\n\nProject now:\n{overview}") }],
    })];
    let status = |event: AgentEvent| {
        emit(TeamEvent::Agent {
            task: "plan".into(),
            owner: lead.name.clone(),
            event,
        })
    };
    let mut last_problem = String::new();
    for _ in 0..3 {
        let reply = match agent::call_model(&client, &cfg, &contents, &json!([]), &status, cancel) {
            Ok(r) => r,
            Err(CallError::Cancelled) => return Ok(None),
            Err(CallError::Api(e)) => bail!("the lead could not make a plan: {}", e.user_message()),
        };
        *tokens += reply.usage.total_tokens;
        if reply.usage.total_tokens > 0 {
            status(AgentEvent::Spent {
                company: client.company(),
                tokens: reply.usage.total_tokens,
            });
        }
        let checked = Plan::parse(&reply.text).and_then(|mut p| {
            p.goal = goal.to_string();
            p.validate(&team.agents).map(|_| p)
        });
        match checked {
            Ok(plan) => return Ok(Some(plan)),
            Err(problem) => {
                contents.push(reply.content.clone());
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": format!("The plan was rejected: {problem}. Reply with the corrected JSON object only.") }],
                }));
                last_problem = problem;
            }
        }
    }
    bail!("the lead did not make a usable plan in three tries: {last_problem}")
}

/// The part of an agent's instructions that is about the team and its task.
fn team_section(spec: &AgentSpec, plan: &Plan, task: &Task) -> String {
    let mut s = format!(
        "\n# Your team\nYou are {}, the {} of a team of AI agents that build this together: {}\n{}\n",
        spec.name,
        spec.role.label(),
        plan.goal,
        spec.role.brief()
    );
    s.push_str("The contract every agent keeps to:\n");
    if plan.conventions.trim().is_empty() {
        s.push_str("(none: choose simple names and keep them)\n");
    } else {
        s.push_str(plan.conventions.trim());
        s.push('\n');
    }
    s.push_str(&format!("\nYour task {}: {}\n", task.id, task.title));
    if !task.description.is_empty() {
        s.push_str(&task.description);
        s.push('\n');
    }
    if !task.files.is_empty() {
        s.push_str(&format!("Files: {}\n", task.files.join(", ")));
    }
    if !task.accept.trim().is_empty() {
        s.push_str(&format!("Done when: {}\n", task.accept.trim()));
    }
    let before: Vec<String> = task
        .deps
        .iter()
        .filter_map(|d| plan.tasks.iter().find(|t| &t.id == d))
        .map(|t| format!("- {} ({}): {}", t.id, t.owner, truncate_end(&t.summary, 300)))
        .collect();
    if !before.is_empty() {
        s.push_str("\nFinished before you started:\n");
        s.push_str(&before.join("\n"));
        s.push('\n');
    }
    s.push_str(
        "\nOther agents do the other tasks. Do your own task; change other files only when your task needs it, and keep to the names in the contract. \
Their work is already on disk: read it instead of guessing.\n\
Work fast and small: write the files, check them once if a check is cheap (run the code, or read it back), then stop. Do not install tools, \
do not make extra files for notes, and do not polish what already works.\n\
When you are done, answer with a short handoff for the next agent, at most 6 lines: what you made, the files, and anything it must know.\n",
    );
    s
}

/// Names of the files in the top folder of the project.
fn root_files(work: &std::path::Path) -> std::collections::HashSet<String> {
    std::fs::read_dir(work)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Some models write their handoff note into a file (`HANDOFF`, `handoff.txt`) although they
/// were told to answer with it. Such files are not part of the project, so they are removed.
fn remove_note_files(work: &std::path::Path, before: &std::collections::HashSet<String>) {
    for name in root_files(work) {
        let lower = name.to_ascii_lowercase();
        let note = lower.starts_with("handoff") || lower.starts_with("hand-off");
        if note && !before.contains(&name) && work.join(&name).is_file() {
            let _ = std::fs::remove_file(work.join(&name));
        }
    }
}

struct TaskResult {
    status: Status,
    summary: String,
    tokens: u64,
    cancelled: bool,
}

fn run_task(
    engine: &Engine,
    spec: &AgentSpec,
    plan: &Plan,
    task: &Task,
    token_left: u64,
    emit: &dyn Fn(TeamEvent),
    cancel: &Arc<AtomicBool>,
) -> Result<TaskResult> {
    let client = client_for(&engine.settings, spec)?;
    // Every agent works with its own model: whether it can see decides the tools it gets.
    let mut ctx = engine.tool_context(cancel.clone())?;
    ctx.vision = models::sees_images(&spec.model);
    let system = format!(
        "{}{}",
        engine.prompt_for(&ctx, &spec.model),
        team_section(spec, plan, task)
    );
    let cfg = AgentConfig {
        model: spec.model.clone(),
        max_steps: spec.max_steps,
        token_budget: token_left,
        system_prompt: system,
        retry_base_delay: Duration::from_secs(2),
    };
    let message = format!("Do your task now: {}. {}", task.id, task.title);
    let forward = |event: AgentEvent| {
        emit(TeamEvent::Agent {
            task: task.id.clone(),
            owner: spec.name.clone(),
            event,
        })
    };
    let mut history: Vec<Value> = Vec::new();
    let before = root_files(&engine.work_dir());
    let turn = agent::run_turn(&client, &cfg, &mut history, &message, &ctx, &forward, cancel);
    remove_note_files(&engine.work_dir(), &before);
    let note = turn.final_text.trim();
    let cancelled = matches!(turn.end, TurnEnd::Cancelled);
    let (status, summary) = match &turn.end {
        TurnEnd::Finished => (
            Status::Done,
            if note.is_empty() {
                "(no note)".to_string()
            } else {
                truncate_end(note, 600)
            },
        ),
        TurnEnd::Cancelled => (Status::Todo, "stopped".to_string()),
        TurnEnd::StepLimit => (
            Status::Failed,
            format!("ran out of steps. {}", truncate_end(note, 300)),
        ),
        TurnEnd::TokenLimit => (
            Status::Failed,
            format!("used up its token budget. {}", truncate_end(note, 300)),
        ),
        TurnEnd::Failed(msg) => (Status::Failed, truncate_end(msg, 300)),
    };
    Ok(TaskResult {
        status,
        summary,
        tokens: turn.usage.total_tokens,
        cancelled,
    })
}

/// Plans (or continues a saved plan) and does the tasks in order.
pub fn run_team(
    engine: &Engine,
    goal: &str,
    team: &Team,
    resume: bool,
    review: &dyn Fn(&Plan) -> bool,
    emit: &dyn Fn(TeamEvent),
    cancel: &Arc<AtomicBool>,
) -> Result<TeamOutcome> {
    let goal = goal.trim();
    let work = engine.work_dir();
    let saved = if resume { Plan::load(&work) } else { None };
    if goal.is_empty() && saved.is_none() {
        bail!("describe what the team should build");
    }
    let sync = engine.sync_from_project()?;
    if !sync.conflicts.is_empty() {
        emit(TeamEvent::Note(format!(
            "{} file(s) changed both in your project and in the agents' copy; the agents' version is kept: {}",
            sync.conflicts.len(),
            sync.conflicts.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        )));
    }

    let mut tokens = 0u64;
    let mut plan = match saved {
        Some(mut p) => {
            // What was interrupted or failed gets another try.
            for t in &mut p.tasks {
                if matches!(t.status, Status::Doing | Status::Failed | Status::Blocked) {
                    t.status = Status::Todo;
                }
            }
            emit(TeamEvent::Resumed(p.clone()));
            p
        }
        None => {
            let Some(p) = make_plan(engine, goal, team, &mut tokens, emit, cancel)? else {
                return Ok(TeamOutcome {
                    plan: Plan::default(),
                    tokens,
                    cancelled: true,
                    declined: false,
                });
            };
            emit(TeamEvent::PlanReady(p.clone()));
            if !review(&p) {
                return Ok(TeamOutcome {
                    plan: p,
                    tokens,
                    cancelled: false,
                    declined: true,
                });
            }
            p
        }
    };
    plan.save(&work)?;

    let mut used: HashMap<String, u64> = HashMap::new();
    let outcome = |plan: &Plan, tokens: u64, cancelled: bool| TeamOutcome {
        plan: plan.clone(),
        tokens,
        cancelled,
        declined: false,
    };
    for i in plan.order() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(outcome(&plan, tokens, true));
        }
        if plan.tasks[i].status == Status::Done {
            continue;
        }
        let task = plan.tasks[i].clone();

        let dependency_missing = task.deps.iter().any(|d| {
            plan.tasks
                .iter()
                .any(|t| &t.id == d && t.status != Status::Done)
        });
        let finish = |plan: &mut Plan, status: Status, summary: String, spent: u64| {
            plan.tasks[i].status = status;
            plan.tasks[i].summary = summary.clone();
            let _ = plan.save(&work);
            emit(TeamEvent::TaskEnd {
                id: task.id.clone(),
                status,
                summary,
                tokens: spent,
            });
        };
        if dependency_missing {
            finish(
                &mut plan,
                Status::Blocked,
                "a task it depends on is not done".into(),
                0,
            );
            continue;
        }
        let mut tried: Vec<String> = Vec::new();
        loop {
            let owner = plan.tasks[i].owner.clone();
            let Some(spec) = team.find(&owner) else {
                finish(&mut plan, Status::Failed, format!("no agent called '{owner}'"), 0);
                break;
            };
            let spent = used.get(&spec.name).copied().unwrap_or(0);
            let (status, summary, spent_now, cancelled) =
                if spec.token_budget > 0 && spent >= spec.token_budget {
                    (
                        Status::Failed,
                        format!("{} used up its token budget", spec.name),
                        0,
                        false,
                    )
                } else {
                    let token_left = if spec.token_budget == 0 {
                        0
                    } else {
                        spec.token_budget - spent
                    };
                    plan.tasks[i].status = Status::Doing;
                    let _ = plan.save(&work);
                    emit(TeamEvent::TaskStart {
                        id: task.id.clone(),
                        owner: spec.name.clone(),
                        title: task.title.clone(),
                    });
                    let r = match run_task(engine, spec, &plan, &task, token_left, emit, cancel) {
                        Ok(r) => r,
                        Err(e) => TaskResult {
                            status: Status::Failed,
                            summary: truncate_end(&format!("{e:#}"), 300),
                            tokens: 0,
                            cancelled: false,
                        },
                    };
                    (r.status, r.summary, r.tokens, r.cancelled)
                };
            tokens += spent_now;
            *used.entry(spec.name.clone()).or_default() += spent_now;
            if cancelled {
                plan.tasks[i].status = Status::Todo;
                let _ = plan.save(&work);
                return Ok(outcome(&plan, tokens, true));
            }
            if status == Status::Failed && tried.len() < 2 {
                tried.push(owner.clone());
                let rest: Vec<AgentSpec> = team
                    .agents
                    .iter()
                    .filter(|a| !tried.contains(&a.name))
                    .cloned()
                    .collect();
                if !rest.is_empty() {
                    let next = assign_owner(&task, &rest, &HashMap::new());
                    emit(TeamEvent::Note(format!(
                        "{owner} could not finish {} ({}); {next} tries it",
                        task.id,
                        truncate_end(&summary, 120)
                    )));
                    plan.tasks[i].owner = next;
                    continue;
                }
            }
            finish(&mut plan, status, summary, spent_now);
            break;
        }
    }
    Ok(outcome(&plan, tokens, false))
}
