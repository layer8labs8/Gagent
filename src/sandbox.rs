//! The code execution sandbox.
//!
//! Commands run inside a Docker container (Ubuntu) that has the shadow
//! workspace mounted at `/workspace`. The container has no capabilities, runs
//! as an unprivileged user, has memory/CPU/process limits, and never sees the
//! Gemini API key or your real project folder.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::SandboxSettings;
use crate::util::{strip_ansi, unique_id};
use crate::workspace::{Workspace, SANDBOX_MOUNT};

/// Dockerfile of the sandbox image, embedded so the app can build it itself.
pub const SANDBOX_DOCKERFILE: &str = include_str!("../docker/sandbox/Dockerfile");

/// Largest amount of command output that is kept in memory.
const CAPTURE_HEAD_BYTES: usize = 200_000;
const CAPTURE_TAIL_BYTES: usize = 800_000;

/// A command to run in the sandbox.
#[derive(Clone, Debug)]
pub struct ExecRequest {
    pub command: String,
    /// Directory relative to the workspace root (`""` for the root).
    pub cwd: String,
    pub timeout_secs: u32,
}

/// The outcome of a sandboxed command.
#[derive(Clone, Debug, Default)]
pub struct ExecResult {
    /// Exit status; `None` when the command was killed or never started.
    pub exit_code: Option<i32>,
    /// Combined stdout and stderr with terminal escape codes removed.
    pub output: String,
    pub timed_out: bool,
    pub cancelled: bool,
    /// Some output in the middle was dropped because it was too long.
    pub truncated: bool,
    pub duration: Duration,
}

impl ExecResult {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.cancelled
    }
}

/// Anything that can run shell commands against the shadow workspace.
pub trait Sandbox: Send + Sync {
    /// Runs a command and waits for it to finish.
    fn exec(&self, req: &ExecRequest, cancel: &AtomicBool) -> Result<ExecResult>;
    /// Throws away the running environment (installed packages, processes).
    fn reset(&self) -> Result<()>;
}

/// Receives human-readable progress lines (image build output and so on).
pub type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// State of the Docker installation, for the status indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerStatus {
    Ready {
        version: String,
        image_present: bool,
    },
    /// The `docker` command was not found.
    NotInstalled,
    /// Docker is installed but the engine does not answer.
    NotRunning(String),
}

impl DockerStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, DockerStatus::Ready { .. })
    }

    /// One line for the UI.
    pub fn describe(&self) -> String {
        match self {
            DockerStatus::Ready { version, image_present: true } => {
                format!("Docker {version} ready")
            }
            DockerStatus::Ready { version, image_present: false } => {
                format!("Docker {version} ready, sandbox image not built yet")
            }
            DockerStatus::NotInstalled => {
                "Docker is not installed. Install Docker Desktop (Windows/macOS) or Docker Engine (Linux).".into()
            }
            DockerStatus::NotRunning(why) => {
                format!("Docker is installed but not running: {why}")
            }
        }
    }
}

/// Name of the docker executable (overridable for tests and Podman users).
pub fn docker_binary() -> String {
    std::env::var("GAGENT_DOCKER")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "docker".into())
}

/// Checks whether Docker can be used and whether the sandbox image exists.
pub fn docker_status(image: &str) -> DockerStatus {
    let docker = docker_binary();
    let out = run_quick(
        &docker,
        &["version", "--format", "{{.Server.Version}}"],
        Duration::from_secs(15),
    );
    match out {
        Err(e) if is_not_found(&e) => DockerStatus::NotInstalled,
        Err(e) => DockerStatus::NotRunning(e.to_string()),
        Ok((code, stdout, stderr)) => {
            if code != 0 {
                let reason = first_line(&stderr)
                    .unwrap_or_else(|| "the Docker engine did not answer".into());
                return DockerStatus::NotRunning(reason);
            }
            let present = matches!(
                run_quick(
                    &docker,
                    &["image", "inspect", image],
                    Duration::from_secs(15)
                ),
                Ok((0, _, _))
            );
            DockerStatus::Ready {
                version: stdout.trim().to_string(),
                image_present: present,
            }
        }
    }
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .map(|io| io.kind() == std::io::ErrorKind::NotFound)
        .unwrap_or(false)
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn base_command(program: &str) -> Command {
    let cmd = Command::new(program);
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd
    };
    cmd
}

/// Runs a short command and returns (exit code, stdout, stderr).
fn run_quick(program: &str, args: &[&str], timeout: Duration) -> Result<(i32, String, String)> {
    let mut cmd = base_command(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(anyhow::Error::from)?;
    let out = child.stdout.take();
    let err = child.stderr.take();
    let h_out = thread::spawn(move || read_all(out));
    let h_err = thread::spawn(move || read_all(err));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "`{program} {}` timed out after {}s",
                    args.join(" "),
                    timeout.as_secs()
                );
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    let stdout = h_out.join().unwrap_or_default();
    let stderr = h_err.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), stdout, stderr))
}

fn read_all(pipe: Option<impl Read>) -> String {
    let mut buf = Vec::new();
    if let Some(mut p) = pipe {
        let _ = p.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Keeps the start and the end of a long output stream.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: usize,
}

impl Capture {
    fn push(&mut self, data: &[u8]) {
        let mut data = data;
        if self.head.len() < CAPTURE_HEAD_BYTES {
            let take = (CAPTURE_HEAD_BYTES - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        if data.is_empty() {
            return;
        }
        self.tail.extend(data.iter().copied());
        while self.tail.len() > CAPTURE_TAIL_BYTES {
            self.tail.pop_front();
            self.dropped += 1;
        }
    }

    fn finish(self) -> (String, bool) {
        let mut bytes = self.head;
        let truncated = self.dropped > 0;
        if truncated {
            bytes.extend_from_slice(
                format!("\n[... {} bytes of output dropped ...]\n", self.dropped).as_bytes(),
            );
        }
        bytes.extend(self.tail);
        (strip_ansi(&String::from_utf8_lossy(&bytes)), truncated)
    }
}

/// Docker-backed sandbox.
pub struct DockerSandbox {
    docker: String,
    settings: SandboxSettings,
    work_dir: PathBuf,
    container: String,
    progress: Progress,
    lifecycle: Mutex<()>,
}

impl DockerSandbox {
    pub fn new(
        settings: SandboxSettings,
        work_dir: &Path,
        session_id: &str,
        progress: Progress,
    ) -> Self {
        let clean: String = session_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        Self {
            docker: docker_binary(),
            settings,
            work_dir: work_dir.to_path_buf(),
            container: format!("gagent-{clean}"),
            progress,
            lifecycle: Mutex::new(()),
        }
    }

    pub fn container_name(&self) -> &str {
        &self.container
    }

    fn say(&self, line: &str) {
        (self.progress)(line);
    }

    fn docker(&self, args: &[&str], timeout: Duration) -> Result<(i32, String, String)> {
        run_quick(&self.docker, args, timeout).map_err(|e| {
            if is_not_found(&e) {
                anyhow!("Docker is not installed or not on PATH")
            } else {
                e
            }
        })
    }

    /// Builds the sandbox image from the embedded Dockerfile.
    pub fn build_image(&self) -> Result<()> {
        build_image(&self.docker, &self.settings.image, &self.progress)
    }

    fn fingerprint(&self) -> String {
        let s = &self.settings;
        format!(
            "{}|net={}|mem={}|cpu={}|pids={}|{}",
            s.image,
            s.network,
            s.memory_mb,
            s.cpus,
            s.pids_limit,
            self.work_dir.display()
        )
    }

    fn container_is_current(&self) -> bool {
        let fmt = "{{.State.Running}}|{{index .Config.Labels \"gagent.fingerprint\"}}";
        match self.docker(
            &["inspect", "-f", fmt, &self.container],
            Duration::from_secs(20),
        ) {
            Ok((0, out, _)) => {
                let out = out.trim();
                match out.split_once('|') {
                    Some((running, fp)) => running == "true" && fp == self.fingerprint(),
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn ensure_ready(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        // Fast path: the container is already running with the current settings.
        if self.container_is_current() {
            return Ok(());
        }
        match docker_status(&self.settings.image) {
            DockerStatus::Ready { image_present, .. } => {
                if !image_present {
                    self.say("Building the sandbox image (first run only, this can take a few minutes)...");
                    self.build_image()?;
                }
            }
            other => bail!("{}", other.describe()),
        }
        let _ = self.docker(&["rm", "-f", &self.container], Duration::from_secs(30));
        self.say("Starting sandbox container...");

        let work = dunce::canonicalize(&self.work_dir)
            .with_context(|| format!("workspace missing: {}", self.work_dir.display()))?;
        // HOME lives inside the workspace so installed packages survive container restarts.
        std::fs::create_dir_all(work.join(".gagent").join("home"))
            .context("cannot create the sandbox home directory")?;
        let s = &self.settings;
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            self.container.clone(),
            "--init".into(),
            "--label".into(),
            "gagent=1".into(),
            "--label".into(),
            format!("gagent.fingerprint={}", self.fingerprint()),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--memory".into(),
            format!("{}m", s.memory_mb),
            "--memory-swap".into(),
            format!("{}m", s.memory_mb),
            "--cpus".into(),
            format!("{}", s.cpus),
            "--pids-limit".into(),
            s.pids_limit.to_string(),
        ];
        if let Some((uid, gid)) = host_user() {
            args.push("--user".into());
            args.push(format!("{uid}:{gid}"));
        } else {
            args.push("--user".into());
            args.push("1000:1000".into());
        }
        if !s.network {
            args.push("--network".into());
            args.push("none".into());
        }
        args.push("-v".into());
        args.push(format!("{}:{SANDBOX_MOUNT}", work.display()));
        args.push("-w".into());
        args.push(SANDBOX_MOUNT.into());
        for (k, v) in container_env() {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push(s.image.clone());
        args.push("sleep".into());
        // The container expires on its own if the app crashes and never cleans up.
        args.push("86400".into());

        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, _, stderr) = self.docker(&refs, Duration::from_secs(120))?;
        if code != 0 {
            bail!("could not start the sandbox container: {}", stderr.trim());
        }
        Ok(())
    }

    /// Stops and removes the container. Safe to call when it does not exist.
    pub fn shutdown(&self) {
        let _guard = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let _ = self.docker(&["rm", "-f", &self.container], Duration::from_secs(30));
    }
}

impl Sandbox for DockerSandbox {
    fn exec(&self, req: &ExecRequest, cancel: &AtomicBool) -> Result<ExecResult> {
        if req.command.trim().is_empty() {
            bail!("the command is empty");
        }
        let ws = Workspace::new(&self.work_dir)?;
        let cwd_abs = ws.resolve(&req.cwd)?;
        if !cwd_abs.is_dir() {
            bail!("working directory '{}' does not exist", req.cwd);
        }
        let cwd_rel = ws.relative(&cwd_abs);
        let container_cwd = if cwd_rel.is_empty() {
            SANDBOX_MOUNT.to_string()
        } else {
            format!("{SANDBOX_MOUNT}/{cwd_rel}")
        };

        self.ensure_ready()?;

        let secs = req.timeout_secs.clamp(1, 3600);
        let started = Instant::now();
        let mut cmd = base_command(&self.docker);
        cmd.args(["exec", "-w", &container_cwd, &self.container])
            .args(["timeout", "-k", "5", &format!("{secs}s")])
            .args([
                "bash",
                "-c",
                "exec 2>&1; eval \"$1\"",
                "gagent-shell",
                &req.command,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("cannot start `docker exec`")?;

        let capture = Arc::new(Mutex::new(Capture::default()));
        let mut readers = Vec::new();
        if let Some(p) = child.stdout.take() {
            readers.push(pump(Box::new(p), capture.clone()));
        }
        if let Some(p) = child.stderr.take() {
            readers.push(pump(Box::new(p), capture.clone()));
        }

        let hard_deadline = started + Duration::from_secs(u64::from(secs) + 20);
        let mut cancelled = false;
        let mut watchdog = false;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break None;
            }
            if Instant::now() >= hard_deadline {
                watchdog = true;
                break None;
            }
            thread::sleep(Duration::from_millis(40));
        };
        if status.is_none() {
            kill_child(&mut child);
            // The process inside the container may still be running; start fresh next time.
            let _ = run_quick(
                &self.docker,
                &["rm", "-f", &self.container],
                Duration::from_secs(20),
            );
        }
        for r in readers {
            let _ = r.join();
        }

        let (output, truncated) =
            std::mem::take(&mut *capture.lock().unwrap_or_else(|p| p.into_inner())).finish();
        let exit_code = status.and_then(|s| s.code());
        let timed_out = watchdog
            || exit_code == Some(124)
            || (exit_code == Some(137)
                && started.elapsed() >= Duration::from_secs(u64::from(secs)));
        Ok(ExecResult {
            exit_code: if cancelled || watchdog {
                None
            } else {
                exit_code
            },
            output,
            timed_out,
            cancelled,
            truncated,
            duration: started.elapsed(),
        })
    }

    fn reset(&self) -> Result<()> {
        self.shutdown();
        Ok(())
    }
}

fn pump(mut pipe: Box<dyn Read + Send>, capture: Arc<Mutex<Capture>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => capture
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(&buf[..n]),
            }
        }
    })
}

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Environment variables set inside the container.
fn container_env() -> Vec<(&'static str, String)> {
    let home = format!("{SANDBOX_MOUNT}/.gagent/home");
    vec![
        ("HOME", home.clone()),
        ("USER", "agent".into()),
        ("LOGNAME", "agent".into()),
        ("TERM", "dumb".into()),
        ("CI", "true".into()),
        ("DEBIAN_FRONTEND", "noninteractive".into()),
        ("PYTHONUNBUFFERED", "1".into()),
        ("PYTHONDONTWRITEBYTECODE", "1".into()),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1".into()),
        ("PIP_BREAK_SYSTEM_PACKAGES", "1".into()),
        ("NPM_CONFIG_UPDATE_NOTIFIER", "false".into()),
        ("NPM_CONFIG_PREFIX", format!("{home}/.npm-global")),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        (
            "PATH",
            format!("{home}/.local/bin:{home}/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        ),
    ]
}

/// The uid and gid of the current user, so files written in the sandbox are
/// owned by the user on Linux. `None` on Windows and macOS, where the sandbox
/// uses an unprivileged 1000:1000 instead.
fn host_user() -> Option<(u32, u32)> {
    #[cfg(target_os = "linux")]
    {
        let uid = run_quick("id", &["-u"], Duration::from_secs(5))
            .ok()?
            .1
            .trim()
            .parse()
            .ok()?;
        let gid = run_quick("id", &["-g"], Duration::from_secs(5))
            .ok()?
            .1
            .trim()
            .parse()
            .ok()?;
        Some((uid, gid))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Builds the sandbox image from the embedded Dockerfile, streaming output to `progress`.
pub fn build_image(docker: &str, image: &str, progress: &Progress) -> Result<()> {
    let mut cmd = base_command(docker);
    cmd.args(["build", "--progress=plain", "-t", image]);
    if let Ok(base) = std::env::var("GAGENT_SANDBOX_BASE_IMAGE") {
        if !base.trim().is_empty() {
            cmd.args(["--build-arg", &format!("BASE_IMAGE={}", base.trim())]);
        }
    }
    cmd.arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!("Docker is not installed or not on PATH")
        } else {
            anyhow!(e)
        }
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(SANDBOX_DOCKERFILE.as_bytes())?;
    }
    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let mut handles = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tail = tail.clone();
        let progress = progress.clone();
        handles.push(thread::spawn(move || {
            let reader = std::io::BufReader::new(pipe);
            for line in std::io::BufRead::lines(reader).map_while(Result::ok) {
                let line = strip_ansi(&line);
                if line.trim().is_empty() {
                    continue;
                }
                progress(&line);
                let mut t = tail.lock().unwrap_or_else(|p| p.into_inner());
                t.push_back(line);
                if t.len() > 25 {
                    t.pop_front();
                }
            }
        }));
    }
    let status = child.wait()?;
    for h in handles {
        let _ = h.join();
    }
    if !status.success() {
        let tail = tail.lock().unwrap_or_else(|p| p.into_inner());
        let lines: Vec<String> = tail.iter().cloned().collect();
        bail!("building the sandbox image failed:\n{}", lines.join("\n"));
    }
    Ok(())
}

/// Removes every container created by Gagent (used by `gagent-cli doctor --clean`).
pub fn remove_all_gagent_containers() -> Result<usize> {
    let docker = docker_binary();
    let (code, out, err) = run_quick(
        &docker,
        &["ps", "-aq", "--filter", "label=gagent=1"],
        Duration::from_secs(30),
    )?;
    if code != 0 {
        bail!("{}", err.trim());
    }
    let ids: Vec<&str> = out.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut args = vec!["rm", "-f"];
    args.extend(ids.iter().copied());
    run_quick(&docker, &args, Duration::from_secs(60))?;
    Ok(ids.len())
}

/// A sandbox that always fails with a fixed explanation. Used when Docker is
/// not available so the agent can still read and edit files.
pub struct UnavailableSandbox {
    pub reason: String,
}

impl Sandbox for UnavailableSandbox {
    fn exec(&self, _req: &ExecRequest, _cancel: &AtomicBool) -> Result<ExecResult> {
        Err(anyhow!("the sandbox is not available: {}", self.reason))
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }
}

/// A sandbox that replays prepared results. Intended for tests.
pub struct ScriptedSandbox {
    results: Mutex<VecDeque<ExecResult>>,
    pub commands: Mutex<Vec<String>>,
}

impl ScriptedSandbox {
    pub fn new(results: Vec<ExecResult>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            commands: Mutex::new(Vec::new()),
        }
    }
}

impl Sandbox for ScriptedSandbox {
    fn exec(&self, req: &ExecRequest, _cancel: &AtomicBool) -> Result<ExecResult> {
        self.commands.lock().unwrap().push(req.command.clone());
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("no scripted result left"))
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }
}

/// Creates a unique container-safe session label.
pub fn new_sandbox_id() -> String {
    unique_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut c = Capture::default();
        let chunk = vec![b'a'; 100_000];
        for _ in 0..30 {
            c.push(&chunk);
        }
        c.push(b"THE-END");
        let (text, truncated) = c.finish();
        assert!(truncated);
        assert!(text.contains("bytes of output dropped"));
        assert!(text.ends_with("THE-END"));
        assert!(text.len() < CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES + 200);
    }

    #[test]
    fn capture_small_output_is_untouched() {
        let mut c = Capture::default();
        c.push(b"hello\n");
        c.push(b"world\n");
        let (text, truncated) = c.finish();
        assert_eq!(text, "hello\nworld\n");
        assert!(!truncated);
    }

    #[test]
    fn dockerfile_is_embedded() {
        assert!(SANDBOX_DOCKERFILE.contains("ARG BASE_IMAGE"));
        assert!(SANDBOX_DOCKERFILE.contains("FROM"));
    }

    #[test]
    fn status_descriptions_are_helpful() {
        assert!(DockerStatus::NotInstalled
            .describe()
            .contains("Install Docker"));
        assert!(DockerStatus::NotRunning("x".into())
            .describe()
            .contains("not running"));
        assert!(DockerStatus::Ready {
            version: "1".into(),
            image_present: false
        }
        .describe()
        .contains("not built"));
    }

    #[test]
    fn unavailable_sandbox_explains_itself() {
        let s = UnavailableSandbox {
            reason: "no docker".into(),
        };
        let err = s
            .exec(
                &ExecRequest {
                    command: "ls".into(),
                    cwd: "".into(),
                    timeout_secs: 5,
                },
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no docker"));
    }

    #[test]
    fn scripted_sandbox_replays() {
        let s = ScriptedSandbox::new(vec![ExecResult {
            exit_code: Some(3),
            output: "x".into(),
            ..Default::default()
        }]);
        let r = s
            .exec(
                &ExecRequest {
                    command: "a".into(),
                    cwd: "".into(),
                    timeout_secs: 5,
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(s.commands.lock().unwrap().as_slice(), ["a"]);
    }
}
