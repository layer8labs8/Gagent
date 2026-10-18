//! Small helpers shared by the whole crate.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A short unique identifier, safe to use in file and container names.
pub fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:04x}", nanos / 1_000, fastrand::u16(..))
}

/// Shortens `text` to at most `max_chars` characters by removing the middle.
/// Returns the new text and whether anything was removed.
pub fn truncate_middle(text: &str, max_chars: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max_chars {
        return (text.to_string(), false);
    }
    let head = max_chars / 3;
    let tail = max_chars - head;
    let head_str: String = text.chars().take(head).collect();
    let tail_str: String = text.chars().skip(total - tail).collect();
    let removed = total - head - tail;
    (
        format!("{head_str}\n[... {removed} characters omitted ...]\n{tail_str}"),
        true,
    )
}

/// Keeps at most `max_chars` characters from the start of `text`.
pub fn truncate_end(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Removes ANSI escape sequences (colours, cursor movement) from terminal output.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c == '\r' {
            // Progress bars rewrite the same line; keep only the final state.
            if chars.peek() != Some(&'\n') {
                if let Some(pos) = out.rfind('\n') {
                    out.truncate(pos + 1);
                } else {
                    out.clear();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Formats a byte count like `1.5 MB`.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Returns true when the bytes look like a binary (non-text) file.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    sample.contains(&0)
}

/// Writes a file atomically: data goes to a temporary sibling that is then renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = dir.join(format!(".gagent-tmp-{}", unique_id()));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all().ok();
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot write {}", path.display()));
    }
    Ok(())
}

/// Converts a path to a forward-slash string, regardless of platform.
pub fn to_slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Hex-encoded SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Formats a Unix timestamp in the local time zone.
pub fn format_local(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%b %e, %H:%M").to_string(),
        None => String::new(),
    }
}

/// Formats a Unix timestamp as a local clock time, e.g. `14:03:27`.
pub fn format_clock(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text() {
        let (t, cut) = truncate_middle("hello", 10);
        assert_eq!(t, "hello");
        assert!(!cut);
    }

    #[test]
    fn truncate_removes_middle() {
        let text = "a".repeat(100) + &"b".repeat(100);
        let (t, cut) = truncate_middle(&text, 60);
        assert!(cut);
        assert!(t.contains("characters omitted"));
        assert!(t.starts_with('a'));
        assert!(t.trim_end().ends_with('b'));
    }

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m text"), "red text");
        assert_eq!(strip_ansi("10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(strip_ansi("line1\r\nline2"), "line1\nline2");
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }

    #[test]
    fn binary_detection() {
        assert!(looks_binary(&[1, 2, 0, 3]));
        assert!(!looks_binary(b"plain text\n"));
    }

    #[test]
    fn atomic_write_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/c.txt");
        write_atomic(&p, b"x").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x");
    }

    #[test]
    fn sha_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
