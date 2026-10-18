//! The real Docker sandbox. Skipped automatically when Docker is not available.

mod support;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gagent::config::SandboxSettings;
use gagent::sandbox::{DockerSandbox, ExecRequest, Sandbox};
use gagent::util::unique_id;

macro_rules! require_docker {
    () => {
        if !support::docker_ready() {
            return;
        }
    };
}

struct Box_ {
    dir: tempfile::TempDir,
    sandbox: DockerSandbox,
}

fn sandbox_with(settings: SandboxSettings) -> Box_ {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = DockerSandbox::new(
        settings,
        dir.path(),
        &format!("test-{}", unique_id()),
        Arc::new(|l| eprintln!("  [sandbox] {l}")),
    );
    Box_ { dir, sandbox }
}

fn run_host(cmd: &str) -> String {
    let out = std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn new_box() -> Box_ {
    sandbox_with(SandboxSettings::default())
}

fn run(b: &Box_, cmd: &str) -> gagent::sandbox::ExecResult {
    run_t(b, cmd, 60)
}

fn run_t(b: &Box_, cmd: &str, secs: u32) -> gagent::sandbox::ExecResult {
    b.sandbox
        .exec(
            &ExecRequest {
                command: cmd.into(),
                cwd: String::new(),
                timeout_secs: secs,
            },
            &AtomicBool::new(false),
        )
        .unwrap()
}

#[test]
fn runs_commands_and_captures_both_streams() {
    require_docker!();
    let b = new_box();
    let r = run(&b, "echo out; echo err >&2; exit 3");
    assert_eq!(r.exit_code, Some(3));
    assert!(
        r.output.contains("out") && r.output.contains("err"),
        "{}",
        r.output
    );
    assert!(!r.timed_out && !r.cancelled);
    assert!(run(&b, "true").success());
    b.sandbox.shutdown();
}

#[test]
fn workspace_is_shared_both_ways() {
    require_docker!();
    let b = new_box();
    fs::write(b.dir.path().join("in.txt"), "from host\n").unwrap();
    let r = run(
        &b,
        "cat in.txt && echo 'from sandbox' > out.txt && mkdir -p sub/dir && echo x > sub/dir/f.txt",
    );
    assert!(r.success(), "{}", r.output);
    assert_eq!(
        fs::read_to_string(b.dir.path().join("out.txt")).unwrap(),
        "from sandbox\n"
    );
    assert!(b.dir.path().join("sub/dir/f.txt").is_file());
    // Working directory handling.
    let r = b
        .sandbox
        .exec(
            &ExecRequest {
                command: "pwd".into(),
                cwd: "sub/dir".into(),
                timeout_secs: 20,
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(r.output.trim(), "/workspace/sub/dir");
    assert!(b
        .sandbox
        .exec(
            &ExecRequest {
                command: "pwd".into(),
                cwd: "missing".into(),
                timeout_secs: 20
            },
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(b
        .sandbox
        .exec(
            &ExecRequest {
                command: "pwd".into(),
                cwd: "../..".into(),
                timeout_secs: 20
            },
            &AtomicBool::new(false)
        )
        .is_err());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let host = fs::metadata(b.dir.path()).unwrap().uid();
        assert_eq!(
            fs::metadata(b.dir.path().join("out.txt")).unwrap().uid(),
            host,
            "files must belong to the host user"
        );
    }
    b.sandbox.shutdown();
}

#[test]
fn toolchains_are_available() {
    require_docker!();
    let b = new_box();
    let r = run(&b, "python3 -c 'print(6*7)'");
    assert_eq!(r.output.trim(), "42", "{}", r.output);

    fs::write(
        b.dir.path().join("hello.c"),
        "#include <stdio.h>\nint main(void){puts(\"hello from c\");return 0;}\n",
    )
    .unwrap();
    let r = run(&b, "gcc hello.c -o hello && ./hello");
    assert_eq!(r.output.trim(), "hello from c", "{}", r.output);

    let r = run(&b, "git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm init && git log --oneline | wc -l");
    assert!(r.success(), "git must work in the workspace: {}", r.output);
    assert_eq!(r.output.trim(), "1");

    let r = run(
        &b,
        "python3 -m venv .gagent/venv >/dev/null 2>&1 && echo venv-ok",
    );
    assert!(r.output.contains("venv-ok"), "{}", r.output);
    let r = run(&b, "echo 'print(\"user site ok\")' > t.py && python3 t.py");
    assert_eq!(r.output.trim(), "user site ok");
    b.sandbox.shutdown();
}

#[test]
fn timeout_kills_the_command() {
    require_docker!();
    let b = new_box();
    let started = Instant::now();
    let r = run_t(&b, "sleep 60", 2);
    assert!(r.timed_out, "{r:?}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    // The sandbox keeps working afterwards.
    assert!(run(&b, "echo alive").output.contains("alive"));
    b.sandbox.shutdown();
}

#[test]
fn cancel_stops_a_running_command_and_recovers() {
    require_docker!();
    let b = new_box();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let r = b
        .sandbox
        .exec(
            &ExecRequest {
                command: "sleep 120".into(),
                cwd: String::new(),
                timeout_secs: 300,
            },
            &cancel,
        )
        .unwrap();
    assert!(r.cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "took {:?}",
        started.elapsed()
    );
    assert!(run(&b, "echo again").output.contains("again"));
    b.sandbox.shutdown();
}

#[test]
fn sandbox_is_locked_down() {
    require_docker!();
    let b = new_box();
    // The sandbox runs as the host user (never as root, unless the app itself runs as root).
    let r = run(&b, "id -u");
    let host_is_root = run_host("id -u") == "0";
    if !host_is_root {
        assert_ne!(r.output.trim(), "0", "must not run as root");
        let r = run(&b, "touch /etc/gagent-test 2>&1; echo status=$?");
        assert!(!r.output.contains("status=0"), "{}", r.output);
    }
    let r = run(&b, "grep CapEff /proc/self/status");
    assert!(
        r.output.contains("0000000000000000"),
        "no capabilities expected: {}",
        r.output
    );
    let r = run(&b, "cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes");
    let limit: u64 = r.output.trim().parse().unwrap_or(0);
    assert_eq!(limit, 2048 * 1024 * 1024, "memory limit: {}", r.output);
    // The host's files are not visible.
    let r = run(&b, "ls /home /root 2>&1 | head -5");
    assert!(!r.output.contains("user"), "{}", r.output);
    // Writing inside the workspace is allowed.
    assert!(run(&b, "echo ok > allowed.txt").success());
    b.sandbox.shutdown();
}

#[test]
fn secrets_from_the_host_environment_do_not_leak() {
    require_docker!();
    std::env::set_var("GEMINI_API_KEY", "SECRET-KEY-VALUE");
    let b = new_box();
    let r = run(&b, "env");
    assert!(!r.output.contains("SECRET-KEY-VALUE"));
    assert!(!r.output.contains("GEMINI"));
    b.sandbox.shutdown();
}

#[test]
fn network_can_be_switched_off() {
    require_docker!();
    let mut s = SandboxSettings::default();
    s.network = false;
    let b = sandbox_with(s);
    let r = run(
        &b,
        "curl -sS -m 5 https://example.com >/dev/null 2>&1; echo status=$?",
    );
    assert!(!r.output.contains("status=0"), "{}", r.output);
    let r = run(&b, "ls /sys/class/net");
    assert_eq!(
        r.output.trim(),
        "lo",
        "only loopback expected: {}",
        r.output
    );
    b.sandbox.shutdown();
}

#[test]
fn changing_settings_recreates_the_container() {
    require_docker!();
    let dir = tempfile::tempdir().unwrap();
    let id = format!("test-{}", unique_id());
    let progress: gagent::sandbox::Progress = Arc::new(|_| {});
    let with_net = DockerSandbox::new(
        SandboxSettings::default(),
        dir.path(),
        &id,
        progress.clone(),
    );
    let req = ExecRequest {
        command: "ls /sys/class/net | wc -l".into(),
        cwd: String::new(),
        timeout_secs: 30,
    };
    let r = with_net.exec(&req, &AtomicBool::new(false)).unwrap();
    assert!(r.output.trim().parse::<u32>().unwrap() >= 2, "{}", r.output);
    let mut off = SandboxSettings::default();
    off.network = false;
    let without = DockerSandbox::new(off, dir.path(), &id, progress);
    let r = without.exec(&req, &AtomicBool::new(false)).unwrap();
    assert_eq!(r.output.trim(), "1", "{}", r.output);
    without.shutdown();
    std::mem::forget(with_net); // its container was already replaced
}

#[test]
fn huge_output_is_bounded() {
    require_docker!();
    let b = new_box();
    let r = run(
        &b,
        "yes 'a line of output' | head -c 4000000; echo; echo THE-END-MARKER",
    );
    assert!(r.truncated);
    assert!(r.output.contains("THE-END-MARKER"));
    assert!(r.output.len() < 1_200_000, "{}", r.output.len());
    b.sandbox.shutdown();
}

#[test]
fn symlinks_planted_in_the_sandbox_cannot_reach_the_host() {
    require_docker!();
    let b = new_box();
    assert!(run(&b, "ln -s /etc/passwd evil && ln -s / rootlink").success());
    let ws = gagent::workspace::Workspace::new(b.dir.path()).unwrap();
    assert!(ws.resolve("evil").is_err());
    assert!(ws.resolve("rootlink/etc/passwd").is_err());
    b.sandbox.shutdown();
}

#[test]
fn reset_removes_the_container() {
    require_docker!();
    let b = new_box();
    assert!(run(&b, "echo hi").success());
    let name = b.sandbox.container_name().to_string();
    let listed = |name: &str| {
        let out = std::process::Command::new("docker")
            .args(["ps", "-aq", "--filter", &format!("name={name}")])
            .output()
            .unwrap();
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    };
    assert!(listed(&name));
    b.sandbox.reset().unwrap();
    assert!(!listed(&name));
    // It starts again on demand.
    assert!(run(&b, "echo back").output.contains("back"));
    b.sandbox.shutdown();
}

#[test]
fn status_reports_ready() {
    require_docker!();
    let st = gagent::sandbox::docker_status(gagent::config::DEFAULT_SANDBOX_IMAGE);
    assert!(st.is_ready(), "{st:?}");
    let st = gagent::sandbox::docker_status("gagent-image-that-does-not-exist:none");
    assert!(matches!(
        st,
        gagent::sandbox::DockerStatus::Ready {
            image_present: false,
            ..
        }
    ));
}
