//! Speech: the user's voice becomes text, and an answer can be read aloud.
//!
//! Recording and playing use what the system already has (PowerShell on Windows, `arecord` and
//! `aplay` on Linux), so nothing heavy is added to Gagent. Speech to text is done by Groq's
//! Whisper when the account may use it, and otherwise by a Gemini model; speech is made by a
//! Gemini voice model. The recording is deleted as soon as it was sent; nothing is kept.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::{Provider, Settings};
use crate::http::Http;
use crate::vision::base64_encode;

/// The longest recording (seconds): a long one costs time and tokens.
pub const MAX_RECORD_SECS: u64 = 120;
/// The most that is read aloud in one go.
const MAX_SPOKEN_CHARS: usize = 1200;
const GROQ_WHISPER: &str = "whisper-large-v3-turbo";
/// Gemini models that understand speech, best first.
const GEMINI_LISTENERS: &[&str] = &[
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3.1-flash-lite",
    "gemini-flash-latest",
];
/// Gemini models that speak, best first.
const GEMINI_SPEAKERS: &[&str] = &[
    "gemini-2.5-flash-preview-tts",
    "gemini-3.1-flash-tts-preview",
    "gemini-3.8-flash-tts",
];

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A WAV file (16-bit, mono) around raw PCM samples.
pub fn pcm_to_wav(pcm: &[u8], rate: u32) -> Vec<u8> {
    let len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Reads base64 text (the standard alphabet; line breaks and padding are fine).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The sample rate in a mime type such as `audio/L16;codec=pcm;rate=24000`.
fn rate_of(mime: &str) -> u32 {
    mime.split(';')
        .find_map(|p| p.trim().strip_prefix("rate="))
        .and_then(|r| r.parse().ok())
        .unwrap_or(24_000)
}

/// The text of an answer in a form that sounds right: no code, no marks, short.
pub fn speakable(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !in_code {
                out.push_str("(code omitted) ");
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let cleaned: String = line
            .trim_start_matches(['#', '>', '-', '*', ' '])
            .chars()
            .filter(|c| !matches!(c, '*' | '`' | '_' | '|'))
            .collect();
        let cleaned = cleaned
            .split_whitespace()
            .map(|w| if w.starts_with("http://") || w.starts_with("https://") { "(link)" } else { w })
            .collect::<Vec<_>>()
            .join(" ");
        if !cleaned.is_empty() {
            out.push_str(&cleaned);
            out.push(' ');
        }
    }
    let out = out.trim().to_string();
    if out.chars().count() <= MAX_SPOKEN_CHARS {
        return out;
    }
    let cut: String = out.chars().take(MAX_SPOKEN_CHARS).collect();
    match cut.rfind(['.', '!', '?']) {
        Some(i) if i > MAX_SPOKEN_CHARS / 2 => cut[..=i].to_string(),
        _ => cut,
    }
}

/// The body of a `multipart/form-data` request with text fields and one file.
fn multipart(fields: &[(&str, &str)], file: (&str, &str, &str, &[u8])) -> (String, Vec<u8>) {
    let boundary = format!("gagent{}", crate::util::unique_id());
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    let (field, file_name, mime, bytes) = file;
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file_name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A model of `prefs` that the service lists (best first); the last one when the list is not known.
fn pick_model(host: &str, prefs: &[&str]) -> String {
    if let Some(live) = crate::modelstats::live_models(host) {
        if let Some(found) = prefs.iter().find(|p| {
            live.iter().any(|m| m == *p)
                && crate::modelstats::blocked_for(host, p).is_none()
                && !crate::modelstats::is_missing(host, p)
        }) {
            return (*found).to_string();
        }
    }
    prefs.last().copied().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------
// Speech to text
// ---------------------------------------------------------------------------

/// What the person said in `wav`. Groq's Whisper first (when the account may use it), then Gemini.
/// `language` is a code like `az` or `tr`, or empty to let the service find out.
pub fn transcribe(settings: &Settings, wav: &[u8], language: &str) -> Result<String, String> {
    let mut problems = Vec::new();
    match transcribe_groq(settings, wav, language) {
        Ok(text) => return Ok(text),
        Err(e) => problems.push(e),
    }
    match transcribe_gemini(settings, wav, language) {
        Ok(text) => Ok(text),
        Err(e) => {
            problems.push(e);
            Err(format!(
                "Could not turn the speech into text. {}",
                problems.join(" | ")
            ))
        }
    }
}

fn transcribe_groq(settings: &Settings, wav: &[u8], language: &str) -> Result<String, String> {
    let mut s = settings.clone();
    s.provider = Provider::Groq;
    let key = s.effective_api_key();
    if key.is_empty() {
        return Err("Groq has no key.".into());
    }
    let base = s.api_base_url();
    let host = crate::ratelimit::host_of(&base);
    if crate::modelstats::blocked_for(&host, GROQ_WHISPER).is_some() {
        return Err("Groq's Whisper is not enabled in your Groq project.".into());
    }
    let mut fields = vec![("model", GROQ_WHISPER), ("response_format", "json"), ("temperature", "0")];
    if !language.trim().is_empty() {
        fields.push(("language", language.trim()));
    }
    let (content_type, body) = multipart(&fields, ("file", "voice.wav", "audio/wav", wav));
    let auth = format!("Bearer {key}");
    let response = Http::new(Duration::from_secs(120), 8 << 20)
        .send(
            "POST",
            &format!("{base}/audio/transcriptions"),
            &[("Authorization", auth.as_str())],
            Some(&content_type),
            &body,
        )
        .map_err(|e| e.to_string())?;
    if !response.is_success() {
        let error = crate::gemini::http_error(&response);
        crate::gemini::note_failure(&host, GROQ_WHISPER, &error);
        return Err(error.user_message());
    }
    let text = response.json()?["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        Err("Groq heard nothing.".into())
    } else {
        Ok(text)
    }
}

fn transcribe_gemini(settings: &Settings, wav: &[u8], language: &str) -> Result<String, String> {
    let mut s = settings.clone();
    s.provider = Provider::Gemini;
    let key = s.effective_api_key();
    if key.is_empty() {
        return Err("Gemini has no key.".into());
    }
    let base = s.api_base_url();
    let host = crate::ratelimit::host_of(&base);
    let model = pick_model(&host, GEMINI_LISTENERS);
    let hint = if language.trim().is_empty() {
        "The speaker may use Azerbaijani, Turkish, English or Russian. Write each word in the language it was said, with that language's own letters.".to_string()
    } else {
        format!("The language is {}. Write it with its own letters.", language.trim())
    };
    let body = json!({
        "contents": [{ "role": "user", "parts": [
            { "text": format!("Write down exactly what is said in this recording. Reply with the spoken words only, no comments. {hint}") },
            { "inlineData": { "mimeType": "audio/wav", "data": base64_encode(wav) } },
        ]}],
    });
    let response = Http::new(Duration::from_secs(180), 8 << 20)
        .post_json(
            &format!("{base}/models/{model}:generateContent"),
            &[("x-goog-api-key", key.as_str())],
            &body,
        )
        .map_err(|e| e.to_string())?;
    if !response.is_success() {
        let error = crate::gemini::http_error(&response);
        crate::gemini::note_failure(&host, &model, &error);
        return Err(error.user_message());
    }
    let value = response.json()?;
    let text: String = value["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    let text = text.trim().to_string();
    if text.is_empty() {
        Err("Gemini heard nothing.".into())
    } else {
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Text to speech
// ---------------------------------------------------------------------------

/// The text as a WAV file, spoken by a Gemini voice.
pub fn speak(settings: &Settings, text: &str) -> Result<Vec<u8>, String> {
    let spoken = speakable(text);
    if spoken.is_empty() {
        return Err("There is nothing to read aloud.".into());
    }
    let mut s = settings.clone();
    s.provider = Provider::Gemini;
    let key = s.effective_api_key();
    if key.is_empty() {
        return Err("Reading aloud needs a Gemini key.".into());
    }
    let base = s.api_base_url();
    let host = crate::ratelimit::host_of(&base);
    let model = pick_model(&host, GEMINI_SPEAKERS);
    let body = json!({
        "contents": [{ "parts": [{ "text": spoken }] }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": "Kore" } } },
        },
    });
    let response = Http::new(Duration::from_secs(180), 32 << 20)
        .post_json(
            &format!("{base}/models/{model}:generateContent"),
            &[("x-goog-api-key", key.as_str())],
            &body,
        )
        .map_err(|e| e.to_string())?;
    if !response.is_success() {
        let error = crate::gemini::http_error(&response);
        crate::gemini::note_failure(&host, &model, &error);
        return Err(error.user_message());
    }
    let value = response.json()?;
    let part = &value["candidates"][0]["content"]["parts"][0]["inlineData"];
    let data = part["data"].as_str().ok_or("The voice model sent no sound.")?;
    let pcm = base64_decode(data).ok_or("The voice model sent damaged sound.")?;
    let mime = part["mimeType"].as_str().unwrap_or("");
    if mime.contains("wav") {
        return Ok(pcm);
    }
    Ok(pcm_to_wav(&pcm, rate_of(mime)))
}

// ---------------------------------------------------------------------------
// Microphone and loudspeaker (with the tools of the system)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn quiet(command: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000)
}

#[cfg(not(windows))]
fn quiet(command: &mut Command) -> &mut Command {
    command
}

/// A recording in progress.
pub struct Recorder {
    child: Child,
    out: PathBuf,
    stop_file: PathBuf,
    started: Instant,
}

impl Recorder {
    /// Starts recording from the microphone into a file in `dir`.
    pub fn start(dir: &Path) -> Result<Recorder, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
        let out = dir.join("voice.wav");
        let stop_file = dir.join("voice.stop");
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&stop_file);
        let child = spawn_recorder(&out, &stop_file)?;
        Ok(Recorder {
            child,
            out,
            stop_file,
            started: Instant::now(),
        })
    }

    pub fn seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Stops and returns the recording as a WAV file. The file on disk is deleted.
    pub fn stop(mut self) -> Result<Vec<u8>, String> {
        #[cfg(windows)]
        {
            let _ = std::fs::write(&self.stop_file, b"stop");
        }
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .arg("-INT")
                .arg(self.child.id().to_string())
                .status();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = self.child.kill();
                    break;
                }
            }
        }
        let mut said = String::new();
        if let Some(mut out) = self.child.stdout.take() {
            let _ = out.read_to_string(&mut said);
        }
        let bytes = std::fs::read(&self.out);
        let _ = std::fs::remove_file(&self.out);
        let _ = std::fs::remove_file(&self.stop_file);
        if said.contains("ERR") {
            return Err(format!(
                "The microphone could not be used ({}). Is one connected, and allowed for desktop apps (Windows: Settings, Privacy, Microphone)?",
                said.trim()
            ));
        }
        match bytes {
            Ok(b) if b.len() > 4000 => Ok(b),
            Ok(_) => Err("The recording is empty or too short.".into()),
            Err(_) => Err("Nothing was recorded. Is a microphone connected and allowed for desktop apps?".into()),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(windows)]
fn spawn_recorder(out: &Path, stop_file: &Path) -> Result<Child, String> {
    let quote = |p: &Path| p.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
Add-Type -TypeDefinition 'using System.Runtime.InteropServices; using System.Text; public class Mci {{ [DllImport(\"winmm.dll\", CharSet=CharSet.Auto)] public static extern int mciSendString(string c, StringBuilder r, int n, System.IntPtr h); }}'; \
function Send($c) {{ return [Mci]::mciSendString($c, $null, 0, [System.IntPtr]::Zero) }}; \
$r = Send 'open new type waveaudio alias gagentrec'; \
if ($r -ne 0) {{ Write-Output \"ERR open $r\"; exit 1 }}; \
foreach ($c in 'set gagentrec time format ms','set gagentrec bitspersample 16','set gagentrec channels 1','set gagentrec samplespersec 16000','set gagentrec alignment 2','set gagentrec bytespersec 32000') {{ Send $c | Out-Null }}; \
$r = Send 'record gagentrec'; \
if ($r -ne 0) {{ Write-Output \"ERR record $r\"; exit 1 }}; \
$sw = [Diagnostics.Stopwatch]::StartNew(); \
while (-not (Test-Path -LiteralPath '{stop}') -and $sw.Elapsed.TotalSeconds -lt {max}) {{ Start-Sleep -Milliseconds 100 }}; \
Send 'stop gagentrec' | Out-Null; \
$r = Send ('save gagentrec \"' + '{out}' + '\"'); \
Send 'close gagentrec' | Out-Null; \
if ($r -ne 0) {{ Write-Output \"ERR save $r\"; exit 1 }}; \
Write-Output 'OK'",
        stop = quote(stop_file),
        out = quote(out),
        max = MAX_RECORD_SECS,
    );
    // -EncodedCommand takes the script as base64 of UTF-16, so no quoting can go wrong.
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut command = Command::new("powershell");
    quiet(&mut command)
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &base64_encode(&utf16)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
        .spawn()
        .map_err(|e| format!("could not start the recorder (PowerShell): {e}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_recorder(out: &Path, _stop_file: &Path) -> Result<Child, String> {
    Command::new("arecord")
        .args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "wav", "-d"])
        .arg(MAX_RECORD_SECS.to_string())
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Recording needs `arecord` (Ubuntu: sudo apt install alsa-utils).".to_string()
            } else {
                format!("could not start arecord: {e}")
            }
        })
}

#[cfg(target_os = "macos")]
fn spawn_recorder(_out: &Path, _stop_file: &Path) -> Result<Child, String> {
    Err("Recording from the microphone is not supported on macOS yet.".into())
}

/// A sound being played.
pub struct Playback {
    child: Child,
}

impl Playback {
    pub fn stop(&mut self) {
        let _ = self.child.kill();
    }

    pub fn is_done(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

/// Plays a WAV file with the player of the system.
pub fn play(wav: &[u8], dir: &Path) -> Result<Playback, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
    let path = dir.join("speech.wav");
    std::fs::write(&path, wav).map_err(|e| format!("cannot write the sound: {e}"))?;
    let mut command;
    if cfg!(windows) {
        let quoted = path.to_string_lossy().replace('\'', "''");
        command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(New-Object Media.SoundPlayer '{quoted}').PlaySync()"),
        ]);
    } else if cfg!(target_os = "macos") {
        command = Command::new("afplay");
        command.arg(&path);
    } else {
        command = Command::new("aplay");
        command.arg("-q").arg(&path);
    }
    quiet(&mut command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().map_err(|e| format!("could not play the sound: {e}"))?;
    Ok(Playback { child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_gets_a_wav_header() {
        let wav = pcm_to_wav(&[1, 2, 3, 4], 24_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
        assert_eq!(rate_of("audio/L16;codec=pcm;rate=16000"), 16_000);
        assert_eq!(rate_of("audio/L16"), 24_000);
    }

    #[test]
    fn base64_goes_there_and_back() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"\x00\xff\x10\x80 sound", &[7u8; 1000]] {
            assert_eq!(base64_decode(&base64_encode(bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_decode("aGk=\n").unwrap(), b"hi");
        assert!(base64_decode("a$b").is_none());
    }

    #[test]
    fn an_answer_is_made_ready_for_speaking() {
        let text = "# Done\nI changed **two** files, see https://example.com/x for more.\n```python\nprint('hi')\n```\n- tested `it`";
        let s = speakable(text);
        assert!(!s.contains('*') && !s.contains('`') && !s.contains("print"), "{s}");
        assert!(s.contains("(link)") && s.contains("(code omitted)"), "{s}");
        assert!(s.contains("I changed two files"), "{s}");
        let long = "This is a sentence. ".repeat(200);
        let cut = speakable(&long);
        assert!(cut.chars().count() <= MAX_SPOKEN_CHARS && cut.ends_with('.'), "{}", cut.len());
        assert_eq!(speakable("```\ncode only\n```"), "(code omitted)");
    }

    #[test]
    fn the_form_has_fields_and_a_file() {
        let (content_type, body) = multipart(
            &[("model", "whisper"), ("language", "az")],
            ("file", "voice.wav", "audio/wav", b"RIFFdata"),
        );
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper\r\n")));
        assert!(text.contains("name=\"file\"; filename=\"voice.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFFdata\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }
}
