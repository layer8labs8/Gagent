//! Git around the agent's work: a safety checkpoint before the agent's changes reach the project,
//! the difference the agent made, and a commit of exactly the files the agent changed.
//!
//! Uses the `git` program of the computer (no library), and only when the project is a git
//! repository. The checkpoint does not touch the branch, the index or the files: it is a commit
//! object kept under `refs/gagent/`, so `git branch` and `git log` do not show it.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::util::now_secs;

/// The most characters of a diff that are kept.
const MAX_DIFF_CHARS: usize = 60_000;
/// Checkpoints kept per project.
const KEEP_CHECKPOINTS: usize = 20;

/// What the window shows about the repository of a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitInfo {
    pub branch: String,
    /// Files with changes that are not committed (paths relative to the project).
    pub changed: Vec<String>,
}

#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_window(_command: &mut Command) {}

/// Runs git in `dir`: the exit code, what it printed, and what it complained about.
fn run_raw(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<(i32, String, String), String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    no_window(&mut command);
    let out = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git is not installed on this computer".to_string()
        } else {
            format!("could not run git: {e}")
        }
    })?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
    ))
}

/// Runs git and returns what it printed; a failure is its first line of complaint.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let (code, out, err) = run_raw(dir, args, &[])?;
    if code == 0 {
        Ok(out)
    } else {
        let why = err
            .lines()
            .chain(out.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("git failed");
        Err(why.to_string())
    }
}

/// True when `dir` is inside a git repository.
pub fn is_repo(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true")
}

/// Saves the present state of the project before the agent's changes reach it. Returns the short
/// hash of the checkpoint, or `None` when the project is not a git repository (or git is missing).
/// A project with uncommitted work is saved as it is (tracked files); a clean one is saved as its
/// last commit.
pub fn checkpoint(dir: &Path) -> Option<String> {
    if !is_repo(dir) {
        return None;
    }
    // `stash create` makes a commit of the working tree without changing anything; nothing printed
    // means there was nothing to save. It needs a name for the commit, which is given here.
    let identity = [
        ("GIT_AUTHOR_NAME", "Gagent"),
        ("GIT_AUTHOR_EMAIL", "gagent@localhost"),
        ("GIT_COMMITTER_NAME", "Gagent"),
        ("GIT_COMMITTER_EMAIL", "gagent@localhost"),
    ];
    let saved = run_raw(dir, &["stash", "create", "Gagent checkpoint"], &identity)
        .ok()
        .filter(|(code, out, _)| *code == 0 && !out.is_empty())
        .map(|(_, out, _)| out);
    let hash = match saved {
        Some(h) => h,
        None => run(dir, &["rev-parse", "HEAD"]).ok()?,
    };
    let name = format!("refs/gagent/checkpoint-{}", now_secs());
    run(dir, &["update-ref", &name, &hash]).ok()?;
    prune(dir);
    Some(hash.chars().take(8).collect())
}

/// Keeps the newest checkpoints only.
fn prune(dir: &Path) {
    let Ok(list) = run(dir, &["for-each-ref", "--format=%(refname)", "refs/gagent/"]) else {
        return;
    };
    let mut names: Vec<&str> = list.lines().collect();
    names.sort_unstable();
    while names.len() > KEEP_CHECKPOINTS {
        let old = names.remove(0);
        let _ = run(dir, &["update-ref", "-d", old]);
    }
}

/// The branch and the uncommitted files of the repository, or `None` outside a repository.
pub fn info(dir: &Path) -> Option<GitInfo> {
    if !is_repo(dir) {
        return None;
    }
    let branch = run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "(no commit yet)".to_string());
    let status = run_raw(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=all"], &[])
        .ok()
        .filter(|(code, _, _)| *code == 0)
        .map(|(_, out, _)| out)
        .unwrap_or_default();
    Some(GitInfo {
        branch,
        changed: parse_status(&status),
    })
}

/// The paths in the output of `git status --porcelain=v1 -z` (entries end with a NUL; a rename
/// is followed by its old name).
fn parse_status(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        if code.starts_with('R') || code.starts_with('C') || code.as_bytes().get(1) == Some(&b'R') {
            entries.next();
        }
        files.push(path.to_string());
    }
    files
}

/// What `files` look like compared with the last commit (or with `base`), also for new files
/// that git does not know yet.
pub fn diff(dir: &Path, files: &[String], base: Option<&str>) -> Result<String, String> {
    if files.is_empty() {
        return Err("There are no files to show.".into());
    }
    let base = base.unwrap_or("HEAD");
    let names: Vec<&str> = files.iter().map(String::as_str).collect();
    let mut out = String::new();

    let mut args = vec!["diff", "--no-color", "--no-ext-diff", base, "--"];
    args.extend(&names);
    let tracked = run(dir, &args);
    if let Ok(text) = &tracked {
        out.push_str(text);
    }

    let mut args = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(&names);
    let untracked = run(dir, &args).unwrap_or_default();
    for file in untracked.lines().filter(|l| !l.trim().is_empty()) {
        // Exit code 1 only says that the files differ.
        if let Ok((_, text, _)) = run_raw(
            dir,
            &["diff", "--no-color", "--no-index", "--", "/dev/null", file],
            &[],
        ) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text);
        }
    }
    if out.trim().is_empty() {
        return match tracked {
            Err(e) => Err(e),
            Ok(_) => Ok("No differences.".to_string()),
        };
    }
    if out.chars().count() > MAX_DIFF_CHARS {
        let cut: String = out.chars().take(MAX_DIFF_CHARS).collect();
        out = format!("{cut}\n[the rest of the diff is not shown]");
    }
    Ok(out)
}

/// Commits `files` (and nothing else of the working tree) with `message`; returns the short hash.
pub fn commit(dir: &Path, files: &[String], message: &str) -> Result<String, String> {
    if files.is_empty() {
        return Err("There are no files to commit.".into());
    }
    if message.trim().is_empty() {
        return Err("Write a short message for the commit.".into());
    }
    let names: Vec<&str> = files.iter().map(String::as_str).collect();
    let mut add = vec!["add", "-A", "--"];
    add.extend(&names);
    run(dir, &add)?;
    let mut commit = vec!["commit", "-q", "-m", message.trim(), "--"];
    commit.extend(&names);
    run(dir, &commit)?;
    run(dir, &["rev-parse", "--short", "HEAD"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git_here() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]).unwrap();
        run(dir.path(), &["config", "user.email", "me@example.com"]).unwrap();
        run(dir.path(), &["config", "user.name", "Me"]).unwrap();
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        fs::write(dir.path().join("other.txt"), "mine\n").unwrap();
        run(dir.path(), &["add", "-A"]).unwrap();
        run(dir.path(), &["commit", "-q", "-m", "first"]).unwrap();
        dir
    }

    #[test]
    fn status_paths_are_read_from_the_nul_separated_output() {
        let text = " M a.txt\0?? new file.txt\0R  new.txt\0old.txt\0A  b.txt\0";
        assert_eq!(parse_status(text), vec!["a.txt", "new file.txt", "new.txt", "b.txt"]);
        assert!(parse_status("").is_empty());
    }

    #[test]
    fn a_checkpoint_keeps_the_work_of_the_user_and_touches_nothing() {
        if !git_here() {
            return;
        }
        let d = repo();
        let p = d.path();
        // A clean project is saved as its last commit.
        let clean = checkpoint(p).expect("a checkpoint of a clean project");
        assert_eq!(clean.len(), 8);
        assert_eq!(run(p, &["rev-parse", "--short=8", "HEAD"]).unwrap(), clean);

        // Work that is not committed is saved as it is, and stays where it is.
        fs::write(p.join("a.txt"), "two\n").unwrap();
        let before = run(p, &["status", "--porcelain"]).unwrap();
        let saved = checkpoint(p).unwrap();
        assert_ne!(saved, clean);
        assert_eq!(run(p, &["status", "--porcelain"]).unwrap(), before);
        assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "two\n");
        assert_eq!(run(p, &["show", &format!("{saved}:a.txt")]).unwrap(), "two");
        assert_eq!(run(p, &["log", "--oneline"]).unwrap().lines().count(), 1, "no new commit on the branch");
        assert!(run(p, &["for-each-ref", "refs/gagent/"]).unwrap().contains("checkpoint-"));

        // Outside a repository there is nothing to do.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(checkpoint(plain.path()), None);
        assert_eq!(info(plain.path()), None);
    }

    #[test]
    fn the_diff_and_the_commit_cover_only_the_files_of_the_agent() {
        if !git_here() {
            return;
        }
        let d = repo();
        let p = d.path();
        fs::write(p.join("a.txt"), "two\n").unwrap();
        fs::write(p.join("new file.txt"), "brand new\n").unwrap();
        fs::write(p.join("other.txt"), "mine, changed\n").unwrap();
        let agent = vec!["a.txt".to_string(), "new file.txt".to_string()];

        let g = info(p).unwrap();
        assert!(g.branch == "master" || g.branch == "main", "{}", g.branch);
        assert_eq!(g.changed.len(), 3, "{:?}", g.changed);

        let text = diff(p, &agent, None).unwrap();
        assert!(text.contains("+two") && text.contains("-one"), "{text}");
        assert!(text.contains("+brand new"), "{text}");
        assert!(!text.contains("mine, changed"), "{text}");

        let hash = commit(p, &agent, "Gagent: change a.txt").unwrap();
        assert!(!hash.is_empty());
        let shown = run(p, &["show", "--stat", "--format=%s", "HEAD"]).unwrap();
        assert!(shown.contains("Gagent: change a.txt") && shown.contains("a.txt") && shown.contains("new file.txt"), "{shown}");
        assert!(!shown.contains("other.txt"), "{shown}");
        // The user's own work is still waiting.
        assert_eq!(info(p).unwrap().changed, vec!["other.txt".to_string()]);

        assert!(commit(p, &[], "x").is_err());
        assert!(commit(p, &agent, "  ").is_err());
        assert!(commit(p, &["other.txt".to_string()], "second").is_ok());
        assert!(commit(p, &["other.txt".to_string()], "nothing left").is_err());
    }
}
