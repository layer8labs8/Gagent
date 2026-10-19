//! The system prompt that turns Gemini into a coding agent.

use crate::workspace::{walk_files, WalkOptions, Workspace};

/// How many entries of the project overview are put into the prompt.
const OVERVIEW_ENTRIES: usize = 120;

/// Describes the environment the agent works in.
pub struct PromptInfo<'a> {
    /// Name of the language model that answers (for example `nvidia/nemotron-3-ultra-550b-a55b`).
    pub model: &'a str,
    pub workspace: &'a Workspace,
    pub opts: WalkOptions,
    /// Whether the sandbox has network access.
    pub network: bool,
    /// Whether a working sandbox exists at all.
    pub sandbox_available: bool,
    /// The sandbox is a WSL Ubuntu on the user's Windows PC, not a Docker container.
    pub wsl: bool,
    /// Commands run with bash directly on the user's own computer.
    pub host: bool,
    /// What the sandbox has installed (from a probe); empty when unknown.
    pub tools: &'a str,
}

/// Builds the system prompt for one request.
pub fn system_prompt(info: &PromptInfo) -> String {
    let mut p = String::new();
    p.push_str(
        "You are Gagent, an autonomous software engineering agent running inside a desktop app. \
You complete the user's task by reading and editing project files and by running code in a sandbox, \
and you keep going until the task is done and verified.\n",
    );
    p.push_str(&format!(
        "The language model behind you is `{}`. If someone asks who or what you are, say that you are Gagent, \
running on that model, and do not claim to be made by any other company.\n\n",
        info.model
    ));

    p.push_str("# Environment\n");
    p.push_str(
        "- You work in a private copy of the user's project (the workspace). All file tools take paths \
relative to the workspace root, for example `src/main.py`. Never use absolute paths or `..`.\n\
- The user reviews your changes and applies them to the real project later, so you do not need to \
ask permission before editing files.\n",
    );
    if info.sandbox_available && !info.tools.is_empty() {
        p.push_str(&format!(
            "- Installed in the sandbox: {}. Anything not listed is NOT installed, and you cannot install system packages (no sudo, no apt); if a task needs one, say so in your answer.\n",
            info.tools
        ));
    }
    if info.sandbox_available && info.host {
        let os = if cfg!(windows) {
            "Windows (bash is Git Bash, so use Unix commands and forward slashes; `python` is the Windows Python)"
        } else {
            "Linux or macOS"
        };
        p.push_str(&format!(
            "- `run_command` executes bash directly on the user's own computer ({os}). The current directory is the \
workspace. Use only relative paths inside the workspace: commands that touch anything outside it, \
`sudo`, killing processes by name, shutdown, registry or disk changes are refused. You cannot install \
system packages; use what is installed, or `pip install --user` / `npm install` in the workspace. \
Each command has a time limit. Commands must not wait for keyboard input. A background process ends together \
with its command, so start a server, test it (`curl`, or a Python/Node script) and stop it (`kill $!`) inside ONE command.\n"
        ));
    } else if info.sandbox_available && info.wsl {
        p.push_str(
            "- `run_command` executes bash in an Ubuntu Linux (WSL) on the user's own computer. The current \
directory is the workspace. This is NOT isolated from the user's other files, so never read, write or delete \
anything outside the workspace and never use paths under /mnt. You are an unprivileged user without sudo. \
Python 3 and the usual command line tools are normally there, but other tools (pip, node, gcc) may be missing: \
check with `command -v` and prefer the Python standard library. There is no network guarantee. \
Each command has a time limit. Commands must not wait for keyboard input. Start long-running servers in the \
background with output redirected to a file, test them, and stop them.\n",
        );
    } else if info.sandbox_available {
        p.push_str(
            "- `run_command` executes bash inside an Ubuntu 24.04 sandbox. The workspace is mounted at \
/workspace, which is the default working directory. You are an unprivileged user without sudo or apt. \
Available: Python 3 (`python3`; `python` and `pip` work too) with pip and venv, Node.js with npm, gcc/g++/make, git, curl, jq, zip/unzip. ",
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
            "- The code sandbox is NOT available right now (neither Docker nor WSL is available), so `run_command` \
will fail. You can still read and edit files. Say clearly in your final answer that you could not run the code.\n",
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
4. Never claim that something works unless you saw it work. Report failures honestly. For web pages, test the \
JavaScript logic by running it (`node`, with the logic in functions and a few asserts) and check that every id and \
class the script uses exists in the HTML.\n\
5. Do not delete or overwrite files unrelated to the task. Never print or upload secrets.\n\
6. Ask the user a question only when the task cannot be done without the answer; otherwise make a \
sensible assumption and mention it.\n\
7. To save space, old file contents in your earlier `write_file` / `edit_file` calls and old command output are replaced \
by placeholders such as `[omitted: 1200 characters; the file on disk has them]`. The files on disk are the truth: read \
a file again if you need its contents, and never write such a placeholder into a file.\n\
8. Be economical, every step is paid for: search before reading, read only the line ranges you need, \
call several independent tools in one step, and do not read a file again right after you wrote it.\n\
9. Finish with a short summary: what you changed, how you verified it, and anything that is left. \
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

    fn info<'a>(ws: &'a Workspace, network: bool, sandbox: bool) -> PromptInfo<'a> {
        PromptInfo {
            model: "test-model",
            tools: "",
            workspace: ws,
            opts: WalkOptions::default(),
            network,
            sandbox_available: sandbox,
            wsl: false,
            host: false,
        }
    }

    #[test]
    fn prompt_reflects_environment() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/deep/er")).unwrap();
        fs::write(dir.path().join("src/deep/er/a.rs"), "x").unwrap();
        fs::write(dir.path().join("README.md"), "x").unwrap();
        let ws = Workspace::new(dir.path()).unwrap();

        let p = system_prompt(&info(&ws, true, true));
        assert!(p.contains("Network access is enabled"));
        assert!(p.contains("`test-model`"));
        assert!(p.contains("README.md"));
        assert!(p.contains("src/deep/ (1 files)"), "{p}");

        let p = system_prompt(&info(&ws, false, true));
        assert!(p.contains("Network access is DISABLED"));

        let p = system_prompt(&info(&ws, false, false));
        assert!(p.contains("NOT available"));
    }

    #[test]
    fn wsl_prompt_warns_about_the_users_files() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let mut i = info(&ws, true, true);
        i.wsl = true;
        let p = system_prompt(&i);
        assert!(p.contains("WSL"));
        assert!(p.contains("never use paths under /mnt"));
        assert!(!p.contains("Ubuntu 24.04 sandbox"));
    }

    #[test]
    fn host_prompt_names_the_rules_of_the_users_computer() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let mut i = info(&ws, true, true);
        i.host = true;
        let p = system_prompt(&i);
        assert!(p.contains("on the user's own computer"), "{p}");
        assert!(p.contains("relative paths inside the workspace"));
        assert!(!p.contains("Ubuntu 24.04 sandbox"));
    }

    #[test]
    fn installed_tools_are_listed_for_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let mut i = info(&ws, true, true);
        i.tools = "python3 pip3 node cargo; python: Python 3.14.4";
        let p = system_prompt(&i);
        assert!(
            p.contains("Installed in the sandbox: python3 pip3 node cargo"),
            "{p}"
        );
        assert!(p.contains("cannot install system packages"));
        // Nothing is said when the probe found nothing.
        assert!(!system_prompt(&info(&ws, true, true)).contains("Installed in the sandbox"));
    }

    #[test]
    fn the_fixed_part_of_every_request_stays_small() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let mut i = info(&ws, true, true);
        i.tools = "python3 pip3 node npm cargo rustc cc gcc make git curl; python: Python 3.14.4";
        let prompt = system_prompt(&i).len();
        let tools = crate::tools::declarations().to_string().len();
        // Sent again with every single request, so every character counts.
        assert!(
            prompt + tools < 8_000,
            "system prompt {prompt} + tools {tools} characters"
        );
    }

    #[test]
    fn empty_workspace_is_described() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        assert!(system_prompt(&info(&ws, true, true)).contains("The workspace is empty"));
    }
}
