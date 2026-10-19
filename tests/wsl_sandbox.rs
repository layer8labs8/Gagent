//! The WSL sandbox against a real WSL Ubuntu. Skipped when WSL is not available
//! (Linux, macOS, or Windows without a Linux distribution).

use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gagent::sandbox::{DockerStatus, ExecRequest, ExecResult, Sandbox};
use gagent::wsl::{wsl_status, WslSandbox};

struct Box_ {
    dir: tempfile::TempDir,
    sandbox: WslSandbox,
}

/// A sandbox on a fresh workspace, or `None` (with a notice) when WSL cannot be used.
fn setup() -> Option<Box_> {
    match wsl_status("") {
        Ok(DockerStatus::Wsl { distro, .. }) => {
            let dir = tempfile::tempdir().unwrap();
            let sandbox = WslSandbox::new(&distro, dir.path());
            Some(Box_ { dir, sandbox })
        }
        Ok(other) => {
            eprintln!("SKIPPED: {}", other.describe());
            None
        }
        Err(why) => {
            eprintln!("SKIPPED: {why}");
            None
        }
    }
}

fn run(b: &Box_, command: &str) -> ExecResult {
    run_in(b, "", command, 60)
}

fn run_in(b: &Box_, cwd: &str, command: &str, secs: u32) -> ExecResult {
    b.sandbox
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
    let Some(b) = setup() else { return };
    let r = run(&b, "echo to-stdout; echo to-stderr >&2; exit 3");
    assert_eq!(r.exit_code, Some(3), "{}", r.output);
    assert!(r.output.contains("to-stdout") && r.output.contains("to-stderr"));
    assert!(!r.success());
    let r = run(&b, "echo fine");
    assert!(r.success(), "{}", r.output);
}

#[test]
fn workspace_is_shared_both_ways() {
    let Some(b) = setup() else { return };
    fs::write(b.dir.path().join("from_host.txt"), "hello from windows\n").unwrap();
    let r = run(
        &b,
        "cat from_host.txt && echo from-linux > made_in_linux.txt",
    );
    assert!(r.output.contains("hello from windows"), "{}", r.output);
    let made = fs::read_to_string(b.dir.path().join("made_in_linux.txt")).unwrap();
    assert_eq!(made.trim(), "from-linux");
}

#[test]
fn working_directory_and_home_are_inside_the_workspace() {
    let Some(b) = setup() else { return };
    fs::create_dir_all(b.dir.path().join("sub")).unwrap();
    let r = run_in(&b, "sub", "pwd; echo HOME=$HOME", 60);
    assert!(
        r.output.lines().next().unwrap().ends_with("/sub"),
        "{}",
        r.output
    );
    assert!(r.output.contains("/.gagent/home"), "{}", r.output);
    assert!(b.dir.path().join(".gagent").join("home").is_dir());
}

#[test]
fn quotes_newlines_and_heredocs_arrive_intact() {
    let Some(b) = setup() else { return };
    let r = run(
        &b,
        "name=\"two words\"\necho \"[$name]\" 'single $x'\ncat <<'EOF'\nline \"q\" \\ back\nEOF\nprintf '%s|' a 'b c'",
    );
    assert!(r.output.contains("[two words] single $x"), "{}", r.output);
    assert!(r.output.contains("line \"q\" \\ back"), "{}", r.output);
    assert!(r.output.contains("a|b c|"), "{}", r.output);
}

#[test]
fn windows_environment_and_programs_are_not_visible() {
    let Some(b) = setup() else { return };
    std::env::set_var("GAGENT_TEST_SECRET", "leaked");
    let r = run(
        &b,
        "echo \"secret=[${GAGENT_TEST_SECRET}]\"; echo \"path=$PATH\"; command -v cmd.exe powershell.exe || echo no-windows-programs",
    );
    assert!(r.output.contains("secret=[]"), "{}", r.output);
    // The workspace itself lives on a Windows drive, but no Windows program directories are on PATH.
    assert!(!r.output.contains("/mnt/c/Windows"), "{}", r.output);
    assert!(!r.output.contains("/mnt/c/Program"), "{}", r.output);
    assert!(r.output.contains("no-windows-programs"), "{}", r.output);
}

#[test]
fn the_rust_toolchain_of_the_linux_user_is_found() {
    let Some(b) = setup() else { return };
    let r = run(&b, "echo \"$PATH\" | tr ':' '\\n' | grep -c '/.cargo/bin$'");
    assert_eq!(
        r.output.trim(),
        "1",
        "the cargo directory must be on PATH: {}",
        r.output
    );
    let r = run(&b, "echo \"$CARGO_HOME|$RUSTUP_HOME\"");
    assert!(r.output.contains("/.gagent/home/.cargo|"), "{}", r.output);
}

#[test]
fn sudo_and_apt_fail_at_once_instead_of_hanging() {
    let Some(b) = setup() else { return };
    let started = Instant::now();
    let r = run(&b, "sudo apt update; echo \"exit=$?\"");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it must not wait for a password"
    );
    assert!(
        r.output.contains("not available in this sandbox"),
        "{}",
        r.output
    );
    assert!(r.output.contains("exit=1"), "{}", r.output);
    let r = run(&b, "apt-get install -y gcc; echo \"exit=$?\"");
    assert!(r.output.contains("exit=1"), "{}", r.output);
}

#[test]
fn the_vm_is_kept_warm_while_the_sandbox_is_used_and_released_on_reset() {
    let Some(b) = setup() else { return };
    assert!(!b.sandbox.is_warm());
    let r = run(&b, "echo hi");
    assert!(r.success(), "{}", r.output);
    assert!(
        b.sandbox.is_warm(),
        "a holder process keeps WSL from switching itself off"
    );
    // Further commands reuse it.
    let started = Instant::now();
    let r = run(&b, "echo again");
    assert!(r.success());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "warm commands are quick"
    );
    b.sandbox.reset().unwrap();
    assert!(!b.sandbox.is_warm(), "reset releases the VM");
}

#[test]
fn timeout_kills_the_command() {
    let Some(b) = setup() else { return };
    let started = Instant::now();
    let r = run_in(&b, "", "sleep 60", 2);
    assert!(r.timed_out, "{r:?}");
    assert!(!r.success());
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn cancel_stops_the_linux_process_too() {
    let Some(b) = setup() else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = Instant::now();
    let r = b
        .sandbox
        .exec(
            &ExecRequest {
                command: "sleep 61".into(),
                cwd: String::new(),
                timeout_secs: 120,
            },
            &cancel,
        )
        .unwrap();
    trigger.join().unwrap();
    assert!(r.cancelled, "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(40));
    std::thread::sleep(Duration::from_secs(2));
    let left = run(&b, "pgrep -f '[s]leep 61' | wc -l");
    assert_eq!(
        left.output.trim(),
        "0",
        "the sleep process is still running"
    );
}

#[test]
fn bad_input_is_rejected_before_wsl_is_started() {
    let Some(b) = setup() else { return };
    let err = b
        .sandbox
        .exec(
            &ExecRequest {
                command: "ls".into(),
                cwd: "../..".into(),
                timeout_secs: 5,
            },
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(err.to_string().contains(".."), "{err}");
    let err = b
        .sandbox
        .exec(
            &ExecRequest {
                command: "   ".into(),
                cwd: String::new(),
                timeout_secs: 5,
            },
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[test]
fn status_names_the_distribution() {
    let Some(b) = setup() else { return };
    drop(b);
    let status = wsl_status("").unwrap();
    assert!(status.is_ready() && status.is_wsl());
    assert!(
        status.describe().starts_with("WSL "),
        "{}",
        status.describe()
    );
    assert!(wsl_status("no-such-distro-xyz").is_err());
}
