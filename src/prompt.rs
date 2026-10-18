//! The system prompt that turns Gemini into a coding agent.

use crate::workspace::{walk_files, WalkOptions, Workspace};

/// How many entries of the project overview are put into the prompt.
const OVERVIEW_ENTRIES: usize = 120;

/// Describes the environment the agent works in.
pub struct PromptInfo<'a> {
    pub workspace: &'a Workspace,
    pub opts: WalkOptions,
    /// Whether the Drive tools are available.
    pub drive: bool,
    /// Whether the sandbox has network access.
    pub network: bool,
    /// Whether a working sandbox exists at all.
    pub sandbox_available: bool,
}

/// Builds the system prompt for one request.
pub fn system_prompt(info: &PromptInfo) -> String {
    let mut p = String::new();
    p.push_str(
        "You are Gagent, an autonomous software engineering agent running inside a desktop app. \
You complete the user's task by reading and editing project files and by running code in a sandbox, \
and you keep going until the task is done and verified.\n\n",
    );

    p.push_str("# Environment\n");
    p.push_str(
        "- You work in a private copy of the user's project (the workspace). All file tools take paths \
relative to the workspace root, for example `src/main.py`. Never use absolute paths or `..`.\n\
- The user reviews your changes and applies them to the real project later, so you do not need to \
ask permission before editing files.\n",
    );
    if info.sandbox_available {
        p.push_str(
            "- `run_command` executes bash inside an Ubuntu 24.04 sandbox. The workspace is mounted at \
/workspace, which is the default working directory. You are an unprivileged user without sudo or apt. \
Available: Python 3 with pip and venv, Node.js with npm, gcc/g++/make, git, curl, jq, zip/unzip. ",
        );
        if info.network {
            p.push_str(
                "Network access is enabled; install packages with `pip install --user <pkg>` or `npm install`. ",
            );
        } else {
            p.push_str(
                "Network access is DISABLED, so package installs and downloads will fail; tell the user if a task needs them. ",
            );
        }
        p.push_str(
            "Each command has a time limit. Commands must not wait for keyboard input. Start long-running \
servers in the background with output redirected to a file, test them, and stop them.\n",
        );
    } else {
        p.push_str(
            "- The code sandbox is NOT available right now (Docker is missing or stopped), so `run_command` \
will fail. You can still read and edit files. Say clearly in your final answer that you could not run the code.\n",
        );
    }
    if info.drive {
        p.push_str(
            "- Google Drive storage is connected: `drive_list`, `drive_upload` and `drive_download` move files \
between the workspace and the user's Gagent folder in Drive.\n",
        );
    }

    p.push_str(
        "\n# How to work\n\
1. Explore first: list files, read the relevant code, search for symbols. Do not guess file contents.\n\
2. Make focused changes. Use `edit_file` for changes to existing files and `write_file` for new files or \
complete rewrites. Keep the existing style of the project.\n\
3. Verify by running. After changing code, run it or its tests with `run_command` and read the output \
carefully. If it fails, find the root cause in the logs, fix it, and run again. Repeat until it works or you \
are genuinely blocked. Do not repeat the same failing action without changing something.\n\
4. Never claim that something works unless you saw it work. Report failures honestly.\n\
5. Do not delete or overwrite files unrelated to the task. Never print or upload secrets.\n\
6. Ask the user a question only when the task cannot be done without the answer; otherwise make a \
sensible assumption and mention it.\n\
7. Finish with a short summary: what you changed, how you verified it, and anything that is left. \
Reply in the language the user writes in.\n",
    );

    let overview = overview(info);
    if !overview.is_empty() {
        p.push_str("\n# Workspace overview (current)\n");
        p.push_str(&overview);
    }
    p
}

/// A compact listing of the top of the project tree.
fn overview(info: &PromptInfo) -> String {
    let Ok(files) = walk_files(info.workspace.root(), info.opts) else {
        return String::new();
    };
    if files.is_empty() {
        return "The workspace is empty.\n".to_string();
    }
    let mut lines = Vec::new();
    let mut dirs = std::collections::BTreeMap::<String, usize>::new();
    for f in &files {
        let comps: Vec<&str> = f.rel.split('/').collect();
        if comps.len() <= 2 {
            lines.push(f.rel.clone());
        } else {
            *dirs
                .entry(format!("{}/{}/", comps[0], comps[1]))
                .or_insert(0) += 1;
        }
    }
    for (d, n) in dirs {
        lines.push(format!("{d} ({n} files)"));
    }
    lines.sort();
    let total = lines.len();
    lines.truncate(OVERVIEW_ENTRIES);
    let mut out = format!("{} files in total.\n", files.len());
    out.push_str(&lines.join("\n"));
    if total > OVERVIEW_ENTRIES {
        out.push_str(&format!(
            "\n... and {} more entries",
            total - OVERVIEW_ENTRIES
        ));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn info<'a>(ws: &'a Workspace, drive: bool, network: bool, sandbox: bool) -> PromptInfo<'a> {
        PromptInfo {
            workspace: ws,
            opts: WalkOptions::default(),
            drive,
            network,
            sandbox_available: sandbox,
        }
    }

    #[test]
    fn prompt_reflects_environment() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/deep/er")).unwrap();
        fs::write(dir.path().join("src/deep/er/a.rs"), "x").unwrap();
        fs::write(dir.path().join("README.md"), "x").unwrap();
        let ws = Workspace::new(dir.path()).unwrap();

        let p = system_prompt(&info(&ws, true, true, true));
        assert!(p.contains("Network access is enabled"));
        assert!(p.contains("drive_upload"));
        assert!(p.contains("README.md"));
        assert!(p.contains("src/deep/ (1 files)"), "{p}");

        let p = system_prompt(&info(&ws, false, false, true));
        assert!(p.contains("Network access is DISABLED"));
        assert!(!p.contains("drive_upload"));

        let p = system_prompt(&info(&ws, false, false, false));
        assert!(p.contains("NOT available"));
    }

    #[test]
    fn empty_workspace_is_described() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        assert!(system_prompt(&info(&ws, false, true, true)).contains("The workspace is empty"));
    }
}
