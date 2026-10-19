//! Running commands with bash directly on this computer (Git Bash on Windows).
//! Skipped when no bash is found.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gagent::config::{CheckLevel, PermissionMode, SandboxBackend, SandboxSettings};
use gagent::permission::{Approvals, Decision};
use gagent::host::{find_shell, HostSandbox};
use gagent::sandbox::{self, Backend, ExecRequest, ExecResult, Sandbox};
use gagent::tools::{self, ToolContext};
use gagent::workspace::{WalkOptions, Workspace};
use serde_json::json;

struct Rig {
    dir: tempfile::TempDir,
    sandbox: HostSandbox,
}

fn setup() -> Option<Rig> {
    let Some(shell) = find_shell() else {
        eprintln!("SKIPPED: no bash on this computer");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let sandbox = HostSandbox::new(&shell, dir.path());
    Some(Rig { dir, sandbox })
}

fn run(r: &Rig, command: &str) -> ExecResult {
    run_in(r, "", command, 60)
}

fn run_in(r: &Rig, cwd: &str, command: &str, secs: u32) -> ExecResult {
    r.sandbox
        .exec(
            &ExecRequest {
                command: command.into(),
                cwd: cwd.into(),
                timeout_secs: secs,
            },
            &AtomicBool::new(false),
        )
        .unwrap()
}

#[test]
fn runs_commands_and_captures_both_streams() {
    let Some(r) = setup() else { return };
    let out = run(&r, "echo to-stdout; echo to-stderr >&2; exit 3");
    assert_eq!(out.exit_code, Some(3), "{}", out.output);
    assert!(out.output.contains("to-stdout") && out.output.contains("to-stderr"));
    assert!(!out.success());
    let out = run(&r, "echo fine");
    assert!(out.success(), "{}", out.output);
}

#[test]
fn commands_start_in_the_workspace_and_home_is_inside_it() {
    let Some(r) = setup() else { return };
    fs::create_dir_all(r.dir.path().join("sub")).unwrap();
    fs::write(r.dir.path().join("sub").join("a.txt"), "from-the-host\n").unwrap();
    let out = run(&r, "ls; cat sub/a.txt");
    assert!(out.output.contains("sub") && out.output.contains("from-the-host"), "{}", out.output);
    let out = run_in(&r, "sub", "cat a.txt && echo made > b.txt", 30);
    assert!(out.success(), "{}", out.output);
    assert!(r.dir.path().join("sub").join("b.txt").is_file());
    // HOME is private to the workspace, so `~` never reaches the user's own files.
    let out = run(&r, "echo \"$HOME\" > home.txt; cd ~ && pwd >> home.txt; cat home.txt");
    assert!(out.output.contains(".gagent"), "{}", out.output);
}

#[test]
fn quotes_variables_unicode_and_heredocs_arrive_unchanged() {
    let Some(r) = setup() else { return };
    let command = "NAME=\"é ü ö\"\n\
        cat > note.txt <<'EOF'\n\
        back\\slash \"double\" 'single' $NOT_EXPANDED `not run`\n\
        EOF\n\
        printf '%s|' \"$NAME\" >> note.txt\n\
        echo done";
    let command = command.replace("        ", "");
    let out = run(&r, &command);
    assert!(out.success(), "{}", out.output);
    let text = fs::read_to_string(r.dir.path().join("note.txt")).unwrap();
    assert!(text.contains("back\\slash \"double\" 'single' $NOT_EXPANDED `not run`"), "{text}");
    assert!(text.contains("é ü ö|"), "{text}");
}

#[test]
fn secrets_in_the_environment_are_not_passed_on() {
    let Some(r) = setup() else { return };
    std::env::set_var("GAGENT_TEST_API_KEY", "must-not-leak");
    std::env::set_var("GAGENT_TEST_HARMLESS_VALUE", "visible");
    let out = run(
        &r,
        "echo \"key=[$GAGENT_TEST_API_KEY] other=[$GAGENT_TEST_HARMLESS_VALUE] path_set=${PATH:+yes}\"",
    );
    std::env::remove_var("GAGENT_TEST_API_KEY");
    std::env::remove_var("GAGENT_TEST_HARMLESS_VALUE");
    assert!(out.output.contains("key=[]"), "{}", out.output);
    // GAGENT_* names are all private to the app.
    assert!(out.output.contains("other=[]"), "{}", out.output);
    assert!(out.output.contains("path_set=yes"), "{}", out.output);
    let out = run(&r, "git --version");
    assert!(out.success() || out.output.contains("not found"), "{}", out.output);
}

#[test]
fn sudo_and_apt_fail_at_once() {
    let Some(r) = setup() else { return };
    let started = Instant::now();
    let out = run(&r, "sudo apt update; echo \"exit=$?\"");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(out.output.contains("not available here"), "{}", out.output);
    assert!(out.output.contains("exit=1"), "{}", out.output);
}

#[test]
fn python3_works_when_python_is_installed() {
    let Some(r) = setup() else { return };
    if !run(&r, "command -v python >/dev/null 2>&1 || command -v python3 >/dev/null 2>&1").success()
    {
        eprintln!("SKIPPED: no Python on this computer");
        return;
    }
    let out = run(&r, "python3 -c \"print('sum', 20 + 22)\"");
    assert!(out.success() && out.output.contains("sum 42"), "{}", out.output);
}

#[test]
fn timeout_kills_the_command_and_what_it_started() {
    let Some(r) = setup() else { return };
    let started = Instant::now();
    let out = run_in(&r, "", "echo before; sleep 60; echo after", 2);
    assert!(out.timed_out, "{out:?}");
    assert!(out.output.contains("before") && !out.output.contains("after"));
    assert_eq!(out.exit_code, None);
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
}

#[test]
fn cancel_stops_the_command() {
    let Some(r) = setup() else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        c.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let out = r
        .sandbox
        .exec(
            &ExecRequest {
                command: "sleep 60".into(),
                cwd: String::new(),
                timeout_secs: 120,
            },
            &cancel,
        )
        .unwrap();
    trigger.join().unwrap();
    assert!(out.cancelled, "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
}

#[test]
fn a_background_process_does_not_hold_the_command_back() {
    let Some(r) = setup() else { return };
    let started = Instant::now();
    let out = run(&r, "(sleep 20 &) ; echo started");
    assert!(out.success() && out.output.contains("started"), "{}", out.output);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited {:?} for a background process",
        started.elapsed()
    );
}

#[test]
fn bad_input_is_rejected() {
    let Some(r) = setup() else { return };
    for (command, cwd) in [("ls", "../.."), ("ls", "missing_folder"), ("   ", "")] {
        let err = r
            .sandbox
            .exec(
                &ExecRequest {
                    command: command.into(),
                    cwd: cwd.into(),
                    timeout_secs: 5,
                },
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

#[test]
fn the_unix_commands_run_at_a_lower_priority() {
    let Some(r) = setup() else { return };
    if cfg!(windows) {
        return;
    }
    let out = run(&r, "nice");
    let value: i32 = out.output.trim().parse().unwrap_or(0);
    assert!(value >= 10, "niceness {value}");
}

fn ctx_for(r: &Rig) -> ToolContext {
    ToolContext {
        work: Workspace::new(r.dir.path()).unwrap(),
        sandbox: Arc::new(HostSandbox::new(&find_shell().unwrap(), r.dir.path())),
        opts: WalkOptions::default(),
        default_timeout_secs: 30,
        cancel: Arc::new(AtomicBool::new(false)),
        check: CheckLevel::Off,
        tools_note: Arc::new(Mutex::new(Some(String::new()))),
        approvals: Arc::new(Approvals::new(PermissionMode::Auto)),
        reads: Default::default(),
    }
}

#[test]
fn the_agent_tools_stop_dangerous_commands_before_they_run() {
    let Some(r) = setup() else { return };
    let ctx = ctx_for(&r);
    // Whoever is asked says no, so everything that is not plainly fine must fail.
    ctx.approvals.set_responder(|_| Decision::Deny);
    fs::write(r.dir.path().join("keep.txt"), "important").unwrap();
    for (command, word) in [
        ("sudo rm -rf .", "blocked"),
        ("shutdown /s /t 0", "blocked"),
        ("rm -rf ~", "blocked"),
        ("cat /etc/passwd", "did not allow"),
        ("taskkill /IM explorer.exe /F", "did not allow"),
        ("cd ../.. && ls", "did not allow"),
        ("pip install requests", "did not allow"),
    ] {
        let out = tools::execute(&ctx, "run_command", &json!({ "command": command }));
        assert!(!out.ok, "{command}");
        assert!(out.text.contains(word), "{command}: {}", out.text);
    }
    assert_eq!(fs::read_to_string(r.dir.path().join("keep.txt")).unwrap(), "important");
    // An ordinary command goes through, and its own workspace path may be named.
    let here = ctx.work.root().to_string_lossy().replace('\\', "/");
    let out = tools::execute(
        &ctx,
        "run_command",
        &json!({ "command": format!("cd \"{here}\" && cat keep.txt") }),
    );
    assert!(out.ok && out.text.contains("important"), "{}", out.text);
}

#[test]
fn automatic_choice_prefers_this_computer() {
    let Some(shell) = find_shell() else { return };
    let mut settings = SandboxSettings::default();
    assert_eq!(sandbox::resolve_backend(&settings), Backend::Host(shell.clone()));
    assert!(sandbox::sandbox_status(&settings).is_host());
    settings.backend = SandboxBackend::Host;
    assert_eq!(sandbox::resolve_backend(&settings), Backend::Host(shell));
}

#[test]
fn a_missing_tool_just_fails_and_nothing_else_is_started() {
    let Some(r) = setup() else { return };
    // The same shell is used for every command, whatever the project holds.
    fs::write(r.dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let out = run(&r, "echo here; definitely-not-installed-tool --version; echo \"exit=$?\"");
    assert!(out.output.contains("here"), "{}", out.output);
    assert!(out.output.contains("exit=127"), "{}", out.output);
    assert!(!out.output.contains("/mnt/c/"), "this ran in WSL: {}", out.output);
}
