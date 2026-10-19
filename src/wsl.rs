//! Running the agent's commands in WSL (Windows Subsystem for Linux).
//!
//! Docker Desktop needs several gigabytes of disk and a lot of memory. A WSL
//! Ubuntu distribution is a small Linux system that many Windows machines
//! already have: commands run in it with `wsl.exe`, in the agent's private copy
//! of the project, and the results come back as text.
//!
//! Unlike the Docker sandbox this is not isolated: Linux commands can reach your
//! Windows drives under `/mnt/c`. The command runs with a clean environment
//! (no Windows variables, no API keys, no Windows programs on `PATH`) and the
//! agent is told to stay inside the workspace.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::sandbox::{
    base_command, container_env_for, kill_child, pump, run_quick_bytes, Capture, DockerStatus,
    ExecRequest, ExecResult, Sandbox,
};
use crate::util::unique_id;
use crate::workspace::Workspace;

/// Name of the WSL launcher (overridable for tests).
pub fn wsl_binary() -> String {
    std::env::var("GAGENT_WSL")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "wsl.exe".into())
}

/// Decodes text printed by `wsl.exe`, which is UTF-16 for its own messages and
/// UTF-8 for output of Linux programs.
pub fn decode_wsl_text(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2 && bytes.iter().skip(1).step_by(2).any(|b| *b == 0);
    if looks_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Splits the output of `wsl -l -q` into distribution names.
pub fn parse_distro_list(bytes: &[u8]) -> Vec<String> {
    decode_wsl_text(bytes)
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}').to_string())
        // Distribution names never contain spaces; sentences are WSL's messages.
        .filter(|l| !l.is_empty() && !l.contains(' '))
        .collect()
}

/// Distributions that Docker Desktop keeps for itself are not for running code.
fn is_internal(name: &str) -> bool {
    name.starts_with("docker-desktop")
}

fn list(args: &[&str]) -> Result<Vec<String>, String> {
    match run_quick_bytes(&wsl_binary(), args, Duration::from_secs(20)) {
        Ok((0, out, _)) => Ok(parse_distro_list(&out)),
        // `wsl --list` fails with a message when no distribution is installed.
        Ok(_) => Ok(Vec::new()),
        Err(e) => match e.downcast_ref::<std::io::Error>() {
            Some(io) if io.kind() == std::io::ErrorKind::NotFound => {
                Err("WSL is not installed on this computer.".into())
            }
            _ => Err(e.to_string()),
        },
    }
}

/// Checks whether `distro` (empty: the default one) can run commands.
///
/// This does not start the WSL virtual machine, so checking is cheap and does
/// not use memory. `Err` carries a sentence that says what is missing.
pub fn wsl_status(distro: &str) -> Result<DockerStatus, String> {
    if !cfg!(windows) && std::env::var_os("GAGENT_WSL").is_none() {
        return Err("WSL exists only on Windows.".into());
    }
    let all = list(&["-l", "-q"])?;
    let usable: Vec<&String> = all.iter().filter(|d| !is_internal(d)).collect();
    let chosen = if distro.trim().is_empty() {
        usable.first().map(|d| d.to_string())
    } else {
        usable
            .iter()
            .find(|d| d.eq_ignore_ascii_case(distro.trim()))
            .map(|d| d.to_string())
    };
    let Some(chosen) = chosen else {
        return Err(if distro.trim().is_empty() {
            "WSL has no Linux distribution. Install Ubuntu with `wsl --install -d Ubuntu` (about 1 GB).".into()
        } else {
            format!("The WSL distribution '{}' does not exist.", distro.trim())
        });
    };
    // Python is only looked for when the distribution is already running; asking
    // otherwise would start the virtual machine just to draw a status line.
    let running = list(&["-l", "--running", "-q"]).unwrap_or_default();
    let python = if running.iter().any(|d| d.eq_ignore_ascii_case(&chosen)) {
        match run_quick_bytes(
            &wsl_binary(),
            &["-d", &chosen, "--exec", "sh", "-c", "command -v python3"],
            Duration::from_secs(20),
        ) {
            Ok((code, _, _)) => code == 0,
            Err(_) => true,
        }
    } else {
        true
    };
    Ok(DockerStatus::Wsl {
        distro: chosen,
        python,
    })
}

/// Ubuntu only has `python3` and `python3 -m pip`. Models (and many scripts) say
/// `python` and `pip`, so small wrappers in the sandbox home provide them.
fn install_shims(home_win: &Path) {
    let bin = home_win.join(".local").join("bin");
    if std::fs::create_dir_all(&bin).is_err() {
        return;
    }
    // Without root, `sudo` would wait for a password that nobody can type, and the
    // command hangs until its time limit. These stubs fail at once with an explanation.
    const NO_ROOT: &str = "#!/bin/sh\necho \"$(basename \"$0\"): not available in this sandbox. There is no root access and system packages cannot be installed. Use what is installed, or tell the user which package is needed.\" >&2\nexit 1\n";
    for (name, body) in [
        ("python", "#!/bin/sh\nexec python3 \"$@\"\n"),
        ("pip", "#!/bin/sh\nexec python3 -m pip \"$@\"\n"),
        ("sudo", NO_ROOT),
        ("apt", NO_ROOT),
        ("apt-get", NO_ROOT),
    ] {
        let path = bin.join(name);
        if !path.exists() {
            let _ = std::fs::write(&path, body);
        }
    }
}

/// How long the Linux VM is kept running after the last command.
const KEEP_WARM: Duration = Duration::from_secs(8 * 60);

/// State shared with the thread that releases the VM when it is no longer needed.
struct Warmth {
    /// A `sleep` process in WSL. While it runs, WSL does not switch itself off, so the
    /// next command does not pay the 9 second cold start (WSL stops itself about
    /// 25 seconds after the last process ends, and a model often thinks longer).
    holder: Mutex<Option<std::process::Child>>,
    last_use: Mutex<Instant>,
}

impl Warmth {
    fn release(&self) {
        let mut holder = self.holder.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut child) = holder.take() {
            kill_child(&mut child);
        }
    }
}

impl Drop for Warmth {
    fn drop(&mut self) {
        self.release();
    }
}

/// Runs commands in a WSL distribution.
pub struct WslSandbox {
    distro: String,
    work_dir: std::path::PathBuf,
    run_id: String,
    counter: AtomicU64,
    /// `HOME` inside Linux, derived once from the workspace path.
    home: Mutex<Option<String>>,
    /// The real home directory of the Linux user (where `~/.cargo` and `~/.rustup` live).
    real_home: Mutex<Option<Option<String>>>,
    warmth: Arc<Warmth>,
}

impl WslSandbox {
    pub fn new(distro: &str, work_dir: &Path) -> Self {
        Self {
            distro: distro.trim().to_string(),
            work_dir: work_dir.to_path_buf(),
            run_id: unique_id(),
            counter: AtomicU64::new(0),
            home: Mutex::new(None),
            real_home: Mutex::new(None),
            warmth: Arc::new(Warmth {
                holder: Mutex::new(None),
                last_use: Mutex::new(Instant::now()),
            }),
        }
    }

    /// True while the VM is being kept running for this sandbox.
    pub fn is_warm(&self) -> bool {
        let mut holder = self.warmth.holder.lock().unwrap_or_else(|p| p.into_inner());
        match holder.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Marks the sandbox as in use and makes sure the VM stays up.
    fn keep_warm(&self) {
        *self
            .warmth
            .last_use
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Instant::now();
        if self.is_warm() {
            return;
        }
        let mut cmd = base_command(&wsl_binary());
        cmd.args(self.distro_args())
            .args(["--exec", "sleep", "3600"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(child) = cmd.spawn() else {
            return;
        };
        *self.warmth.holder.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
        // Gives the VM back after a while without commands, so that an open window
        // does not hold more than a gigabyte of memory all day.
        let weak = Arc::downgrade(&self.warmth);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(20));
            let Some(warmth) = weak.upgrade() else {
                return;
            };
            let idle = warmth
                .last_use
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .elapsed();
            if idle > KEEP_WARM {
                warmth.release();
                return;
            }
            if warmth
                .holder
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none()
            {
                return;
            }
        });
    }

    fn distro_args(&self) -> Vec<String> {
        if self.distro.is_empty() {
            Vec::new()
        } else {
            vec!["-d".into(), self.distro.clone()]
        }
    }

    /// Linux path of the sandbox home directory (inside the workspace).
    fn linux_home(&self, windows_home: &Path) -> Result<String> {
        let mut cached = self.home.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = cached.as_ref() {
            return Ok(h.clone());
        }
        let mut args = self.distro_args();
        args.extend(["--exec".into(), "wslpath".into(), "-a".into(), "-u".into()]);
        args.push(windows_home.to_string_lossy().into_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, err) = run_quick_bytes(&wsl_binary(), &refs, Duration::from_secs(90))
            .context("cannot start WSL")?;
        let text = decode_wsl_text(&out);
        if code != 0 || text.trim().is_empty() {
            bail!(
                "WSL could not translate the workspace path: {}",
                decode_wsl_text(&err).trim()
            );
        }
        let home = text.trim().to_string();
        *cached = Some(home.clone());
        Ok(home)
    }

    /// The Linux user's real home directory, found once.
    fn real_home(&self) -> Option<String> {
        let mut cached = self.real_home.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(found) = cached.as_ref() {
            return found.clone();
        }
        let mut args = self.distro_args();
        args.extend([
            "--exec".into(),
            "sh".into(),
            "-c".into(),
            "printf %s \"$HOME\"".into(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let found = match run_quick_bytes(&wsl_binary(), &refs, Duration::from_secs(60)) {
            Ok((0, out, _)) => {
                let text = decode_wsl_text(&out).trim().to_string();
                (text.starts_with('/') && text.len() > 1).then_some(text)
            }
            _ => None,
        };
        *cached = Some(found.clone());
        found
    }

    /// Stops the commands started by this sandbox that are still running.
    fn kill_marked(&self, marker: &str) {
        let mut args = self.distro_args();
        args.extend([
            "--exec".into(),
            "pkill".into(),
            "-TERM".into(),
            "-f".into(),
            marker.to_string(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = run_quick_bytes(&wsl_binary(), &refs, Duration::from_secs(20));
    }
}

impl Sandbox for WslSandbox {
    fn exec(&self, req: &ExecRequest, cancel: &AtomicBool) -> Result<ExecResult> {
        if req.command.trim().is_empty() {
            bail!("the command is empty");
        }
        let ws = Workspace::new(&self.work_dir)?;
        let cwd_abs = ws.resolve(&req.cwd)?;
        if !cwd_abs.is_dir() {
            bail!("working directory '{}' does not exist", req.cwd);
        }
        let home_win = ws.root().join(".gagent").join("home");
        std::fs::create_dir_all(&home_win).context("cannot create the sandbox home directory")?;
        install_shims(&home_win);
        self.keep_warm();
        let home = self.linux_home(&home_win)?;

        let secs = req.timeout_secs.clamp(1, 3600);
        let marker = format!(
            "gagent-run-{}-{}",
            self.run_id,
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let started = Instant::now();
        let mut cmd = base_command(&wsl_binary());
        cmd.args(self.distro_args());
        cmd.arg("--cd").arg(&cwd_abs);
        cmd.args(["--exec", "env", "-i"]);
        let mut env = container_env_for(&home);
        // The commands run with a workspace-local HOME, so the Rust toolchain installed
        // for the Linux user (rustup, cargo) must be pointed at explicitly. Cargo's
        // download cache stays in the workspace home; credentials are not shared.
        if let Some(real) = self.real_home() {
            for (k, v) in env.iter_mut() {
                if *k == "PATH" {
                    v.push_str(&format!(":{real}/.cargo/bin"));
                }
            }
            env.push(("RUSTUP_HOME", format!("{real}/.rustup")));
            env.push(("CARGO_HOME", format!("{home}/.cargo")));
        }
        for (k, v) in env {
            cmd.arg(format!("{k}={v}"));
        }
        cmd.args(["timeout", "-k", "5", &format!("{secs}s")])
            // The marker is only an extra argument, so `pkill -f` can find the process.
            // `$0` stays `gagent-shell`, which is what shows up in shell error messages.
            .args([
                "bash",
                "-c",
                "exec 2>&1; (cd \"$HOME/.local/bin\" 2>/dev/null && chmod +x python pip sudo apt apt-get 2>/dev/null); eval \"$1\"",
                "gagent-shell",
                &req.command,
                &marker,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("cannot start wsl.exe")?;

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
            // Killing wsl.exe does not stop the Linux process; ask Linux to.
            self.kill_marked(&marker);
        }
        for r in readers {
            let _ = r.join();
        }

        let (output, truncated) =
            std::mem::take(&mut *capture.lock().unwrap_or_else(|p| p.into_inner())).finish();
        // wsl.exe's own error messages are UTF-16; with the NUL bytes removed they read fine.
        let output = output.replace('\0', "");
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
        self.warmth.release();
        // Only talk to WSL when the distribution is already running: stopping
        // leftovers must not start the virtual machine.
        let running = list(&["-l", "--running", "-q"]).unwrap_or_default();
        let target = if self.distro.is_empty() {
            running.iter().any(|d| !is_internal(d))
        } else {
            running.iter().any(|d| d.eq_ignore_ascii_case(&self.distro))
        };
        if target {
            self.kill_marked(&format!("gagent-run-{}-", self.run_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn utf16_messages_are_decoded() {
        assert_eq!(decode_wsl_text(&utf16("Ubuntu\r\n")), "Ubuntu\r\n");
        assert_eq!(decode_wsl_text(b"plain output"), "plain output");
        let mut with_bom = vec![0xff, 0xfe];
        with_bom.extend(utf16("Ubuntu"));
        assert_eq!(decode_wsl_text(&with_bom), "Ubuntu");
    }

    #[test]
    fn distro_lists_are_parsed() {
        let out = utf16("Ubuntu\r\ndocker-desktop\r\nDebian\r\n\r\n");
        assert_eq!(
            parse_distro_list(&out),
            vec!["Ubuntu", "docker-desktop", "Debian"]
        );
        assert!(parse_distro_list(b"").is_empty());
        assert!(is_internal("docker-desktop-data"));
        assert!(!is_internal("Ubuntu-24.04"));
    }
}
