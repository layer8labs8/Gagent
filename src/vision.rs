//! Pictures for models that can see: loading image files, taking a screenshot of a page with
//! the browser that is already on the computer, and keeping pictures out of the stored history.
//!
//! A picture costs a few thousand tokens every time it is sent, so only the last few messages
//! keep theirs, and saved sessions keep none.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Largest picture file the agent may look at.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// Pictures the agent may look at in one task.
pub const MAX_IMAGES_PER_TASK: u32 = 12;
/// How many characters a picture counts for when the size of the history is measured.
const IMAGE_COST_CHARS: usize = 6_000;

/// A picture ready to be sent to a model.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub mime: &'static str,
    /// Base64 text of the file.
    pub data: String,
}

impl Image {
    /// The `inlineData` part used in the stored conversation.
    pub fn part(&self) -> Value {
        json!({ "inlineData": { "mimeType": self.mime, "data": self.data } })
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The picture type from the first bytes of a file.
pub fn mime_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Width and height of a PNG file.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || mime_of(bytes) != Some("image/png") {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// Reads a picture file.
pub fn load(path: &Path) -> Result<Image, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read the picture: {e}"))?;
    if meta.len() as usize > MAX_IMAGE_BYTES {
        return Err(format!(
            "the picture is {} MB; the limit is {} MB. Use a smaller picture or take a smaller screenshot",
            meta.len() / (1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read the picture: {e}"))?;
    let mime = mime_of(&bytes)
        .ok_or_else(|| "this is not a PNG, JPEG, GIF or WebP picture".to_string())?;
    Ok(Image {
        mime,
        data: base64_encode(&bytes),
    })
}

// ---------------------------------------------------------------------------
// Screenshots
// ---------------------------------------------------------------------------

/// A browser that can take a screenshot without a window: Edge, Chrome, Chromium, Brave.
pub fn find_browser() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("GAGENT_BROWSER").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
                candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
                candidates.push(base.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        for p in [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ] {
            candidates.push(PathBuf::from(p));
        }
    } else {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "brave-browser",
        ] {
            if let Some(p) = crate::sandbox::find_on_path(name) {
                candidates.push(p);
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// `C:\a b\x.html` -> `file:///C:/a%20b/x.html`.
pub fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// True for the addresses a screenshot may open besides files of the workspace: a server the
/// agent started on this computer.
pub fn is_local_url(url: &str) -> bool {
    ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|p| {
            url.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/', '?', '#']))
        })
}

/// Opens `url` in the browser without a window and saves a PNG of it to `out`.
pub fn render(
    browser: &Path,
    url: &str,
    out: &Path,
    profile: &Path,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create the folder: {e}"))?;
    }
    std::fs::create_dir_all(profile).map_err(|e| format!("cannot create the folder: {e}"))?;
    let _ = std::fs::remove_file(out);
    let mut cmd = Command::new(browser);
    cmd.arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        // A profile of its own: otherwise a browser that is already open takes the request and
        // this process ends at once without a screenshot.
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={width},{height}"))
        .arg("--virtual-time-budget=4000")
        .arg(format!("--screenshot={}", out.display()))
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window, below normal priority.
        cmd.creation_flags(0x0800_0000 | 0x0000_4000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the browser: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(format!("the browser failed: {e}")),
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the browser did not finish in 45 seconds".to_string());
        }
        thread::sleep(Duration::from_millis(100));
    }
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err("the browser did not save a screenshot (is the address right?)".to_string()),
    }
}

// ---------------------------------------------------------------------------
// The stored conversation
// ---------------------------------------------------------------------------

const OMITTED: &str = "[a picture was here; it is not kept to save space]";

fn is_image(part: &Value) -> bool {
    part.get("inlineData").is_some_and(Value::is_object)
}

/// A copy of the conversation without any picture, for saving to disk.
pub fn without_images(history: &[Value]) -> Vec<Value> {
    history
        .iter()
        .map(|message| {
            let mut message = message.clone();
            if let Some(parts) = message["parts"].as_array_mut() {
                for part in parts.iter_mut().filter(|p| is_image(p)) {
                    *part = json!({ "text": OMITTED });
                }
            }
            message
        })
        .collect()
}

/// Replaces the pictures of all but the last `keep_last` messages.
pub fn drop_old_images(history: &mut [Value], keep_last: usize) {
    let limit = history.len().saturating_sub(keep_last);
    for message in history.iter_mut().take(limit) {
        if let Some(parts) = message["parts"].as_array_mut() {
            for part in parts.iter_mut().filter(|p| is_image(p)) {
                *part = json!({ "text": OMITTED });
            }
        }
    }
}

/// Size of the conversation in characters, counting each picture as a fixed amount instead
/// of the length of its text.
pub fn history_chars(history: &[Value]) -> usize {
    history
        .iter()
        .map(|message| {
            let mut size = message.to_string().len();
            if let Some(parts) = message["parts"].as_array() {
                for part in parts.iter().filter(|p| is_image(p)) {
                    let data = part["inlineData"]["data"].as_str().map_or(0, str::len);
                    size = size.saturating_sub(data) + IMAGE_COST_CHARS;
                }
            }
            size
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_3X2: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xFB, 0xFF, 0xFE]), "+//+");
    }

    #[test]
    fn picture_types_come_from_the_first_bytes() {
        assert_eq!(mime_of(PNG_3X2), Some("image/png"));
        assert_eq!(mime_of(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(mime_of(b"GIF89a.."), Some("image/gif"));
        assert_eq!(mime_of(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(mime_of(b"<html>"), None);
        assert_eq!(png_size(PNG_3X2), Some((3, 2)));
    }

    #[test]
    fn a_picture_file_is_loaded_and_a_text_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), PNG_3X2).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let image = load(&dir.path().join("a.png")).unwrap();
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.part()["inlineData"]["mimeType"], "image/png");
        assert!(load(&dir.path().join("a.txt")).unwrap_err().contains("not a PNG"));
        assert!(load(&dir.path().join("missing.png")).is_err());
    }

    #[test]
    fn a_real_browser_takes_a_screenshot_when_there_is_one() {
        let Some(browser) = find_browser() else {
            eprintln!("SKIPPED: no browser on this computer");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("page.html");
        std::fs::write(
            &page,
            "<html><body style='background:#123456'><h1 style='color:white'>Gagent</h1></body></html>",
        )
        .unwrap();
        let out = dir.path().join("shots").join("one.png");
        let profile = dir.path().join("profile");
        if let Err(why) = render(
            &browser,
            &file_url(&page),
            &out,
            &profile,
            640,
            480,
            &AtomicBool::new(false),
        ) {
            eprintln!("SKIPPED: the browser could not draw here: {why}");
            return;
        }
        let image = load(&out).unwrap();
        assert_eq!(image.mime, "image/png");
        let (w, h) = png_size(&std::fs::read(&out).unwrap()).unwrap();
        assert!(w >= 600 && h >= 400, "{w}x{h}");
    }

    #[test]
    fn file_addresses_are_written_for_browsers() {
        assert_eq!(file_url(Path::new(r"C:\Users\me\a b\x.html")), "file:///C:/Users/me/a%20b/x.html");
        assert_eq!(file_url(Path::new("/home/me/x#1.html")), "file:///home/me/x%231.html");
    }

    #[test]
    fn only_servers_on_this_computer_may_be_opened() {
        assert!(is_local_url("http://localhost:8080/index.html"));
        assert!(is_local_url("http://127.0.0.1:3000"));
        assert!(!is_local_url("http://localhost.evil.example/"));
        assert!(!is_local_url("https://example.com/"));
        assert!(!is_local_url("file:///etc/passwd"));
    }

    #[test]
    fn pictures_leave_the_history_when_it_is_saved_and_when_it_gets_old() {
        let image = Image {
            mime: "image/png",
            data: "A".repeat(100_000),
        };
        let mut history = vec![
            json!({"role":"user","parts":[{"text":"look"}]}),
            json!({"role":"user","parts":[{"functionResponse":{"name":"screenshot","response":{"output":"ok"}}}, image.part()]}),
            json!({"role":"model","parts":[{"text":"seen"}]}),
            json!({"role":"user","parts":[image.part()]}),
        ];
        // Each picture counts for a fixed amount, not for its 100 000 characters.
        assert!(history_chars(&history) < 20_000, "{}", history_chars(&history));
        let saved = without_images(&history);
        assert!(!saved.iter().any(|m| m.to_string().contains("inlineData")));
        assert!(saved[1].to_string().contains("not kept"));
        assert!(history[1].to_string().contains("inlineData"), "the live history is untouched");
        drop_old_images(&mut history, 1);
        assert!(!history[1].to_string().contains("inlineData"));
        assert!(history[3].to_string().contains("inlineData"), "the newest message keeps its picture");
    }
}
