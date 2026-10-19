//! The tools the model can call.
//!
//! File tools operate on the shadow workspace through [`Workspace`], so they
//! cannot leave it. `run_command` executes inside the sandbox. Every tool
//! returns plain text that is fed back to the model.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use regex::RegexBuilder;
use serde_json::{json, Value};

use crate::config::CheckLevel;
use crate::sandbox::{ExecRequest, Sandbox};
use crate::util::{human_bytes, looks_binary, truncate_end, truncate_middle, write_atomic};
use crate::workspace::{normalize_rel, walk_files, WalkOptions, Workspace};

/// Longest file the model may read or write in one call.
const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Files larger than this must be read in line ranges.
const MAX_FULL_READ_BYTES: u64 = 256 * 1024;
/// One `read_file` call returns at most this many lines. Every line is sent to the model
/// again at every later step, so big reads are expensive; the model can ask for more.
const DEFAULT_READ_LINES: usize = 500;
const MAX_LIST_LINES: usize = 400;
const MAX_SEARCH_RESULTS: usize = 60;
/// A repeated read of an unchanged file part is answered with a short note only if the
/// first read is at most this many tool calls old (so it is still in the conversation
/// and has not been shortened by the history compaction).
const REREAD_WINDOW: u64 = 6;
/// Longest command output returned to the model, in characters.
const MAX_COMMAND_OUTPUT_CHARS: usize = 12_000;

/// Everything a tool needs to do its work.
pub struct ToolContext {
    pub work: Workspace,
    pub sandbox: Arc<dyn Sandbox>,
    pub opts: WalkOptions,
    pub default_timeout_secs: u32,
    pub cancel: Arc<AtomicBool>,
    /// Quick checks run right after a file was written.
    pub check: CheckLevel,
    /// What the sandbox has installed. Filled by the first `run_command` of a session
    /// (so a chat without commands never starts the sandbox just to look).
    pub tools_note: Arc<std::sync::Mutex<Option<String>>>,
    /// Decides whether a command may run, asking the user when it has to.
    pub approvals: Arc<crate::permission::Approvals>,
    /// Which parts of which files were read lately (to answer a pointless re-read cheaply).
    pub reads: std::sync::Mutex<ReadLog>,
}

/// Remembers recent `read_file` results of one task.
#[derive(Default)]
pub struct ReadLog {
    /// Number of tool calls so far.
    calls: u64,
    seen: Vec<SeenRead>,
}

struct SeenRead {
    rel: String,
    first: usize,
    last: usize,
    len: u64,
    modified: Option<std::time::SystemTime>,
    at: u64,
}

impl ReadLog {
    /// Counts one more tool call.
    fn tick(&mut self) {
        self.calls += 1;
    }

    /// How many calls ago the same, unchanged part of the file was read; `None` when
    /// it has to be read (again).
    fn recent_read(
        &self,
        rel: &str,
        first: usize,
        last: usize,
        meta: &std::fs::Metadata,
    ) -> Option<u64> {
        let modified = meta.modified().ok();
        self.seen
            .iter()
            .rev()
            .find(|r| {
                r.rel == rel
                    && r.first <= first
                    && r.last >= last
                    && r.len == meta.len()
                    && r.modified == modified
                    && self.calls.saturating_sub(r.at) <= REREAD_WINDOW
            })
            .map(|r| self.calls - r.at)
    }

    fn remember(&mut self, rel: &str, first: usize, last: usize, meta: &std::fs::Metadata) {
        self.seen.retain(|r| r.rel != rel);
        self.seen.push(SeenRead {
            rel: rel.to_string(),
            first,
            last,
            len: meta.len(),
            modified: meta.modified().ok(),
            at: self.calls,
        });
        if self.seen.len() > 32 {
            self.seen.remove(0);
        }
    }
}

/// A bash snippet that lists the installed tools, one short line each.
pub const TOOL_PROBE: &str = r#"printf 'tools: '; for t in python3 pip3 node npm cargo rustc cc gcc make git curl unzip; do command -v $t >/dev/null 2>&1 && printf '%s ' $t; done; echo; v=$(python3 --version 2>&1) && echo "python: $v"; v=$(node --version 2>&1) && echo "node: $v"; v=$(cargo --version 2>&1) && echo "cargo: $v"; v=$(rustup target list --installed 2>/dev/null | tr '\n' ' ') && [ -n "$v" ] && echo "rust targets: $v""#;

/// Turns the output of [`TOOL_PROBE`] into one short line for the model.
pub fn summarize_probe(output: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.contains("not found") {
            continue;
        }
        match line.strip_prefix("tools:") {
            Some(rest) if rest.trim().is_empty() => parts.push("no common tools".into()),
            Some(rest) => parts.push(rest.trim().to_string()),
            None => parts.push(line.to_string()),
        }
    }
    truncate_end(&parts.join("; "), 400)
}

/// Looks at the sandbox once per session. Returns the note when this call did the looking.
fn probe_sandbox_once(ctx: &ToolContext) -> Option<String> {
    let mut slot = ctx.tools_note.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_some() {
        return None;
    }
    let req = ExecRequest {
        command: TOOL_PROBE.to_string(),
        cwd: String::new(),
        timeout_secs: 30,
    };
    let note = match ctx.sandbox.exec(&req, &ctx.cancel) {
        Ok(r) if r.exit_code.is_some() => summarize_probe(&r.output),
        _ => String::new(),
    };
    *slot = Some(note.clone());
    Some(note)
}

/// What a tool call produced.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    /// False when the call failed; the text then explains why.
    pub ok: bool,
    /// Text returned to the model.
    pub text: String,
    /// The shadow workspace may have changed.
    pub mutated: bool,
    /// Short result for the activity log, e.g. `exit 1`.
    pub summary: String,
}

impl ToolOutput {
    fn ok(text: impl Into<String>, summary: impl Into<String>, mutated: bool) -> Self {
        Self {
            ok: true,
            text: text.into(),
            mutated,
            summary: summary.into(),
        }
    }

    fn err(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            ok: false,
            summary: truncate_end(text.lines().next().unwrap_or(""), 120),
            text,
            mutated: false,
        }
    }
}

type ToolResult = Result<ToolOutput, String>;

/// Function declarations sent to the model. They are sent with every request, so the
/// descriptions are kept short.
pub fn declarations() -> Value {
    let path = |what: &str| json!({"type": "STRING", "description": what});
    let tools = vec![
        json!({
            "name": "list_files",
            "description": "List files as relative paths (build and dependency folders are hidden).",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": path("Folder, default: the root."),
                    "depth": {"type": "INTEGER", "description": "Levels below path (1-6), default 2."}
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read a text file. Lines come numbered (`  12 | code`); the numbers are not part of the file. Returns at most 500 lines: use start_line/end_line for more.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": path("File path."),
                    "start_line": {"type": "INTEGER", "description": "First line (1-based)."},
                    "end_line": {"type": "INTEGER", "description": "Last line (inclusive)."}
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Create a file or replace it completely (parent folders are created). Use edit_file for small changes.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": path("File path."),
                    "content": path("The complete new contents.")
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "edit_file",
            "description": "Replace text in an existing file. old_text must match exactly (indentation included) and be unique unless replace_all is true.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": path("File path."),
                    "old_text": path("Exact text to replace (no line numbers)."),
                    "new_text": path("Replacement text."),
                    "replace_all": {"type": "BOOLEAN", "description": "Replace every occurrence."}
                },
                "required": ["path", "old_text", "new_text"]
            }
        }),
        json!({
            "name": "delete_file",
            "description": "Delete a file.",
            "parameters": {
                "type": "OBJECT",
                "properties": { "path": path("File path.") },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search_files",
            "description": "Regex search in file contents. Returns `path:line: text`.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "pattern": path("Regular expression (Rust syntax)."),
                    "path": path("Folder, default: everything."),
                    "glob": path("Only files matching this glob, e.g. `*.py`."),
                    "ignore_case": {"type": "BOOLEAN", "description": "Case-insensitive."},
                    "files_only": {"type": "BOOLEAN", "description": "Only list files with the number of matches (cheaper)."}
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Run a bash command in the sandbox; the workspace is the working directory. Returns the exit code and the combined output. Must not wait for input.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "command": path("The bash command line."),
                    "cwd": path("Folder, default: the root."),
                    "timeout_secs": {"type": "INTEGER", "description": "Kill after this many seconds (default 120, max 900)."}
                },
                "required": ["command"]
            }
        }),
    ];
    Value::Array(tools)
}

/// One-line description of a call for the activity log.
pub fn describe_call(name: &str, args: &Value) -> String {
    let s = |keys: &[&str]| arg_str(args, keys).unwrap_or_default();
    match name {
        "list_files" => {
            let p = s(&["path"]);
            format!("List files in {}", if p.is_empty() { "." } else { &p })
        }
        "read_file" => {
            let range = match (arg_u64(args, &["start_line"]), arg_u64(args, &["end_line"])) {
                (Some(a), Some(b)) => format!(" (lines {a}-{b})"),
                (Some(a), None) => format!(" (from line {a})"),
                _ => String::new(),
            };
            format!("Read {}{range}", s(&["path", "file_path"]))
        }
        "write_file" => format!("Write {}", s(&["path", "file_path"])),
        "edit_file" => format!("Edit {}", s(&["path", "file_path"])),
        "delete_file" => format!("Delete {}", s(&["path", "file_path"])),
        "search_files" => format!("Search for `{}`", truncate_end(&s(&["pattern"]), 60)),
        "run_command" => {
            let cmd = s(&["command"]);
            let first = cmd.lines().next().unwrap_or("");
            let more = if cmd.lines().count() > 1 { " ..." } else { "" };
            format!("Run `{}{more}`", truncate_end(first, 100))
        }
        other => format!("Call {other}"),
    }
}

/// Runs one tool call. Never panics on bad input; errors become text for the model.
pub fn execute(ctx: &ToolContext, name: &str, args: &Value) -> ToolOutput {
    ctx.reads.lock().unwrap_or_else(|p| p.into_inner()).tick();
    let result = match name {
        "list_files" => list_files(ctx, args),
        "read_file" => read_file(ctx, args),
        "write_file" => write_file(ctx, args),
        "edit_file" => edit_file(ctx, args),
        "delete_file" => delete_file(ctx, args),
        "search_files" => search_files(ctx, args),
        "run_command" => run_command(ctx, args),
        other => Err(format!("unknown tool '{other}'")),
    };
    let mut out = match result {
        Ok(out) => out,
        Err(e) => ToolOutput::err(format!("Error: {e}")),
    };
    // A file was just written: check it now, so mistakes show up in this very step
    // instead of many steps (and many thousand tokens) later.
    if out.ok && out.mutated && (name == "write_file" || name == "edit_file") {
        if let Some(path) = arg_str(args, &["path", "file_path", "filepath", "filename"]) {
            if let Some(note) = post_write_check(ctx, name, &path) {
                if note.contains("FAILED") {
                    out.summary.push_str(" (check failed)");
                }
                out.text.push('\n');
                out.text.push_str(&note);
            }
        }
    }
    out
}

/// Runs a quick check on a file the agent just wrote. `None`: nothing to say.
fn post_write_check(ctx: &ToolContext, tool: &str, rel: &str) -> Option<String> {
    if ctx.check == CheckLevel::Off {
        return None;
    }
    let rel = normalize_rel(rel).ok()?;
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "json" => {
            // Needs no sandbox.
            let text = std::fs::read_to_string(ctx.work.resolve(&rel).ok()?).ok()?;
            Some(match serde_json::from_str::<Value>(&text) {
                Ok(_) => "[check: valid JSON]".to_string(),
                Err(e) => format!("[check FAILED: invalid JSON: {e}]"),
            })
        }
        "py" => run_check(
            ctx,
            &format!(
                r#"python3 -c 'import ast,sys; ast.parse(open(sys.argv[1], encoding="utf-8").read(), sys.argv[1])' {}"#,
                shell_quote(&rel)
            ),
            "Python syntax",
            30,
        ),
        "js" | "mjs" | "cjs" => run_check(
            ctx,
            &format!("node --check {}", shell_quote(&rel)),
            "JavaScript syntax",
            30,
        ),
        // Compiling is expensive, so it only runs after an edit (the repair loop), at the
        // Rust level, and not more often than once per half minute.
        "rs" if ctx.check == CheckLevel::Rust
            && tool == "edit_file"
            && ctx.work.root().join("Cargo.toml").is_file()
            && cargo_check_due() =>
        {
            run_check(
                ctx,
                "out=$(cargo check --message-format=short --color never 2>&1); code=$?; \
printf '%s\\n' \"$out\" | grep -v -E '^ *(Compiling|Checking|Downloaded|Downloading|Updating|Locking|Adding|Blocking) ' | head -n 40; exit $code",
                "cargo check",
                240,
            )
        }
        _ => None,
    }
}

/// True when the last `cargo check` of the agent is more than 30 seconds ago.
fn cargo_check_due() -> bool {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let due = last.is_none_or(|t| t.elapsed() > Duration::from_secs(30));
    if due {
        *last = Some(Instant::now());
    }
    due
}

/// Runs `command` in the sandbox. Success is a short note; a failure carries the first lines
/// of its output. A missing sandbox or a missing checker (exit 127) says nothing.
fn run_check(ctx: &ToolContext, command: &str, label: &str, secs: u32) -> Option<String> {
    let req = ExecRequest {
        command: command.to_string(),
        cwd: String::new(),
        timeout_secs: secs,
    };
    let result = ctx.sandbox.exec(&req, &ctx.cancel).ok()?;
    if result.cancelled || result.timed_out {
        return None;
    }
    match result.exit_code {
        Some(0) => Some(format!("[check: {label} OK]")),
        Some(127) | None => None,
        Some(_) => {
            let (output, _) = truncate_middle(result.output.trim(), 1800);
            Some(format!("[check FAILED: {label}]\n{output}"))
        }
    }
}

/// Quotes a string for bash.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// Argument helpers (models are sloppy about names and number formats)
// ---------------------------------------------------------------------------

fn arg_str(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

fn arg_u64(args: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| {
        let v = args.get(*k)?;
        v.as_u64()
            .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn required_str(args: &Value, keys: &[&str]) -> Result<String, String> {
    arg_str(args, keys).ok_or_else(|| format!("missing required argument '{}'", keys[0]))
}

// ---------------------------------------------------------------------------
// File tools
// ---------------------------------------------------------------------------

fn list_files(ctx: &ToolContext, args: &Value) -> ToolResult {
    let base = normalize_rel(&arg_str(args, &["path", "dir"]).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    let depth = arg_u64(args, &["depth"]).unwrap_or(2).clamp(1, 6) as usize;
    let dir = ctx.work.resolve(&base).map_err(|e| e.to_string())?;
    if dir.is_file() {
        return Err(format!("'{base}' is a file, not a folder"));
    }
    if !dir.is_dir() {
        return Err(format!("folder '{base}' does not exist"));
    }
    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };

    let mut lines: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut dir_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for file in walk_files(ctx.work.root(), ctx.opts).map_err(|e| e.to_string())? {
        let Some(rest) = file.rel.strip_prefix(&prefix) else {
            continue;
        };
        let comps: Vec<&str> = rest.split('/').collect();
        if comps.len() <= depth {
            lines.insert(
                file.rel.clone(),
                format!("{}  ({})", file.rel, human_bytes(file.size)),
            );
        } else {
            let dir_path = format!("{prefix}{}", comps[..depth].join("/"));
            *dir_counts.entry(dir_path).or_insert(0) += 1;
        }
    }
    for (dir_path, n) in dir_counts {
        lines.insert(format!("{dir_path}/"), format!("{dir_path}/  ({n} files)"));
    }
    if lines.is_empty() {
        return Ok(ToolOutput::ok("(no files)", "no files", false));
    }
    let total = lines.len();
    let mut out: Vec<String> = lines.into_values().take(MAX_LIST_LINES).collect();
    if total > MAX_LIST_LINES {
        out.push(format!(
            "... {} more entries; list a subfolder to see them",
            total - MAX_LIST_LINES
        ));
    }
    Ok(ToolOutput::ok(
        out.join("\n"),
        format!("{total} entries"),
        false,
    ))
}

fn read_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = required_str(args, &["path", "file_path", "filepath", "filename"])?;
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&path).map_err(|_| format!("file '{rel}' does not exist"))?;
    if meta.is_dir() {
        return Err(format!("'{rel}' is a folder; use list_files"));
    }
    let start = arg_u64(args, &["start_line"]);
    let end = arg_u64(args, &["end_line"]);
    if meta.len() > MAX_FULL_READ_BYTES && start.is_none() && end.is_none() {
        return Err(format!(
            "'{rel}' is large ({}); read it in pieces with start_line and end_line, or use search_files",
            human_bytes(meta.len())
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read '{rel}': {e}"))?;
    if looks_binary(&bytes) {
        return Err(format!(
            "'{rel}' looks like a binary file ({})",
            human_bytes(bytes.len() as u64)
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(ToolOutput::ok(
            format!("[{rel}: empty file]"),
            "empty file",
            false,
        ));
    }
    let first = start.unwrap_or(1).max(1) as usize;
    let last = end
        .map(|e| e as usize)
        .unwrap_or(first + DEFAULT_READ_LINES - 1)
        .min(total);
    if first > total {
        return Err(format!("'{rel}' has only {total} lines"));
    }
    if last < first {
        return Err("end_line must not be smaller than start_line".into());
    }
    let last = last.min(first + DEFAULT_READ_LINES - 1);
    {
        let mut log = ctx.reads.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(ago) = log.recent_read(&rel, first, last, &meta) {
            return Ok(ToolOutput::ok(
                format!(
                    "[{rel}: lines {first}-{last} are unchanged since your read {ago} step(s) ago; that result is still above, use it]"
                ),
                "unchanged",
                false,
            ));
        }
        log.remember(&rel, first, last, &meta);
    }
    let width = last.to_string().len().max(4);
    let mut out = format!("[{rel}: lines {first}-{last} of {total}]\n");
    for (i, line) in lines[first - 1..last].iter().enumerate() {
        out.push_str(&format!("{:>width$} | {}\n", first + i, line));
    }
    if last < total {
        out.push_str(&format!(
            "[... {} more lines; use start_line={} to continue]\n",
            total - last,
            last + 1
        ));
    }
    Ok(ToolOutput::ok(
        out,
        format!("{} lines", last - first + 1),
        false,
    ))
}

fn write_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = required_str(args, &["path", "file_path", "filepath", "filename"])?;
    let content = required_str(args, &["content", "contents", "text", "file_text"])?;
    if content.len() > MAX_FILE_BYTES {
        return Err(format!(
            "content is too large ({}); split it into several files",
            human_bytes(content.len() as u64)
        ));
    }
    if rel.trim().is_empty() || normalize_rel(&rel).map_err(|e| e.to_string())?.is_empty() {
        return Err("path must name a file".into());
    }
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    if path.is_dir() {
        return Err(format!("'{rel}' is a folder"));
    }
    let existed = path.exists();
    // Keep the line endings of a file that uses Windows (CRLF) endings, so that
    // rewriting it does not turn every line into a change.
    let content = match std::fs::read(&path) {
        Ok(old) if uses_crlf(&old) && !content.contains('\r') => content.replace('\n', "\r\n"),
        _ => content,
    };
    write_atomic(&path, content.as_bytes()).map_err(|e| format!("{e:#}"))?;
    let lines = content.lines().count();
    let verb = if existed { "Overwrote" } else { "Created" };
    Ok(ToolOutput::ok(
        format!(
            "{verb} {rel} ({lines} lines, {})",
            human_bytes(content.len() as u64)
        ),
        format!("{} lines", lines),
        true,
    ))
}

/// Converts every line break in `s` to `\r\n`.
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// True when most line breaks in `bytes` are `\r\n`.
fn uses_crlf(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(256 * 1024)];
    let crlf = sample.windows(2).filter(|w| w == b"\r\n").count();
    let lf = sample.iter().filter(|b| **b == b'\n').count();
    crlf > 0 && crlf * 2 >= lf
}

fn edit_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = required_str(args, &["path", "file_path", "filepath", "filename"])?;
    let old = required_str(args, &["old_text", "old_string", "old"])?;
    let new = required_str(args, &["new_text", "new_string", "new"])?;
    let replace_all = arg_bool(args, "replace_all");
    if old.is_empty() {
        return Err(
            "old_text must not be empty; use write_file to create or replace a whole file".into(),
        );
    }
    if old == new {
        return Err("old_text and new_text are identical; nothing to change".into());
    }
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path)
        .map_err(|_| format!("file '{rel}' does not exist; use write_file to create it"))?;
    if looks_binary(&bytes) {
        return Err(format!("'{rel}' looks like a binary file"));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("'{rel}' is not valid UTF-8"))?;

    let (old_eff, new_eff) = if text.contains(&old) {
        let new_eff = if uses_crlf(text.as_bytes()) {
            to_crlf(&new)
        } else {
            new.clone()
        };
        (old.clone(), new_eff)
    } else if text.contains("\r\n") && !old.contains("\r\n") {
        (to_crlf(&old), to_crlf(&new))
    } else {
        (old.clone(), new.clone())
    };

    let count = text.matches(&old_eff).count();
    if count == 0 {
        let hint = closest_line_hint(&text, &old);
        return Err(format!(
            "old_text was not found in '{rel}'. Read the file again and copy the text exactly, including indentation, without line numbers.{hint}"
        ));
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old_text matches {count} places in '{rel}'. Add more surrounding lines to make it unique, or set replace_all to true."
        ));
    }
    let updated = if replace_all {
        text.replace(&old_eff, &new_eff)
    } else {
        text.replacen(&old_eff, &new_eff, 1)
    };
    write_atomic(&path, updated.as_bytes()).map_err(|e| format!("{e:#}"))?;
    Ok(ToolOutput::ok(
        format!(
            "Edited {rel} ({count} replacement{})",
            if count == 1 { "" } else { "s" }
        ),
        format!("{count} replacement{}", if count == 1 { "" } else { "s" }),
        true,
    ))
}

/// When an edit does not match, point at the line that looks most similar.
fn closest_line_hint(text: &str, old: &str) -> String {
    let needle = old
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if needle.is_empty() {
        return String::new();
    }
    for (i, line) in text.lines().enumerate() {
        if line.trim() == needle {
            return format!(
                " The first line of old_text does appear on line {} as `{}` (check indentation).",
                i + 1,
                truncate_end(line, 120)
            );
        }
    }
    String::new()
}

fn delete_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = required_str(args, &["path", "file_path", "filepath", "filename"])?;
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| format!("'{rel}' does not exist"))?;
    if meta.is_dir() {
        return Err(format!("'{rel}' is a folder; delete_file only removes files (use run_command with rm -r if needed)"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot delete '{rel}': {e}"))?;
    Ok(ToolOutput::ok(format!("Deleted {rel}"), "deleted", true))
}

fn search_files(ctx: &ToolContext, args: &Value) -> ToolResult {
    let pattern = required_str(args, &["pattern", "query", "regex"])?;
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(arg_bool(args, "ignore_case"))
        .size_limit(10 << 20)
        .build()
        .map_err(|e| format!("invalid regular expression: {e}"))?;
    let base =
        normalize_rel(&arg_str(args, &["path"]).unwrap_or_default()).map_err(|e| e.to_string())?;
    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };
    let glob =
        match arg_str(args, &["glob", "file_glob", "include"]).filter(|g| !g.trim().is_empty()) {
            Some(g) => Some(
                globset::Glob::new(g.trim())
                    .map_err(|e| format!("invalid glob: {e}"))?
                    .compile_matcher(),
            ),
            None => None,
        };

    let files_only = arg_bool(args, "files_only");
    let mut per_file: Vec<(String, usize)> = Vec::new();
    let mut results = Vec::new();
    let mut files_with_hits = 0;
    'files: for file in walk_files(ctx.work.root(), ctx.opts).map_err(|e| e.to_string())? {
        if !prefix.is_empty() && !file.rel.starts_with(&prefix) {
            continue;
        }
        if let Some(g) = &glob {
            let name = file.rel.rsplit('/').next().unwrap_or(&file.rel);
            if !g.is_match(&file.rel) && !g.is_match(name) {
                continue;
            }
        }
        if file.size > 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(ctx.work.root().join(&file.rel)) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut hit = false;
        if files_only {
            let n = text.lines().filter(|l| regex.is_match(l)).count();
            if n > 0 {
                per_file.push((file.rel.clone(), n));
                if per_file.len() >= MAX_LIST_LINES {
                    break 'files;
                }
            }
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            if regex.is_match(line) {
                hit = true;
                results.push(format!(
                    "{}:{}: {}",
                    file.rel,
                    i + 1,
                    truncate_end(line.trim(), 200)
                ));
                if results.len() >= MAX_SEARCH_RESULTS {
                    files_with_hits += 1;
                    break 'files;
                }
            }
        }
        if hit {
            files_with_hits += 1;
        }
    }
    if files_only {
        if per_file.is_empty() {
            return Ok(ToolOutput::ok("No matches.", "no matches", false));
        }
        let total: usize = per_file.iter().map(|(_, n)| n).sum();
        let mut out: String = per_file
            .iter()
            .map(|(f, n)| format!("{f}: {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        if per_file.len() >= MAX_LIST_LINES {
            out.push_str("\n[list cut; narrow the search]");
        }
        return Ok(ToolOutput::ok(
            out,
            format!("{total} matches in {} files", per_file.len()),
            false,
        ));
    }
    if results.is_empty() {
        return Ok(ToolOutput::ok("No matches.", "no matches", false));
    }
    let mut out = results.join("\n");
    if results.len() >= MAX_SEARCH_RESULTS {
        out.push_str(&format!(
            "\n[stopped after {MAX_SEARCH_RESULTS} matches; narrow the search or use files_only]"
        ));
    }
    Ok(ToolOutput::ok(
        out,
        format!("{} matches in {files_with_hits} files", results.len()),
        false,
    ))
}

// ---------------------------------------------------------------------------
// Sandbox command
// ---------------------------------------------------------------------------

fn run_command(ctx: &ToolContext, args: &Value) -> ToolResult {
    let command = required_str(args, &["command", "cmd"])?;
    let cwd =
        normalize_rel(&arg_str(args, &["cwd", "dir", "working_directory"]).unwrap_or_default())
            .map_err(|e| e.to_string())?;
    let timeout = arg_u64(args, &["timeout_secs", "timeout"])
        .map(|t| t as u32)
        .unwrap_or(ctx.default_timeout_secs)
        .clamp(1, 900);
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    ctx.approvals
        .authorize(&command, ctx.work.root(), ctx.sandbox.needs_policy(), &ctx.cancel)?;
    let probed = probe_sandbox_once(ctx).filter(|n| !n.is_empty());
    let req = ExecRequest {
        command,
        cwd,
        timeout_secs: timeout,
    };
    let result = ctx
        .sandbox
        .exec(&req, &ctx.cancel)
        .map_err(|e| format!("{e:#}"))?;

    let cleaned = clean_command_output(result.output.trim_end());
    let (output, cut) = truncate_middle(&cleaned, MAX_COMMAND_OUTPUT_CHARS);
    let secs = round1(result.duration);
    let status_line = if result.cancelled {
        "CANCELLED by the user".to_string()
    } else if result.timed_out {
        format!("TIMED OUT after {timeout}s (the command was killed)")
    } else {
        match result.exit_code {
            Some(code) => format!("exit code: {code}"),
            None => "the command did not finish".to_string(),
        }
    };
    let mut text = format!("{status_line}\nduration: {secs}s\n");
    if let Some(note) = probed {
        text.push_str(&format!("[sandbox tools: {note}]\n"));
    }
    if output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str("--- output ---\n");
        text.push_str(&output);
    }
    if cut || result.truncated {
        text.push_str("\n[output was shortened]");
    }
    let summary = if result.timed_out {
        format!("timed out ({timeout}s)")
    } else if result.cancelled {
        "cancelled".to_string()
    } else {
        format!(
            "exit {} in {secs}s",
            result
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        )
    };
    // Commands can change files (builds, generated output); refresh the change list.
    let mut out = ToolOutput::ok(text, summary, true);
    out.ok = result.success();
    Ok(out)
}

/// Removes noise from long command output: build progress lines (cargo, npm) and
/// endless repeats of one line. Short output is left exactly as it is.
pub fn clean_command_output(text: &str) -> String {
    const NOISE: &[&str] = &[
        "Compiling ",
        "Downloaded ",
        "Downloading ",
        "Checking ",
        "Fresh ",
        "Updating ",
        "Locking ",
        "Adding ",
        "Blocking ",
        "Unpacking ",
    ];
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 40 {
        return text.to_string();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let (mut removed, mut repeats) = (0usize, 0usize);
    let mut last: Option<&str> = None;
    for line in &lines {
        let trimmed = line.trim_start();
        if NOISE.iter().any(|n| trimmed.starts_with(n)) {
            removed += 1;
            continue;
        }
        if last == Some(*line) && !line.trim().is_empty() {
            repeats += 1;
            if repeats > 2 {
                continue;
            }
        } else {
            if repeats > 2 {
                out.push(format!(
                    "[the line above repeated {} more times]",
                    repeats - 2
                ));
            }
            repeats = 0;
        }
        last = Some(*line);
        out.push((*line).to_string());
    }
    if repeats > 2 {
        out.push(format!(
            "[the line above repeated {} more times]",
            repeats - 2
        ));
    }
    if removed > 0 {
        out.insert(0, format!("[{removed} build progress lines removed]"));
    }
    out.join("\n")
}

fn round1(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{ExecResult, ScriptedSandbox};
    use std::fs;

    struct Fx {
        dir: tempfile::TempDir,
        ctx: ToolContext,
        sandbox: Arc<ScriptedSandbox>,
    }

    fn fx(results: Vec<ExecResult>) -> Fx {
        fx_with(ScriptedSandbox::new(results))
    }

    fn fx_with(sandbox: ScriptedSandbox) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(sandbox);
        let ctx = ToolContext {
            work: Workspace::new(dir.path()).unwrap(),
            sandbox: sandbox.clone(),
            opts: WalkOptions::default(),
            default_timeout_secs: 60,
            cancel: Arc::new(AtomicBool::new(false)),
            check: CheckLevel::Off,
            // Already "looked at": the scripted sandbox must not spend a result on the probe.
            tools_note: Arc::new(std::sync::Mutex::new(Some(String::new()))),
            approvals: Arc::new(crate::permission::Approvals::new(
                crate::config::PermissionMode::Auto,
            )),
            reads: Default::default(),
        };
        Fx {
            dir,
            ctx,
            sandbox,
        }
    }

    fn call(fx: &Fx, name: &str, args: Value) -> ToolOutput {
        execute(&fx.ctx, name, &args)
    }

    #[test]
    fn write_then_read_with_line_numbers() {
        let f = fx(vec![]);
        let w = call(
            &f,
            "write_file",
            json!({"path":"src/app.py","content":"a = 1\nb = 2\nc = 3\n"}),
        );
        assert!(w.ok, "{}", w.text);
        assert!(w.mutated);
        assert!(w.text.contains("Created src/app.py"));
        let r = call(
            &f,
            "read_file",
            json!({"path":"src/app.py","start_line":2,"end_line":"2"}),
        );
        assert!(
            r.text.contains("   2 | b = 2") && !r.text.contains("a = 1"),
            "{}",
            r.text
        );
        let r = call(&f, "read_file", json!({"path":"src/app.py"}));
        assert!(r.ok);
        assert!(r.text.contains("   1 | a = 1"), "{}", r.text);
        assert!(r.text.contains("   3 | c = 3"));
        let w2 = call(&f, "write_file", json!({"path":"src/app.py","content":"x"}));
        assert!(w2.text.contains("Overwrote"));
    }

    #[test]
    fn read_file_returns_at_most_500_lines_by_default() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("long.txt"), "x\n".repeat(1200)).unwrap();
        let r = call(&f, "read_file", json!({"path":"long.txt"}));
        assert!(r.ok);
        assert!(r.text.contains("lines 1-500 of 1200"), "{}", &r.text[..80]);
        assert!(r.text.contains("start_line=501"));
    }

    #[test]
    fn cargo_noise_and_repeats_are_removed_from_long_output() {
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!("   Compiling crate{i} v0.1.0\n"));
        }
        text.push_str("error[E0382]: borrow of moved value\n --> src/main.rs:3:5\n");
        for _ in 0..30 {
            text.push_str("same line again\n");
        }
        text.push_str("test result: FAILED. 1 passed; 1 failed\n");
        let cleaned = clean_command_output(&text);
        assert!(!cleaned.contains("Compiling crate7"));
        assert!(cleaned.starts_with("[60 build progress lines removed]"));
        assert!(cleaned.contains("error[E0382]"));
        assert!(cleaned.contains("src/main.rs:3:5"));
        assert!(cleaned.contains("[the line above repeated 27 more times]"));
        assert!(cleaned.contains("test result: FAILED"));
        assert!(cleaned.len() * 5 < text.len());
        // Short output stays exactly as it is.
        let short = "   Compiling a v1\nok\n";
        assert_eq!(clean_command_output(short), short);
    }

    #[test]
    fn written_python_files_are_checked() {
        let mut f = fx(vec![
            ExecResult {
                exit_code: Some(0),
                ..Default::default()
            },
            ExecResult {
                exit_code: Some(1),
                output: "  File \"a.py\", line 1\nSyntaxError: invalid syntax\n".into(),
                ..Default::default()
            },
            ExecResult {
                exit_code: Some(127),
                output: "python3: not found".into(),
                ..Default::default()
            },
        ]);
        f.ctx.check = CheckLevel::Light;
        let ok = call(&f, "write_file", json!({"path":"a.py","content":"x = 1\n"}));
        assert!(
            ok.ok && ok.text.contains("[check: Python syntax OK]"),
            "{}",
            ok.text
        );
        let bad = call(&f, "write_file", json!({"path":"b.py","content":"x ="}));
        assert!(bad.ok, "the write itself succeeded");
        assert!(
            bad.text.contains("[check FAILED: Python syntax]"),
            "{}",
            bad.text
        );
        assert!(bad.text.contains("SyntaxError"));
        assert!(bad.summary.contains("check failed"));
        // A missing checker (exit 127) is not reported.
        let none = call(&f, "write_file", json!({"path":"c.py","content":"x = 1\n"}));
        assert!(!none.text.contains("check"), "{}", none.text);
        let cmds = f.sandbox.commands.lock().unwrap();
        assert_eq!(cmds.len(), 3);
        assert!(cmds[0].contains("ast.parse") && cmds[0].contains("'a.py'"));
    }

    #[test]
    fn checks_can_be_switched_off_and_json_needs_no_sandbox() {
        let f = fx(vec![]);
        let w = call(&f, "write_file", json!({"path":"a.py","content":"x = 1\n"}));
        assert!(!w.text.contains("check"));
        assert!(f.sandbox.commands.lock().unwrap().is_empty());

        let mut g = fx(vec![]);
        g.ctx.check = CheckLevel::Light;
        let good = call(
            &g,
            "write_file",
            json!({"path":"a.json","content":"{\"a\": 1}"}),
        );
        assert!(good.text.contains("[check: valid JSON]"), "{}", good.text);
        let bad = call(
            &g,
            "write_file",
            json!({"path":"b.json","content":"{\"a\": }"}),
        );
        assert!(
            bad.text.contains("[check FAILED: invalid JSON"),
            "{}",
            bad.text
        );
        assert!(g.sandbox.commands.lock().unwrap().is_empty());
        // Other file types are not checked.
        let md = call(&g, "write_file", json!({"path":"a.md","content":"# hi"}));
        assert!(!md.text.contains("check"));
    }

    #[test]
    fn rust_is_only_checked_after_edits_at_the_rust_level() {
        let mut f = fx(vec![ExecResult {
            exit_code: Some(101),
            output: "src/lib.rs:1:1: error[E0425]: cannot find value `x`\n".into(),
            ..Default::default()
        }]);
        fs::write(
            f.ctx.work.root().join("Cargo.toml"),
            "[package]\nname=\"t\"\n",
        )
        .unwrap();
        f.ctx.check = CheckLevel::Light;
        let w = call(
            &f,
            "write_file",
            json!({"path":"src/lib.rs","content":"fn a() {}\n"}),
        );
        assert!(!w.text.contains("check"), "Light does not compile");
        f.ctx.check = CheckLevel::Rust;
        let w = call(
            &f,
            "write_file",
            json!({"path":"src/lib.rs","content":"fn a() {}\n"}),
        );
        assert!(!w.text.contains("check"), "a fresh write is not compiled");
        assert!(f.sandbox.commands.lock().unwrap().is_empty());
        let e = call(
            &f,
            "edit_file",
            json!({"path":"src/lib.rs","old_text":"fn a()","new_text":"fn b()"}),
        );
        assert!(e.text.contains("[check FAILED: cargo check]"), "{}", e.text);
        assert!(e.text.contains("E0425"));
        assert!(f.sandbox.commands.lock().unwrap()[0].contains("cargo check"));
    }

    #[test]
    fn the_first_command_looks_at_the_sandbox_once() {
        let f = fx(vec![
            ExecResult {
                exit_code: Some(0),
                output: "tools: python3 pip3 git \npython: Python 3.12.3\n".into(),
                ..Default::default()
            },
            ExecResult {
                exit_code: Some(0),
                output: "one\n".into(),
                ..Default::default()
            },
            ExecResult {
                exit_code: Some(0),
                output: "two\n".into(),
                ..Default::default()
            },
        ]);
        *f.ctx.tools_note.lock().unwrap() = None;
        let first = call(&f, "run_command", json!({"command":"echo one"}));
        assert!(
            first
                .text
                .contains("[sandbox tools: python3 pip3 git; python: Python 3.12.3]"),
            "{}",
            first.text
        );
        assert!(first.text.contains("one"));
        let second = call(&f, "run_command", json!({"command":"echo two"}));
        assert!(!second.text.contains("sandbox tools"));
        let cmds = f.sandbox.commands.lock().unwrap();
        assert_eq!(cmds.len(), 3, "the probe ran once: {cmds:?}");
        assert!(cmds[0].contains("command -v"));
        assert_eq!(cmds[1], "echo one");
        assert_eq!(
            f.ctx.tools_note.lock().unwrap().as_deref(),
            Some("python3 pip3 git; python: Python 3.12.3")
        );
    }

    #[test]
    fn probe_output_is_summarized() {
        let out = "tools: python3 git \nbash: node: not found\npython: Python 3.14.4\nrust targets: x86_64-pc-windows-gnu x86_64-unknown-linux-gnu \n";
        assert_eq!(
            summarize_probe(out),
            "python3 git; python: Python 3.14.4; rust targets: x86_64-pc-windows-gnu x86_64-unknown-linux-gnu"
        );
        assert_eq!(summarize_probe("tools: \n"), "no common tools");
        assert_eq!(summarize_probe(""), "");
    }

    #[test]
    fn declarations_stay_small() {
        let size = declarations().to_string().len();
        assert!(
            size < 3_800,
            "tool declarations are sent with every request: {size} chars"
        );
    }

    #[test]
    fn edit_requires_unique_match() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("a.txt"), "foo\nbar\nfoo\n").unwrap();
        let e = call(
            &f,
            "edit_file",
            json!({"path":"a.txt","old_text":"foo","new_text":"baz"}),
        );
        assert!(!e.ok);
        assert!(e.text.contains("2 places"), "{}", e.text);
        let e = call(
            &f,
            "edit_file",
            json!({"path":"a.txt","old_text":"foo","new_text":"baz","replace_all":true}),
        );
        assert!(e.ok, "{}", e.text);
        assert_eq!(
            fs::read_to_string(f.ctx.work.root().join("a.txt")).unwrap(),
            "baz\nbar\nbaz\n"
        );
        let e = call(
            &f,
            "edit_file",
            json!({"path":"a.txt","old_text":"bar","new_text":"BAR"}),
        );
        assert!(e.ok);
        assert_eq!(
            fs::read_to_string(f.ctx.work.root().join("a.txt")).unwrap(),
            "baz\nBAR\nbaz\n"
        );
    }

    #[test]
    fn edit_reports_missing_text_with_hint() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("a.py"), "def f():\n    return 1\n").unwrap();
        let e = call(
            &f,
            "edit_file",
            json!({"path":"a.py","old_text":"return 1\n  extra","new_text":"x"}),
        );
        assert!(!e.ok);
        assert!(e.text.contains("not found"));
        let e = call(
            &f,
            "edit_file",
            json!({"path":"a.py","old_text":"        return 1","new_text":"x"}),
        );
        assert!(e.text.contains("line 2"), "{}", e.text);
    }

    #[test]
    fn edit_handles_crlf_files() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let e = call(
            &f,
            "edit_file",
            json!({"path":"w.txt","old_text":"one\ntwo","new_text":"1\n2"}),
        );
        assert!(e.ok, "{}", e.text);
        assert_eq!(
            fs::read_to_string(f.ctx.work.root().join("w.txt")).unwrap(),
            "1\r\n2\r\nthree\r\n"
        );
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let f = fx(vec![]);
        let path = f.ctx.work.root().join("w.txt");
        fs::write(&path, "one\r\ntwo\r\n").unwrap();
        // A single-line match that inserts several lines.
        let e = call(
            &f,
            "edit_file",
            json!({"path":"w.txt","old_text":"two","new_text":"2a\n2b"}),
        );
        assert!(e.ok, "{}", e.text);
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\r\n2a\r\n2b\r\n");
        // Rewriting the whole file keeps CRLF too.
        let w = call(&f, "write_file", json!({"path":"w.txt","content":"a\nb\n"}));
        assert!(w.ok, "{}", w.text);
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\r\nb\r\n");
        // LF files stay LF.
        fs::write(&path, "x\ny\n").unwrap();
        let w = call(&f, "write_file", json!({"path":"w.txt","content":"a\nb\n"}));
        assert!(w.ok);
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
    }

    #[test]
    fn paths_cannot_escape() {
        let f = fx(vec![]);
        for (tool, args) in [
            ("read_file", json!({"path":"../../etc/passwd"})),
            ("write_file", json!({"path":"/etc/cron.d/x","content":"x"})),
            ("write_file", json!({"path":"a/../../x","content":"x"})),
            ("delete_file", json!({"path":"C:\\Windows\\x"})),
            ("list_files", json!({"path":".."})),
        ] {
            let out = call(&f, tool, args.clone());
            assert!(!out.ok, "{tool} {args} should fail");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_planted_by_sandbox_is_not_followed() {
        let f = fx(vec![]);
        std::os::unix::fs::symlink("/etc/passwd", f.ctx.work.root().join("pw")).unwrap();
        let out = call(&f, "read_file", json!({"path":"pw"}));
        assert!(!out.ok);
        assert!(out.text.contains("symbolic link"), "{}", out.text);
    }

    #[test]
    fn list_and_search() {
        let f = fx(vec![]);
        for (p, c) in [
            ("main.py", "import util\nprint(util.TODO_VALUE)\n"),
            ("pkg/util.py", "TODO_VALUE = 1\n# TODO fix\n"),
            ("pkg/deep/er/x.txt", "nothing"),
            ("node_modules/lib/i.js", "TODO"),
        ] {
            let full = f.ctx.work.root().join(p);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, c).unwrap();
        }
        let l = call(&f, "list_files", json!({}));
        assert!(l.text.contains("main.py"), "{}", l.text);
        assert!(l.text.contains("pkg/util.py"));
        assert!(l.text.contains("pkg/deep/  (1 files)"), "{}", l.text);
        assert!(!l.text.contains("node_modules"));
        let l = call(&f, "list_files", json!({"path":"pkg","depth":5}));
        assert!(l.text.contains("pkg/deep/er/x.txt"));

        let s = call(
            &f,
            "search_files",
            json!({"pattern":"todo","ignore_case":true}),
        );
        assert!(s.text.contains("main.py:2:"), "{}", s.text);
        assert!(s.text.contains("pkg/util.py:2: # TODO fix"));
        assert!(!s.text.contains("node_modules"));
        let s = call(
            &f,
            "search_files",
            json!({"pattern":"TODO","glob":"*.py","path":"pkg"}),
        );
        assert!(!s.text.contains("main.py"));
        let s = call(&f, "search_files", json!({"pattern":"zzzz"}));
        assert_eq!(s.text, "No matches.");
        let s = call(&f, "search_files", json!({"pattern":"("}));
        assert!(!s.ok);
    }

    #[test]
    fn binary_and_large_files_are_refused() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        assert!(!call(&f, "read_file", json!({"path":"b.bin"})).ok);
        fs::write(f.ctx.work.root().join("big.txt"), "x\n".repeat(200_000)).unwrap();
        let r = call(&f, "read_file", json!({"path":"big.txt"}));
        assert!(!r.ok);
        assert!(r.text.contains("start_line"));
        let r = call(
            &f,
            "read_file",
            json!({"path":"big.txt","start_line":10,"end_line":12}),
        );
        assert!(r.ok);
        assert!(r.text.contains("lines 10-12 of 200000"), "{}", r.text);
    }

    #[test]
    fn run_command_formats_results() {
        let f = fx(vec![
            ExecResult {
                exit_code: Some(0),
                output: "hello\n".into(),
                duration: Duration::from_millis(1500),
                ..Default::default()
            },
            ExecResult {
                exit_code: Some(2),
                output: "Traceback...\nValueError\n".into(),
                ..Default::default()
            },
            ExecResult {
                exit_code: None,
                output: "partial".into(),
                timed_out: true,
                ..Default::default()
            },
        ]);
        let r = call(&f, "run_command", json!({"command":"echo hello"}));
        assert!(r.ok);
        assert!(r.text.starts_with("exit code: 0"), "{}", r.text);
        assert!(r.text.contains("hello"));
        assert_eq!(r.summary, "exit 0 in 1.5s");
        let r = call(&f, "run_command", json!({"command":"python x.py"}));
        assert!(!r.ok);
        assert!(r.text.contains("exit code: 2") && r.text.contains("ValueError"));
        let r = call(
            &f,
            "run_command",
            json!({"command":"sleep 999","timeout_secs":5}),
        );
        assert!(r.text.contains("TIMED OUT after 5s"));
        assert_eq!(f.sandbox.commands.lock().unwrap().len(), 3);
    }

    #[test]
    fn run_command_validates_input() {
        let f = fx(vec![]);
        assert!(!call(&f, "run_command", json!({})).ok);
        assert!(!call(&f, "run_command", json!({"command":"ls","cwd":"../.."})).ok);
        f.ctx.cancel.store(true, Ordering::Relaxed);
        assert!(!call(&f, "run_command", json!({"command":"ls"})).ok);
    }

    #[test]
    fn long_output_is_shortened() {
        // Different lines: identical repeats are collapsed before the length limit applies.
        let big: String = (0..20_000).map(|i| format!("line number {i}\n")).collect();
        let f = fx(vec![ExecResult {
            exit_code: Some(0),
            output: big,
            ..Default::default()
        }]);
        let r = call(&f, "run_command", json!({"command":"yes"}));
        assert!(r.text.contains("characters omitted"));
        assert!(r.text.chars().count() < MAX_COMMAND_OUTPUT_CHARS + 500);
    }

    #[test]
    fn an_endless_repeat_does_not_fill_the_context() {
        let f = fx(vec![ExecResult {
            exit_code: Some(1),
            output: format!("{}done\n", "retrying connection...\n".repeat(20_000)),
            ..Default::default()
        }]);
        let r = call(&f, "run_command", json!({"command":"loop"}));
        assert!(
            r.text.contains("repeated 19997 more times"),
            "{}",
            &r.text[..200.min(r.text.len())]
        );
        assert!(r.text.contains("done"));
        assert!(r.text.len() < 600, "{} characters", r.text.len());
    }

    #[test]
    fn rereading_an_unchanged_range_costs_almost_nothing() {
        let f = fx(vec![]);
        let body: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        std::fs::write(f.dir.path().join("a.txt"), &body).unwrap();
        let first = call(&f, "read_file", json!({"path":"a.txt"}));
        assert!(first.text.contains("line 50"));
        let again = call(&f, "read_file", json!({"path":"a.txt"}));
        assert!(again.ok);
        assert!(again.text.contains("unchanged"), "{}", again.text);
        assert!(again.text.len() < 200);
        // A smaller part of what was read is covered too.
        let part = call(&f, "read_file", json!({"path":"a.txt","start_line":10,"end_line":20}));
        assert!(part.text.contains("unchanged"), "{}", part.text);
        // After a change the real content comes back.
        std::fs::write(f.dir.path().join("a.txt"), format!("{body}extra\n")).unwrap();
        let changed = call(&f, "read_file", json!({"path":"a.txt"}));
        assert!(changed.text.contains("extra"), "{}", changed.text);
    }

    #[test]
    fn an_old_read_is_repeated_because_the_history_may_have_shortened_it() {
        let f = fx(vec![]);
        std::fs::write(f.dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        call(&f, "read_file", json!({"path":"a.txt"}));
        for _ in 0..8 {
            call(&f, "list_files", json!({}));
        }
        let again = call(&f, "read_file", json!({"path":"a.txt"}));
        assert!(again.text.contains("two"), "{}", again.text);
    }

    #[test]
    fn search_can_list_only_the_files() {
        let f = fx(vec![]);
        std::fs::write(f.dir.path().join("a.py"), "def x():\n    pass\ndef y():\n    pass\n").unwrap();
        std::fs::write(f.dir.path().join("b.py"), "def z():\n    pass\n").unwrap();
        std::fs::write(f.dir.path().join("c.py"), "nothing\n").unwrap();
        let r = call(&f, "search_files", json!({"pattern":"^def ","files_only":true}));
        assert!(r.ok);
        assert!(r.text.contains("a.py: 2") && r.text.contains("b.py: 1"), "{}", r.text);
        assert!(!r.text.contains("c.py") && !r.text.contains("def x"), "{}", r.text);
        assert_eq!(r.summary, "3 matches in 2 files");
    }

    #[test]
    fn a_command_on_the_users_computer_is_checked_first() {
        let f = fx_with(
            ScriptedSandbox::new(vec![
                ExecResult {
                    exit_code: Some(0),
                    output: "fine".into(),
                    ..Default::default()
                },
                ExecResult {
                    exit_code: Some(0),
                    output: "installed".into(),
                    ..Default::default()
                },
            ])
            .with_policy(),
        );
        let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = asked.clone();
        f.ctx.approvals.set_responder(move |req| {
            log.lock().unwrap().push(format!("{} | {}", req.key, req.command));
            if req.key == "install:pip" {
                crate::permission::Decision::Allow
            } else {
                crate::permission::Decision::Deny
            }
        });
        // Refused for good, and nobody is asked.
        let r = call(&f, "run_command", json!({"command":"sudo rm -rf x"}));
        assert!(!r.ok);
        assert!(r.text.contains("blocked") && r.text.contains("sudo"), "{}", r.text);
        assert!(asked.lock().unwrap().is_empty());
        // The user says no.
        let r = call(&f, "run_command", json!({"command":"cat /etc/passwd"}));
        assert!(!r.ok);
        assert!(r.text.contains("did not allow") && r.text.contains("outside the workspace"), "{}", r.text);
        // Nothing reached the sandbox yet.
        assert_eq!(f.sandbox.commands.lock().unwrap().len(), 0);
        // An ordinary command runs without a question, an install after a yes.
        let r = call(&f, "run_command", json!({"command":"python main.py"}));
        assert!(r.ok, "{}", r.text);
        let r = call(&f, "run_command", json!({"command":"pip install rich"}));
        assert!(r.ok && r.text.contains("installed"), "{}", r.text);
        assert_eq!(f.sandbox.commands.lock().unwrap().len(), 2);
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].starts_with("install:pip | pip install"), "{asked:?}");
    }

    #[test]
    fn delete_and_unknown_tool() {
        let f = fx(vec![]);
        fs::write(f.ctx.work.root().join("x.txt"), "x").unwrap();
        assert!(call(&f, "delete_file", json!({"path":"x.txt"})).ok);
        assert!(!f.ctx.work.root().join("x.txt").exists());
        assert!(!call(&f, "delete_file", json!({"path":"x.txt"})).ok);
        let u = call(&f, "frobnicate", json!({}));
        assert!(!u.ok && u.text.contains("unknown tool"));
    }

    #[test]
    fn declarations_are_valid_for_gemini() {
        let d = declarations();
        let arr = d.as_array().unwrap();
        assert_eq!(arr.len(), 7);
        for t in arr {
            assert!(t["name"].is_string() && t["description"].is_string());
            assert_eq!(t["parameters"]["type"], "OBJECT");
            assert!(t["parameters"]["properties"]
                .as_object()
                .map(|p| !p.is_empty())
                .unwrap_or(false));
        }
    }

    #[test]
    fn descriptions_are_readable() {
        assert_eq!(
            describe_call(
                "read_file",
                &json!({"path":"a.py","start_line":3,"end_line":9})
            ),
            "Read a.py (lines 3-9)"
        );
        assert_eq!(
            describe_call("run_command", &json!({"command":"pytest -q\necho"})),
            "Run `pytest -q ...`"
        );
        assert_eq!(describe_call("list_files", &json!({})), "List files in .");
    }
}
