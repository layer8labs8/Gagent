//! Speech to text and text to speech against a scripted Gemini server.

mod support;

use std::sync::Mutex;

use gagent::config::Settings;
use gagent::speech;
use gagent::vision::base64_encode;
use serde_json::json;
use support::*;

/// The tests change the environment; one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn gemini_only(mock: &MockGemini) -> Settings {
    std::env::set_var("GAGENT_GEMINI_BASE_URL", &mock.base_url);
    std::env::remove_var("GROQ_API_KEY");
    std::env::remove_var("GEMINI_API_KEY");
    let mut s = Settings::default();
    s.api_key = "GEMINI-KEY".into();
    s.groq.api_key = String::new();
    s
}

#[test]
fn speech_is_written_down_by_gemini_when_groq_is_not_there() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![text("Salam, bu bir sınaqdır")]);
    let settings = gemini_only(&mock);
    let wav = speech::pcm_to_wav(&[0u8; 8000], 16_000);
    let said = speech::transcribe(&settings, &wav, "az").unwrap();
    assert_eq!(said, "Salam, bu bir sınaqdır");
    let sent = mock.received()[0].body.to_string();
    assert!(sent.contains("audio/wav") && sent.contains("inlineData"), "{sent}");
    assert!(sent.contains("The language is az"), "{sent}");
}

#[test]
fn an_answer_is_read_aloud_as_a_wav_file() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let pcm = vec![1u8, 2, 3, 4, 5, 6];
    let mock = MockGemini::start(vec![Canned::ok(json!({
        "candidates": [{ "content": { "parts": [{ "inlineData": {
            "mimeType": "audio/L16;codec=pcm;rate=24000",
            "data": base64_encode(&pcm),
        }}]}}]
    }))]);
    let settings = gemini_only(&mock);
    let wav = speech::speak(&settings, "I changed **two** files.\n```\ncode\n```").unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(wav.len(), 44 + pcm.len());
    assert_eq!(&wav[44..], &pcm[..]);
    let sent = mock.received()[0].body.to_string();
    assert!(sent.contains("I changed two files."), "{sent}");
    assert!(!sent.contains("code\\n"), "code is not read aloud: {sent}");
    assert!(sent.contains("AUDIO"), "{sent}");
}

#[test]
fn a_service_that_says_no_is_explained() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mock = MockGemini::start(vec![Canned::status(404, "model is gone")]);
    let settings = gemini_only(&mock);
    let wav = speech::pcm_to_wav(&[0u8; 8000], 16_000);
    let error = speech::transcribe(&settings, &wav, "").unwrap_err();
    assert!(error.contains("Could not turn the speech into text"), "{error}");
    assert!(error.contains("not found"), "{error}");
    // Nothing to read aloud, or no key: said at once, no request.
    assert!(speech::speak(&settings, "   ").unwrap_err().contains("nothing to read"));
    let mut no_key = settings.clone();
    no_key.api_key = String::new();
    assert!(speech::speak(&no_key, "hello").unwrap_err().contains("Gemini key"));
}
