//! A team project against a scripted Gemini server: the lead plans, three agents do the tasks.

mod support;

use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gagent::config::{AppDirs, PermissionMode, SandboxBackend, Settings};
use gagent::engine::Engine;
use gagent::team::{AgentSpec, Plan, Status, Team, TeamEvent};
use serde_json::json;
use support::*;

/// The tests share one environment variable (the fake Gemini address).
static SERIAL: Mutex<()> = Mutex::new(());

const PLAN: &str = r##"{"conventions":"page: index.html with #list and #add; styles in style.css","tasks":[
 {"id":"t1","title":"Write the page","description":"HTML with #list and #add.","files":["index.html"],"owner":"coder2","accept":"index.html exists"},
 {"id":"t2","title":"Write the styles","files":["style.css"],"deps":["t1"],"owner":"coder2"},
 {"id":"t3","title":"Test the result","deps":["t1","t2"],"owner":"tester3"}
]}"##;

fn team() -> Team {
    // All three use Gemini, so that one fake server can answer for them.
    Team::new(vec![
        AgentSpec::parse("gemini:lead-model:lead", 0).unwrap(),
        AgentSpec::parse("gemini:coder-model", 1).unwrap(),
        AgentSpec::parse("gemini:tester-model:tester", 2).unwrap(),
    ])
    .unwrap()
}

struct Rig {
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
    engine: Engine,
}

fn rig(mock: &MockGemini) -> Rig {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("README.md"), "# demo\n").unwrap();
    std::env::set_var("GAGENT_GEMINI_BASE_URL", &mock.base_url);
    let mut settings = Settings::default();
    settings.api_key = "TEAM-KEY".into();
    settings.sandbox.backend = SandboxBackend::Host;
    settings.permission = PermissionMode::All;
    settings.auto_apply = false;
    let engine = Engine::open(
        &AppDirs::at(home.path()),
        &settings,
        project.path(),
        "team-session",
        Arc::new(|_| {}),
    )
    .unwrap();
    Rig {
        _home: home,
        project,
        engine,
    }
}

fn run(
    rig: &Rig,
    team: &Team,
    resume: bool,
    approve: bool,
) -> (gagent::team::TeamOutcome, Vec<String>) {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = log.clone();
    let emit = move |e: TeamEvent| {
        let line = match e {
            TeamEvent::Planning { lead, .. } => format!("planning {lead}"),
            TeamEvent::PlanReady(_) => "plan ready".to_string(),
            TeamEvent::Resumed(_) => "resumed".to_string(),
            TeamEvent::TaskStart { id, owner, .. } => format!("start {id} {owner}"),
            TeamEvent::TaskEnd { id, status, .. } => format!("end {id} {}", status.label()),
            TeamEvent::Note(text) => format!("note {text}"),
            TeamEvent::Agent { .. } => return,
        };
        sink.lock().unwrap().push(line);
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let (outcome, _) = rig
        .engine
        .run_team(
            "a notes page",
            team,
            resume,
            &move |_plan: &Plan| approve,
            &emit,
            &cancel,
        )
        .unwrap();
    let events = log.lock().unwrap().clone();
    (outcome, events)
}

#[test]
fn the_lead_plans_and_the_agents_do_the_tasks_in_order() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![
        text(PLAN),
        // t1 by coder2
        calls(vec![call(
            "write_file",
            json!({"path":"index.html","content":"<ul id=\"list\"></ul><input id=\"add\">"}),
        )]),
        text("Handoff: index.html made with #list and #add."),
        // t2 by coder2
        calls(vec![call(
            "write_file",
            json!({"path":"style.css","content":"#list { margin: 0; }"}),
        )]),
        text("Handoff: style.css made."),
        // t3 by tester3
        calls(vec![call("read_file", json!({"path":"index.html"}))]),
        text("Tested: the page has #list and #add."),
    ]);
    let rig = rig(&mock);
    let (outcome, events) = run(&rig, &team(), false, true);

    assert!(!outcome.cancelled && !outcome.declined);
    assert_eq!(outcome.plan.count(Status::Done), 3, "{:?}", outcome.plan);
    assert_eq!(
        events,
        vec![
            "planning lead1",
            "plan ready",
            "start t1 coder2",
            "end t1 done",
            "start t2 coder2",
            "end t2 done",
            "start t3 tester3",
            "end t3 done",
        ]
    );
    // The work is in the agents' copy; the project itself is untouched until it is applied.
    assert!(rig.engine.work_dir().join("index.html").is_file());
    assert!(rig.engine.work_dir().join("style.css").is_file());
    assert!(!rig.project.path().join("index.html").exists());

    let seen = mock.received();
    assert_eq!(seen.len(), 7);
    let system = |i: usize| {
        seen[i].body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // The lead was told the team and the rules of a plan.
    assert!(system(0).contains("coder2") && system(0).contains("ONE JSON object"), "{}", system(0));
    // The first task: the contract and the task are in the instructions.
    assert!(system(1).contains("Your task t1") && system(1).contains("#list and #add"), "{}", system(1));
    assert!(system(1).contains("coder of a team"));
    // The second task knows what the first one handed over.
    assert!(system(3).contains("Finished before you started") && system(3).contains("index.html made"), "{}", system(3));
    // The tester has its own role.
    assert!(system(5).contains("tester of a team"), "{}", system(5));

    // The plan is saved with the results, so the project can be continued.
    let saved = Plan::load(&rig.engine.work_dir()).unwrap();
    assert_eq!(saved.count(Status::Done), 3);
    assert_eq!(saved.tasks[0].summary, "Handoff: index.html made with #list and #add.");
}

#[test]
fn a_plan_that_does_not_hold_together_goes_back_to_the_lead() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![
        text("I think we should build a page."),
        text(r#"{"tasks":[{"id":"t1","title":"A","deps":["nope"]}]}"#),
        text(r#"{"tasks":[{"id":"t1","title":"Write the page","files":["a.html"],"owner":"coder2"}]}"#),
        calls(vec![call("write_file", json!({"path":"a.html","content":"<p>x</p>"}))]),
        text("Handoff: done."),
    ]);
    let rig = rig(&mock);
    let (outcome, events) = run(&rig, &team(), false, true);
    assert_eq!(outcome.plan.count(Status::Done), 1);
    assert_eq!(events[0], "planning lead1");
    assert_eq!(events[1], "plan ready");

    let seen = mock.received();
    assert_eq!(seen.len(), 5);
    // Each rejection came back with the reason.
    let last_user = |i: usize| seen[i].body["contents"].as_array().unwrap().last().unwrap().to_string();
    assert!(last_user(1).contains("no JSON object"), "{}", last_user(1));
    assert!(last_user(2).contains("does not exist"), "{}", last_user(2));
}

#[test]
fn a_plan_the_user_does_not_approve_is_not_started() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![text(PLAN)]);
    let rig = rig(&mock);
    let (outcome, events) = run(&rig, &team(), false, false);
    assert!(outcome.declined);
    assert_eq!(events, vec!["planning lead1", "plan ready"]);
    assert_eq!(mock.received().len(), 1, "only the lead was asked");
    assert!(!rig.engine.work_dir().join("index.html").exists());
}

#[test]
fn a_failed_task_blocks_what_depends_on_it_and_resume_tries_again() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![
        text(PLAN),
        // t1 asks for a tool again and again until its steps run out.
        calls(vec![call("read_file", json!({"path":"README.md"}))]),
        calls(vec![call("list_files", json!({}))]),
        calls(vec![call("read_file", json!({"path":"README.md","start_line":1}))]),
    ]);
    let rig = rig(&mock);
    // One agent only, so that nobody else can take the task over.
    let mut team = Team::new(vec![AgentSpec::parse("gemini:solo-model", 0).unwrap()]).unwrap();
    team.agents[0].max_steps = 3;
    let (outcome, events) = run(&rig, &team, false, true);
    assert_eq!(outcome.plan.tasks[0].status, Status::Failed, "{:?}", outcome.plan);
    assert!(outcome.plan.tasks[0].summary.contains("ran out of steps"));
    assert_eq!(outcome.plan.tasks[1].status, Status::Blocked);
    assert_eq!(outcome.plan.tasks[2].status, Status::Blocked);
    assert!(events.contains(&"end t1 failed".to_string()), "{events:?}");

    // Continuing starts the failed and blocked tasks again, without a new plan.
    let mock2 = MockGemini::start(vec![
        text("Handoff: page made."),
        text("Handoff: styles made."),
        text("Tested."),
    ]);
    std::env::set_var("GAGENT_GEMINI_BASE_URL", &mock2.base_url);
    let (outcome, events) = run(&rig, &team, true, true);
    assert_eq!(outcome.plan.count(Status::Done), 3, "{:?}", outcome.plan);
    assert_eq!(events[0], "resumed");
    assert_eq!(mock2.received().len(), 3, "no new plan was asked for");
}

#[test]
fn another_agent_takes_over_a_task_that_failed() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![
        text(r#"{"tasks":[{"id":"t1","title":"Write the page","owner":"coder1"}]}"#),
        // coder1 has one step only and spends it on a tool call.
        calls(vec![call("read_file", json!({"path":"README.md"}))]),
        // coder2 does it.
        text("Handoff: page made by the second agent."),
    ]);
    let rig = rig(&mock);
    let mut team = Team::new(vec![
        AgentSpec::parse("gemini:first-model", 0).unwrap(),
        AgentSpec::parse("gemini:second-model", 1).unwrap(),
    ])
    .unwrap();
    team.agents[0].max_steps = 1;
    let (outcome, events) = run(&rig, &team, false, true);
    assert_eq!(outcome.plan.tasks[0].status, Status::Done, "{:?}", outcome.plan);
    assert_eq!(outcome.plan.tasks[0].owner, "coder2");
    assert!(events.contains(&"start t1 coder1".to_string()), "{events:?}");
    assert!(events.contains(&"start t1 coder2".to_string()), "{events:?}");
    assert!(events.iter().any(|e| e.starts_with("note coder1 could not finish t1")), "{events:?}");
    assert_eq!(events.last().unwrap(), "end t1 done");
    assert_eq!(mock.received().len(), 3);
}
