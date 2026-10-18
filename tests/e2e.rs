//! Full pipeline: project folder -> shadow copy -> scripted Gemini -> real Docker sandbox
//! -> apply to the project -> undo. Skipped when Docker is not available.

mod support;

use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gagent::agent::{AgentEvent, TurnEnd};
use gagent::config::{AppDirs, Settings};
use gagent::engine::Engine;
use gagent::shadow::ChangeKind;
use serde_json::json;
use support::*;

const BUGGY: &str =
    "def average(values):\n    return sum(values) / len(values)\n\nprint(average([]))\n";

#[test]
fn agent_fixes_a_project_in_the_sandbox_and_user_applies_the_result() {
    if !docker_ready() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("calc.py"), BUGGY).unwrap();
    fs::write(project.path().join("README.md"), "# demo\n").unwrap();
    fs::write(project.path().join(".env"), "SECRET=1\n").unwrap();

    let mock = MockGemini::start(vec![
        calls(vec![call("read_file", json!({"path": "calc.py"}))]),
        calls(vec![call(
            "run_command",
            json!({"command": "python3 calc.py"}),
        )]),
        calls(vec![call(
            "edit_file",
            json!({
                "path": "calc.py",
                "old_text": "    return sum(values) / len(values)\n",
                "new_text": "    if not values:\n        return 0.0\n    return sum(values) / len(values)\n"
            }),
        )]),
        calls(vec![call(
            "run_command",
            json!({"command": "python3 calc.py"}),
        )]),
        calls(vec![call(
            "write_file",
            json!({"path": "tests/test_calc.py", "content": "from calc import average\n\ndef test_empty():\n    assert average([]) == 0.0\n"}),
        )]),
        text("average() now returns 0.0 for an empty list; verified by running the script."),
    ]);
    std::env::set_var("GAGENT_GEMINI_BASE_URL", &mock.base_url);

    let dirs = AppDirs::at(home.path());
    let mut settings = Settings::default();
    settings.api_key = "E2E-KEY".into();
    let engine = Engine::open(
        &dirs,
        &settings,
        project.path(),
        "e2e-session",
        Arc::new(|l| eprintln!("  [sandbox] {l}")),
        None,
    )
    .unwrap();

    // The private copy exists; the secret file was not copied.
    assert!(engine.work_dir().join("calc.py").is_file());
    assert!(!engine.work_dir().join(".env").exists());

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let emit = move |e: AgentEvent| sink.lock().unwrap().push(e);
    let mut history = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = engine
        .run_message(
            &mut history,
            "calc.py crashes on empty lists, please fix it",
            &emit,
            &cancel,
        )
        .unwrap();

    assert_eq!(outcome.turn.end, TurnEnd::Finished, "{:?}", outcome.turn);
    assert!(outcome.auto_applied.is_none());

    // The model saw the real Python traceback from the sandbox, then a clean run.
    let received = mock.received();
    let second_run = received[3].body.to_string();
    assert!(
        second_run.contains("ZeroDivisionError"),
        "the failing log must reach the model"
    );
    let final_request = received[5].body.to_string();
    assert!(final_request.contains("exit code: 0"));
    assert!(final_request.contains("0.0"));
    // The prompt told the model about the sandbox and listed the project.
    let system = received[0].body["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(system.contains("Ubuntu 24.04 sandbox"), "{system}");
    assert!(system.contains("calc.py"));
    assert!(!system.contains(".env"));

    let ends: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolEnd { ok, summary, .. } => Some((*ok, summary.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 5);
    assert!(!ends[1].0 && ends[1].1.starts_with("exit 1"), "{ends:?}");
    assert!(ends[3].0 && ends[3].1.starts_with("exit 0"), "{ends:?}");

    // The real project is untouched until the changes are applied.
    assert_eq!(
        fs::read_to_string(project.path().join("calc.py")).unwrap(),
        BUGGY
    );
    assert!(!project.path().join("tests").exists());

    let changes = engine.changes().unwrap();
    let summary: Vec<_> = changes.iter().map(|c| (c.rel.as_str(), c.kind)).collect();
    assert_eq!(
        summary,
        vec![
            ("calc.py", ChangeKind::Modified),
            ("tests/test_calc.py", ChangeKind::Added)
        ]
    );
    let diff = engine.diff("calc.py").unwrap();
    assert!(diff.contains("+    if not values:"), "{diff}");

    // Apply, check the project, then undo.
    let report = engine.apply_all().unwrap();
    assert_eq!(report.applied.len(), 2, "{report:?}");
    assert!(fs::read_to_string(project.path().join("calc.py"))
        .unwrap()
        .contains("if not values"));
    assert!(project.path().join("tests/test_calc.py").is_file());
    assert!(engine.changes().unwrap().is_empty());

    let restored = engine.revert(&report.backup.unwrap()).unwrap();
    assert_eq!(restored, 2);
    assert_eq!(
        fs::read_to_string(project.path().join("calc.py")).unwrap(),
        BUGGY
    );
    assert!(!project.path().join("tests/test_calc.py").exists());
    assert_eq!(
        fs::read_to_string(project.path().join(".env")).unwrap(),
        "SECRET=1\n"
    );

    engine.shutdown();
}
