//! Running the agent's commands directly on the user's own computer.
//!
//! No virtual machine, no container, nothing to install: bash (Git Bash on Windows)
//! starts in the agent's private copy of the project, with low priority so that the
//! computer stays usable. It is the fastest and lightest way to test code. It is not
//! isolated, so [`crate::policy`] refuses the commands that could do real harm, and the
//! environment is cleaned of anything that looks like a key or a password.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::sandbox::{
    base_command, find_on_path, pump, run_quick, Capture, DockerStatus, ExecRequest, ExecResult,
    Sandbox,
};
use crate::workspace::Workspace;

/// Finds a bash that can run the agent's commands.
pub fn find_shell() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("GAGENT_SHELL").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    candidates().into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    // `C:\Windows\System32\bash.exe` is the launcher of WSL, not a shell for this.
    let mut out = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            out.push(base.join(r"Git\bin\bash.exe"));
            out.push(base.join(r"Git\usr\bin\bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(PathBuf::from(local).join(r"Programs\Git\bin\bash.exe"));
    }
    // Next to git.exe: <git>\cmd\git.exe -> <git>\bin\bash.exe
    if let Some(git) = find_on_path("git") {
        if let Some(root) = git.parent().and_then(Path::parent) {
            out.push(root.join(r"bin\bash.exe"));
        }
    }
    out
}

#[cfg(not(windows))]
fn candidates() -> Vec<PathBuf> {
    [
        "/bin/bash",
        "/usr/bin/bash",
        "/usr/local/bin/bash",
        "/opt/homebrew/bin/bash",
        "/bin/sh",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

fn shell_label(shell: &Path) -> String {
    let name = if cfg!(windows) { "Git Bash" } else { "bash" };
    match run_quick(&shell.to_string_lossy(), &["--version"], Duration::from_secs(10)) {
        Ok((0, out, _)) => {
            let first = out.lines().next().unwrap_or("");
            // "GNU bash, version 5.2.37(1)-release (x86_64-pc-msys)" -> "5.2.37"
            match first.split("version ").nth(1) {
                Some(rest) => format!(
                    "{name} {}",
                    rest.split(|c: char| !(c.is_ascii_digit() || c == '.')).next().unwrap_or("")
                ),
                None => name.to_string(),
            }
        }
        _ => name.to_string(),
    }
}

/// Checks whether commands can run on this computer.
pub fn host_status() -> Result<DockerStatus, String> {
    match find_shell() {
        Some(shell) => Ok(DockerStatus::Host {
            shell: shell_label(&shell),
        }),
        None if cfg!(windows) => Err(
            "Git for Windows (it brings bash) was not found. Install it from https://git-scm.com, or use WSL or Docker."
                .to_string(),
        ),
        None => Err("bash was not found on this computer.".to_string()),
    }
}

/// Variables that look like keys or passwords are not handed to the agent's commands.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("GAGENT_")
        || [
            "KEY",
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "CREDENTIAL",
            "AUTH",
            "COOKIE",
            "SESSION",
        ]
        .iter()
        .any(|w| upper.contains(w))
}

/// Small stand-ins in the sandbox home: `sudo` and `apt` fail at once instead of
/// waiting for a password, and on Windows `python3`/`pip3` are the Windows `python`.
fn install_shims(home: &Path) {
    let bin = home.join(".local").join("bin");
    if std::fs::create_dir_all(&bin).is_err() {
        return;
    }
    const NO_ROOT: &str = "#!/bin/sh\necho \"$(basename \"$0\"): not available here. There is no administrator access; use what is installed or tell the user what is needed.\" >&2\nexit 1\n";
    let mut shims: Vec<(&str, &str)> = vec![
        ("sudo", NO_ROOT),
        ("apt", NO_ROOT),
        ("apt-get", NO_ROOT),
    ];
    if cfg!(windows) {
        shims.push(("python3", "#!/bin/sh\nexec python \"$@\"\n"));
        shims.push(("pip3", "#!/bin/sh\nexec python -m pip \"$@\"\n"));
    }
    for (name, body) in shims {
        let path = bin.join(name);
        if path.exists() {
            continue;
        }
        if std::fs::write(&path, body).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
}

/// The environment of a command: the user's own (so that python, node, git are found),
/// without secrets, with the sandbox home first on `PATH`.
fn apply_env(cmd: &mut Command, home: &Path) {
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        if is_secret_name(&k.to_string_lossy()) {
            continue;
        }
        cmd.env(&k, &v);
    }
    let bin = home.join(".local").join("bin");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path)))
        .unwrap_or(path);
    cmd.env("PATH", joined);
    cmd.env("HOME", home);
    for (k, v) in [
        ("TERM", "dumb"),
        ("CI", "true"),
        ("PYTHONUNBUFFERED", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONUTF8", "1"),
        ("PYTHONIOENCODING", "utf-8"),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
        ("NPM_CONFIG_UPDATE_NOTIFIER", "false"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("DEBIAN_FRONTEND", "noninteractive"),
    ] {
        cmd.env(k, v);
    }
}

/// Stops a command and everything it started (while the shell is still the parent).
fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = run_quick(
            "taskkill",
            &["/PID", &child.id().to_string(), "/T", "/F"],
            Duration::from_secs(10),
        );
    }
    #[cfg(unix)]
    {
        let _ = run_quick(
            "kill",
            &["-KILL", "--", &format!("-{}", child.id())],
            Duration::from_secs(5),
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs commands with bash on this computer. A tool that is not installed here (`cargo`
/// on a PC without Rust) simply fails with "command not found", which the agent reports;
/// nothing heavy is started behind the user's back.
pub struct HostSandbox {
    shell: PathBuf,
    work_dir: PathBuf,
}

impl HostSandbox {
    pub fn new(shell: &Path, work_dir: &Path) -> Self {
        Self {
            shell: shell.to_path_buf(),
            work_dir: work_dir.to_path_buf(),
        }
    }
}

impl Sandbox for HostSandbox {
    fn needs_policy(&self) -> bool {
        true
    }

    fn exec(&self, req: &ExecRequest, cancel: &AtomicBool) -> Result<ExecResult> {
        if req.command.trim().is_empty() {
            bail!("the command is empty");
        }
        let ws = Workspace::new(&self.work_dir)?;
        let cwd_abs = ws.resolve(&req.cwd)?;
        if !cwd_abs.is_dir() {
            bail!("working directory '{}' does not exist", req.cwd);
        }
        // A home of its own: the stand-in scripts of the WSL sandbox (`python` calling
        // `python3`) must never meet the ones made here (`python3` calling `python`).
        let home = ws.root().join(".gagent").join("home-host");
        std::fs::create_dir_all(&home).context("cannot create the sandbox home directory")?;
        install_shims(&home);

        let secs = req.timeout_secs.clamp(1, 3600);
        let started = Instant::now();
        let shell = self.shell.to_string_lossy().into_owned();
        // On Unix the command also runs at a lower priority (`nice`); Windows does it
        // with a process priority class below.
        let mut cmd = match find_on_path("nice").filter(|_| cfg!(unix)) {
            Some(nice) => {
                let mut c = base_command(&nice.to_string_lossy());
                c.args(["-n", "10", &shell]);
                c
            }
            None => base_command(&shell),
        };
        apply_env(&mut cmd, &home);
        cmd.current_dir(&cwd_abs)
            .args(["-c", "exec 2>&1; eval \"$1\"", "gagent-shell", &req.command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("cannot start bash")?;

        let capture = Arc::new(Mutex::new(Capture::default()));
        let mut readers = Vec::new();
        if let Some(p) = child.stdout.take() {
            readers.push(pump(Box::new(p), capture.clone()));
        }
        if let Some(p) = child.stderr.take() {
            readers.push(pump(Box::new(p), capture.clone()));
        }

        let deadline = started + Duration::from_secs(u64::from(secs));
        let (mut cancelled, mut timed_out) = (false, false);
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break None;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break None;
            }
            thread::sleep(Duration::from_millis(40));
        };
        if status.is_none() {
            kill_tree(&mut child);
        }
        // A background process that inherited the output pipes keeps them open after the
        // shell has ended; do not wait for it forever.
        let wait_until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < wait_until && !readers.iter().all(|r| r.is_finished()) {
            thread::sleep(Duration::from_millis(20));
        }
        for r in readers {
            if r.is_finished() {
                let _ = r.join();
            }
        }

        let (output, truncated) =
            std::mem::take(&mut *capture.lock().unwrap_or_else(|p| p.into_inner())).finish();
        let exit_code = status.and_then(|s| s.code());
        Ok(ExecResult {
            exit_code: if cancelled || timed_out { None } else { exit_code },
            output,
            timed_out,
            cancelled,
            truncated,
            duration: started.elapsed(),
        })
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_recognized_by_name() {
        for name in [
            "GEMINI_API_KEY",
            "NVIDIA_API_KEY",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "SSH_AUTH_SOCK",
            "GAGENT_HOME",
            "SESSION_MANAGER",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["PATH", "SystemRoot", "TEMP", "HOME", "LANG", "USERNAME", "PYTHONPATH"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }
}
