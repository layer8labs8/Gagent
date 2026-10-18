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

use crate::drive::CloudStore;
use crate::sandbox::{ExecRequest, Sandbox};
use crate::util::{human_bytes, looks_binary, truncate_end, truncate_middle, write_atomic};
use crate::workspace::{normalize_rel, walk_files, WalkOptions, Workspace};

/// Longest file the model may read or write in one call.
const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Files larger than this must be read in line ranges.
const MAX_FULL_READ_BYTES: u64 = 256 * 1024;
const DEFAULT_READ_LINES: usize = 2000;
const MAX_LIST_LINES: usize = 400;
const MAX_SEARCH_RESULTS: usize = 100;
/// Longest command output returned to the model, in characters.
const MAX_COMMAND_OUTPUT_CHARS: usize = 24_000;

/// Everything a tool needs to do its work.
pub struct ToolContext {
    pub work: Workspace,
    pub sandbox: Arc<dyn Sandbox>,
    pub cloud: Option<Arc<dyn CloudStore>>,
    pub opts: WalkOptions,
    pub default_timeout_secs: u32,
    pub cancel: Arc<AtomicBool>,
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

/// Function declarations sent to Gemini.
pub fn declarations(cloud_enabled: bool) -> Value {
    let mut tools = vec![
        json!({
            "name": "list_files",
            "description": "List files in the workspace as a flat list of relative paths. Hidden build and dependency folders (node_modules, target, .git, ...) are not shown.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": {"type": "STRING", "description": "Folder to list, relative to the workspace root. Default: the root."},
                    "depth": {"type": "INTEGER", "description": "How many folder levels to show below `path` (1-6). Default 2."}
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read a text file. Lines are returned with line numbers (`  12 | code`); the numbers are not part of the file. Use start_line/end_line for large files.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": {"type": "STRING", "description": "File path relative to the workspace root."},
                    "start_line": {"type": "INTEGER", "description": "First line to return (1-based)."},
                    "end_line": {"type": "INTEGER", "description": "Last line to return (inclusive)."}
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Create a file or completely replace its contents. Parent folders are created automatically. For small changes to an existing file use edit_file instead.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": {"type": "STRING", "description": "File path relative to the workspace root."},
                    "content": {"type": "STRING", "description": "The complete new contents of the file."}
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "edit_file",
            "description": "Replace text in an existing file. `old_text` must match the file exactly, including whitespace and indentation, and must be unique unless replace_all is true. Include enough surrounding lines to make it unique.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": {"type": "STRING", "description": "File path relative to the workspace root."},
                    "old_text": {"type": "STRING", "description": "The exact text to replace (without line numbers)."},
                    "new_text": {"type": "STRING", "description": "The replacement text."},
                    "replace_all": {"type": "BOOLEAN", "description": "Replace every occurrence instead of requiring a unique match."}
                },
                "required": ["path", "old_text", "new_text"]
            }
        }),
        json!({
            "name": "delete_file",
            "description": "Delete a file from the workspace.",
            "parameters": {
                "type": "OBJECT",
                "properties": { "path": {"type": "STRING", "description": "File path relative to the workspace root."} },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search_files",
            "description": "Search file contents with a regular expression. Returns `path:line: text` matches.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "pattern": {"type": "STRING", "description": "Regular expression (Rust regex syntax)."},
                    "path": {"type": "STRING", "description": "Folder to search in. Default: the whole workspace."},
                    "glob": {"type": "STRING", "description": "Only search files matching this glob, e.g. `*.py`."},
                    "ignore_case": {"type": "BOOLEAN", "description": "Case-insensitive search."}
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Run a shell command (bash) inside the Linux sandbox. The workspace is mounted at /workspace and is the working directory. Use it to run programs, tests, linters, installers (pip, npm) and compilers. Returns the exit code and the combined stdout/stderr. Commands must not wait for input.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "command": {"type": "STRING", "description": "The bash command line."},
                    "cwd": {"type": "STRING", "description": "Working directory relative to the workspace root. Default: the root."},
                    "timeout_secs": {"type": "INTEGER", "description": "Kill the command after this many seconds (default 120, max 900)."}
                },
                "required": ["command"]
            }
        }),
    ];
    if cloud_enabled {
        tools.push(json!({
            "name": "drive_list",
            "description": "List files and folders in the user's Google Drive storage for Gagent.",
            "parameters": {
                "type": "OBJECT",
                "properties": { "path": {"type": "STRING", "description": "Drive folder, relative to the Gagent folder. Default: the root."} }
            }
        }));
        tools.push(json!({
            "name": "drive_upload",
            "description": "Upload a workspace file to Google Drive (creates or replaces it).",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": {"type": "STRING", "description": "Workspace file to upload."},
                    "remote_path": {"type": "STRING", "description": "Destination path in Drive. Default: same as `path`."}
                },
                "required": ["path"]
            }
        }));
        tools.push(json!({
            "name": "drive_download",
            "description": "Download a file from Google Drive into the workspace.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "remote_path": {"type": "STRING", "description": "File path in Drive."},
                    "path": {"type": "STRING", "description": "Workspace destination. Default: same as `remote_path`."}
                },
                "required": ["remote_path"]
            }
        }));
    }
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
        "drive_list" => format!("List Drive folder {}", s(&["path"])),
        "drive_upload" => format!("Upload {} to Drive", s(&["path"])),
        "drive_download" => format!("Download {} from Drive", s(&["remote_path"])),
        other => format!("Call {other}"),
    }
}

/// Runs one tool call. Never panics on bad input; errors become text for the model.
pub fn execute(ctx: &ToolContext, name: &str, args: &Value) -> ToolOutput {
    let result = match name {
        "list_files" => list_files(ctx, args),
        "read_file" => read_file(ctx, args),
        "write_file" => write_file(ctx, args),
        "edit_file" => edit_file(ctx, args),
        "delete_file" => delete_file(ctx, args),
        "search_files" => search_files(ctx, args),
        "run_command" => run_command(ctx, args),
        "drive_list" => drive_list(ctx, args),
        "drive_upload" => drive_upload(ctx, args),
        "drive_download" => drive_download(ctx, args),
        other => Err(format!("unknown tool '{other}'")),
    };
    match result {
        Ok(out) => out,
        Err(e) => ToolOutput::err(format!("Error: {e}")),
    }
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
        (old.clone(), new.clone())
    } else if text.contains("\r\n") && !old.contains("\r\n") {
        (old.replace('\n', "\r\n"), new.replace('\n', "\r\n"))
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
    if results.is_empty() {
        return Ok(ToolOutput::ok("No matches.", "no matches", false));
    }
    let mut out = results.join("\n");
    if results.len() >= MAX_SEARCH_RESULTS {
        out.push_str(&format!(
            "\n[stopped after {MAX_SEARCH_RESULTS} matches; narrow the search]"
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
    let req = ExecRequest {
        command,
        cwd,
        timeout_secs: timeout,
    };
    let result = ctx
        .sandbox
        .exec(&req, &ctx.cancel)
        .map_err(|e| format!("{e:#}"))?;

    let (output, cut) = truncate_middle(result.output.trim_end(), MAX_COMMAND_OUTPUT_CHARS);
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

fn round1(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64())
}

// ---------------------------------------------------------------------------
// Drive tools
// ---------------------------------------------------------------------------

fn cloud(ctx: &ToolContext) -> Result<&Arc<dyn CloudStore>, String> {
    ctx.cloud
        .as_ref()
        .ok_or_else(|| "Google Drive is not connected".to_string())
}

fn drive_list(ctx: &ToolContext, args: &Value) -> ToolResult {
    let path = arg_str(args, &["path"]).unwrap_or_default();
    let entries = cloud(ctx)?.list(&path).map_err(|e| format!("{e:#}"))?;
    if entries.is_empty() {
        return Ok(ToolOutput::ok("(empty or missing folder)", "empty", false));
    }
    let lines: Vec<String> = entries
        .iter()
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.path)
            } else {
                format!("{}  ({})", e.path, human_bytes(e.size))
            }
        })
        .collect();
    Ok(ToolOutput::ok(
        lines.join("\n"),
        format!("{} entries", entries.len()),
        false,
    ))
}

fn drive_upload(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = required_str(args, &["path", "file_path"])?;
    let remote = arg_str(args, &["remote_path"]).unwrap_or_else(|| rel.clone());
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    let data = std::fs::read(&path).map_err(|_| format!("file '{rel}' does not exist"))?;
    let file = cloud(ctx)?
        .upload(&remote, &data)
        .map_err(|e| format!("{e:#}"))?;
    Ok(ToolOutput::ok(
        format!("Uploaded {rel} to Drive as {}", file.path),
        "uploaded",
        false,
    ))
}

fn drive_download(ctx: &ToolContext, args: &Value) -> ToolResult {
    let remote = required_str(args, &["remote_path"])?;
    let rel = arg_str(args, &["path"]).unwrap_or_else(|| remote.clone());
    let path = ctx.work.resolve(&rel).map_err(|e| e.to_string())?;
    let data = cloud(ctx)?
        .download(&remote)
        .map_err(|e| format!("{e:#}"))?;
    write_atomic(&path, &data).map_err(|e| format!("{e:#}"))?;
    Ok(ToolOutput::ok(
        format!(
            "Downloaded {remote} to {rel} ({})",
            human_bytes(data.len() as u64)
        ),
        "downloaded",
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{ExecResult, ScriptedSandbox};
    use std::fs;

    struct Fx {
        _dir: tempfile::TempDir,
        ctx: ToolContext,
        sandbox: Arc<ScriptedSandbox>,
    }

    fn fx(results: Vec<ExecResult>) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(ScriptedSandbox::new(results));
        let ctx = ToolContext {
            work: Workspace::new(dir.path()).unwrap(),
            sandbox: sandbox.clone(),
            cloud: None,
            opts: WalkOptions::default(),
            default_timeout_secs: 60,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        Fx {
            _dir: dir,
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
        let r = call(&f, "read_file", json!({"path":"src/app.py"}));
        assert!(r.ok);
        assert!(r.text.contains("   1 | a = 1"), "{}", r.text);
        assert!(r.text.contains("   3 | c = 3"));
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
        let w2 = call(&f, "write_file", json!({"path":"src/app.py","content":"x"}));
        assert!(w2.text.contains("Overwrote"));
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
        let big = "line\n".repeat(20_000);
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
        let d = declarations(false);
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
        assert_eq!(declarations(true).as_array().unwrap().len(), 10);
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

    #[derive(Default)]
    struct MemoryCloud {
        files: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
    }

    impl CloudStore for MemoryCloud {
        fn list(&self, _dir: &str) -> anyhow::Result<Vec<crate::drive::RemoteFile>> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(p, d)| crate::drive::RemoteFile {
                    id: p.clone(),
                    name: p.clone(),
                    path: p.clone(),
                    is_dir: false,
                    size: d.len() as u64,
                    modified: String::new(),
                    sha256: None,
                })
                .collect())
        }
        fn upload(&self, path: &str, data: &[u8]) -> anyhow::Result<crate::drive::RemoteFile> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_string(), data.to_vec());
            Ok(crate::drive::RemoteFile {
                id: path.into(),
                name: path.into(),
                path: path.into(),
                is_dir: false,
                size: data.len() as u64,
                modified: String::new(),
                sha256: None,
            })
        }
        fn download(&self, path: &str) -> anyhow::Result<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found"))
        }
    }

    #[test]
    fn drive_tools_round_trip() {
        let mut f = fx(vec![]);
        f.ctx.cloud = Some(Arc::new(MemoryCloud::default()));
        fs::write(f.ctx.work.root().join("data.csv"), "a,b\n1,2\n").unwrap();
        assert!(
            call(
                &f,
                "drive_upload",
                json!({"path":"data.csv","remote_path":"backup/data.csv"})
            )
            .ok
        );
        let l = call(&f, "drive_list", json!({}));
        assert!(l.text.contains("backup/data.csv"));
        let d = call(
            &f,
            "drive_download",
            json!({"remote_path":"backup/data.csv","path":"restored/data.csv"}),
        );
        assert!(d.ok, "{}", d.text);
        assert_eq!(
            fs::read_to_string(f.ctx.work.root().join("restored/data.csv")).unwrap(),
            "a,b\n1,2\n"
        );
        assert!(!call(&f, "drive_download", json!({"remote_path":"missing"})).ok);

        let g = fx(vec![]);
        assert!(!call(&g, "drive_list", json!({})).ok);
    }
}
