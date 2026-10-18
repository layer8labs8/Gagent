//! The Google Drive client against an in-memory fake of the Drive and OAuth endpoints.

use std::collections::HashMap;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use gagent::config::DriveSettings;
use gagent::drive::{start_device_flow, wait_for_approval, CloudStore, DriveClient, Endpoints};
use gagent::workspace::WalkOptions;
use regex::Regex;
use serde_json::{json, Value};

#[derive(Clone, Debug)]
struct Item {
    id: String,
    name: String,
    parent: String,
    is_dir: bool,
    data: Vec<u8>,
    props: Value,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    next_id: u32,
    token_calls: u32,
    /// Answer the first Drive request with 401 to force a token refresh.
    reject_first_token: bool,
    device_polls: u32,
    log: Vec<String>,
}

struct Fake {
    base: String,
    state: Arc<Mutex<State>>,
}

fn unquote(s: &str) -> String {
    s.replace("\\'", "'").replace("\\\\", "\\")
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 + 0 => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| url_decode(v))
    })
}

fn split_multipart(content_type: &str, body: &[u8]) -> (Value, Vec<u8>) {
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .unwrap()
        .trim()
        .to_string();
    let marker = format!("--{boundary}");
    let text_positions: Vec<usize> = body
        .windows(marker.len())
        .enumerate()
        .filter(|(_, w)| *w == marker.as_bytes())
        .map(|(i, _)| i)
        .collect();
    assert!(
        text_positions.len() >= 3,
        "expected two parts and a closing marker"
    );
    let part = |from: usize, to: usize| -> &[u8] {
        let chunk = &body[from + marker.len()..to];
        let sep = chunk.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let content = &chunk[sep + 4..];
        &content[..content.len() - 2] // trailing CRLF before the next marker
    };
    let meta: Value = serde_json::from_slice(part(text_positions[0], text_positions[1])).unwrap();
    let data = part(text_positions[1], text_positions[2]).to_vec();
    (meta, data)
}

impl Fake {
    fn start() -> Fake {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = Vec::new();
                let _ = std::io::Read::read_to_end(req.as_reader(), &mut body);
                let url = req.url().to_string();
                let method = req.method().to_string();
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("authorization"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let ctype = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("content-type"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let (status, reply) =
                    handle(&mut st.lock().unwrap(), &method, &url, &auth, &ctype, &body);
                let bytes = match reply {
                    Reply::Json(v) => v.to_string().into_bytes(),
                    Reply::Raw(b) => b,
                };
                let _ = req.respond(tiny_http::Response::from_data(bytes).with_status_code(status));
            }
        });
        Fake {
            base: format!("http://127.0.0.1:{port}"),
            state,
        }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            api: format!("{}/drive/v3", self.base),
            upload: format!("{}/upload/drive/v3", self.base),
            device_code: format!("{}/device/code", self.base),
            token: format!("{}/token", self.base),
        }
    }

    fn client(&self) -> DriveClient {
        DriveClient::with_endpoints(&settings(), self.endpoints()).unwrap()
    }
}

fn settings() -> DriveSettings {
    DriveSettings {
        client_id: "cid".into(),
        client_secret: "secret".into(),
        refresh_token: "refresh".into(),
        folder_name: String::new(),
    }
}

enum Reply {
    Json(Value),
    Raw(Vec<u8>),
}

fn file_json(i: &Item) -> Value {
    json!({
        "id": i.id, "name": i.name,
        "mimeType": if i.is_dir { "application/vnd.google-apps.folder" } else { "text/plain" },
        "size": i.data.len().to_string(), "modifiedTime": "2026-01-01T00:00:00Z",
        "appProperties": i.props,
    })
}

fn handle(
    s: &mut State,
    method: &str,
    url: &str,
    auth: &str,
    ctype: &str,
    body: &[u8],
) -> (u16, Reply) {
    let path = url.split('?').next().unwrap();
    s.log.push(format!("{method} {path}"));
    if path == "/token" {
        let form = String::from_utf8_lossy(body).to_string();
        let get = |k: &str| {
            form.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                .map(url_decode)
        };
        if get("grant_type").as_deref() == Some("refresh_token") {
            if get("refresh_token").as_deref() != Some("refresh")
                || get("client_secret").as_deref() != Some("secret")
            {
                return (400, Reply::Json(json!({"error": "invalid_grant"})));
            }
            s.token_calls += 1;
            return (
                200,
                Reply::Json(
                    json!({"access_token": format!("AT{}", s.token_calls), "expires_in": 3600}),
                ),
            );
        }
        // Device flow polling.
        s.device_polls += 1;
        return match (get("device_code").as_deref(), s.device_polls) {
            (Some("DENY"), _) => (400, Reply::Json(json!({"error": "access_denied"}))),
            (Some("EXPIRE"), _) => (400, Reply::Json(json!({"error": "expired_token"}))),
            (_, 1) => (428, Reply::Json(json!({"error": "authorization_pending"}))),
            (_, _) => (
                200,
                Reply::Json(
                    json!({"access_token": "AT-device", "refresh_token": "REFRESH-device", "expires_in": 3600}),
                ),
            ),
        };
    }
    if path == "/device/code" {
        let form = String::from_utf8_lossy(body).to_string();
        if !form.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file") {
            return (400, Reply::Json(json!({"error": "invalid_scope"})));
        }
        if form.contains("client_id=bad") {
            return (401, Reply::Json(json!({"error": "invalid_client"})));
        }
        let code = if form.contains("client_id=deny") {
            "DENY"
        } else if form.contains("client_id=expire") {
            "EXPIRE"
        } else {
            "DEV"
        };
        return (
            200,
            Reply::Json(json!({
                "device_code": code, "user_code": "ABCD-EFGH", "verification_url": "https://www.google.com/device",
                "expires_in": 600, "interval": 1
            })),
        );
    }

    // Everything below needs a valid bearer token.
    let expected = format!("Bearer AT{}", if s.reject_first_token { 2 } else { 1 });
    if s.reject_first_token && auth == "Bearer AT1" {
        return (
            401,
            Reply::Json(json!({"error": {"code": 401, "message": "Invalid Credentials"}})),
        );
    }
    if auth != expected && !auth.starts_with("Bearer AT") {
        return (
            401,
            Reply::Json(json!({"error": {"code": 401, "message": "Invalid Credentials"}})),
        );
    }

    if path == "/drive/v3/about" {
        return (
            200,
            Reply::Json(json!({"user": {"emailAddress": "me@example.com"}})),
        );
    }
    if path == "/drive/v3/files" && method == "GET" {
        let q = query_param(url, "q").unwrap_or_default();
        let re_parent = Regex::new(r"^'((?:[^'\\]|\\.)*)' in parents").unwrap();
        let parent = unquote(
            &re_parent
                .captures(&q)
                .expect("q must start with a parent clause")[1],
        );
        let name = Regex::new(r"name = '((?:[^'\\]|\\.)*)'")
            .unwrap()
            .captures(&q)
            .map(|c| unquote(&c[1]));
        let only_folders = q.contains("mimeType = 'application/vnd.google-apps.folder'");
        let only_files = q.contains("mimeType != 'application/vnd.google-apps.folder'");
        let files: Vec<Value> = s
            .items
            .iter()
            .filter(|i| i.parent == parent)
            .filter(|i| name.as_ref().map(|n| &i.name == n).unwrap_or(true))
            .filter(|i| !only_folders || i.is_dir)
            .filter(|i| !only_files || !i.is_dir)
            .map(file_json)
            .collect();
        return (200, Reply::Json(json!({"files": files})));
    }
    if path == "/drive/v3/files" && method == "POST" {
        let meta: Value = serde_json::from_slice(body).unwrap();
        s.next_id += 1;
        let id = format!("id{}", s.next_id);
        s.items.push(Item {
            id: id.clone(),
            name: meta["name"].as_str().unwrap().into(),
            parent: meta["parents"][0].as_str().unwrap().into(),
            is_dir: meta["mimeType"] == "application/vnd.google-apps.folder",
            data: vec![],
            props: json!({}),
        });
        return (200, Reply::Json(json!({"id": id})));
    }
    if path == "/upload/drive/v3/files" && method == "POST" {
        let (meta, data) = split_multipart(ctype, body);
        s.next_id += 1;
        let item = Item {
            id: format!("id{}", s.next_id),
            name: meta["name"].as_str().unwrap().into(),
            parent: meta["parents"][0].as_str().unwrap().into(),
            is_dir: false,
            data,
            props: meta["appProperties"].clone(),
        };
        let out = file_json(&item);
        s.items.push(item);
        return (200, Reply::Json(out));
    }
    if let Some(id) = path.strip_prefix("/upload/drive/v3/files/") {
        if method == "PATCH" {
            let (meta, data) = split_multipart(ctype, body);
            let item = s
                .items
                .iter_mut()
                .find(|i| i.id == id)
                .expect("unknown file id");
            item.data = data;
            item.props = meta["appProperties"].clone();
            return (200, Reply::Json(file_json(item)));
        }
    }
    if let Some(id) = path.strip_prefix("/drive/v3/files/") {
        if method == "GET" && url.contains("alt=media") {
            return match s.items.iter().find(|i| i.id == id) {
                Some(i) => (200, Reply::Raw(i.data.clone())),
                None => (
                    404,
                    Reply::Json(json!({"error": {"message": "File not found"}})),
                ),
            };
        }
    }
    (
        404,
        Reply::Json(json!({"error": {"message": format!("no route for {method} {path}")}})),
    )
}

#[test]
fn upload_list_update_download() {
    let fake = Fake::start();
    let drive = fake.client();
    assert_eq!(drive.account_name().as_deref(), Some("me@example.com"));

    // Nothing exists yet, and listing must not create anything.
    assert!(drive.list("").unwrap().is_empty());
    assert!(fake.state.lock().unwrap().items.is_empty());

    let f = drive
        .upload("projects/demo/notes.txt", b"hello drive")
        .unwrap();
    assert_eq!(f.path, "projects/demo/notes.txt");
    assert_eq!(f.size, 11);
    assert!(f.sha256.is_some());

    // Folders were created below the root folder "Gagent".
    let names: Vec<(String, bool)> = fake
        .state
        .lock()
        .unwrap()
        .items
        .iter()
        .map(|i| (i.name.clone(), i.is_dir))
        .collect();
    assert_eq!(
        names,
        vec![
            ("Gagent".into(), true),
            ("projects".into(), true),
            ("demo".into(), true),
            ("notes.txt".into(), false)
        ]
    );

    let root = drive.list("").unwrap();
    assert_eq!(root.len(), 1);
    assert!(root[0].is_dir && root[0].path == "projects");
    let files = drive.list("projects/demo").unwrap();
    assert_eq!(files[0].name, "notes.txt");
    assert_eq!(files[0].path, "projects/demo/notes.txt");

    // Uploading again replaces the file instead of creating a duplicate.
    drive
        .upload("projects/demo/notes.txt", b"second version")
        .unwrap();
    assert_eq!(
        fake.state
            .lock()
            .unwrap()
            .items
            .iter()
            .filter(|i| i.name == "notes.txt")
            .count(),
        1
    );
    assert_eq!(
        drive.download("projects/demo/notes.txt").unwrap(),
        b"second version"
    );

    // Names with quotes work.
    drive.upload("it's \"quoted\".txt", b"q").unwrap();
    assert_eq!(drive.download("it's \"quoted\".txt").unwrap(), b"q");

    // Binary data survives the multipart encoding.
    let bin: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    drive.upload("bin/data.bin", &bin).unwrap();
    assert_eq!(drive.download("bin/data.bin").unwrap(), bin);

    assert!(drive
        .download("projects/missing.txt")
        .unwrap_err()
        .to_string()
        .contains("does not exist"));
    assert!(drive
        .download("nofolder/x.txt")
        .unwrap_err()
        .to_string()
        .contains("does not exist"));
    assert!(drive.upload("../escape.txt", b"x").is_err());
}

#[test]
fn token_is_refreshed_when_rejected() {
    let fake = Fake::start();
    fake.state.lock().unwrap().reject_first_token = true;
    let drive = fake.client();
    drive.upload("a.txt", b"x").unwrap();
    assert_eq!(
        fake.state.lock().unwrap().token_calls,
        2,
        "one refresh after the 401"
    );
}

#[test]
fn bad_refresh_token_gives_a_helpful_error() {
    let fake = Fake::start();
    let mut s = settings();
    s.refresh_token = "revoked".into();
    let drive = DriveClient::with_endpoints(&s, fake.endpoints()).unwrap();
    let err = drive.list("").unwrap_err().to_string();
    assert!(err.contains("Connect Drive again"), "{err}");
}

#[test]
fn folder_tree_round_trip_skips_unchanged_files() {
    let fake = Fake::start();
    let drive = fake.client();
    let src = tempfile::tempdir().unwrap();
    for (p, c) in [
        ("main.py", "print(1)\n"),
        ("lib/util.py", "x = 1\n"),
        ("node_modules/skip/i.js", "x"),
        (".env", "SECRET=1"),
    ] {
        let full = src.path().join(p);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, c).unwrap();
    }
    let cancel = AtomicBool::new(false);
    let log = Mutex::new(Vec::new());
    let progress = |l: &str| log.lock().unwrap().push(l.to_string());

    let (up, same) = drive
        .upload_tree(
            src.path(),
            "proj",
            WalkOptions::default(),
            &progress,
            &cancel,
        )
        .unwrap();
    assert_eq!((up, same), (2, 0));
    let remote: Vec<String> = drive
        .list_recursive("proj")
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(remote, ["proj/lib/util.py", "proj/main.py"]);

    // Nothing changed: nothing is uploaded again.
    let (up, same) = drive
        .upload_tree(
            src.path(),
            "proj",
            WalkOptions::default(),
            &progress,
            &cancel,
        )
        .unwrap();
    assert_eq!((up, same), (0, 2));
    // One file changed.
    fs::write(src.path().join("main.py"), "print(2)\n").unwrap();
    let (up, same) = drive
        .upload_tree(
            src.path(),
            "proj",
            WalkOptions::default(),
            &progress,
            &cancel,
        )
        .unwrap();
    assert_eq!((up, same), (1, 1));
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains("main.py"))
            .count()
            == 2
    );

    // Download into another folder.
    let dst = tempfile::tempdir().unwrap();
    let n = drive
        .download_tree("proj", dst.path(), &progress, &cancel)
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        fs::read_to_string(dst.path().join("main.py")).unwrap(),
        "print(2)\n"
    );
    assert_eq!(
        fs::read_to_string(dst.path().join("lib/util.py")).unwrap(),
        "x = 1\n"
    );

    // Cancelling stops the transfer.
    let stop = AtomicBool::new(true);
    assert!(drive
        .upload_tree(
            src.path(),
            "other",
            WalkOptions::default(),
            &progress,
            &stop
        )
        .is_err());
}

#[test]
fn device_flow_success_denied_expired_and_bad_client() {
    let fake = Fake::start();
    let ep = fake.endpoints();
    let cancel = AtomicBool::new(false);

    let code = start_device_flow(&ep, "good").unwrap();
    assert_eq!(code.user_code, "ABCD-EFGH");
    assert_eq!(code.verification_url, "https://www.google.com/device");
    let tokens = wait_for_approval(&ep, "good", "secret", &code, &cancel).unwrap();
    assert_eq!(tokens.refresh_token, "REFRESH-device");
    assert_eq!(tokens.access_token, "AT-device");

    let code = start_device_flow(&ep, "deny").unwrap();
    let err = wait_for_approval(&ep, "deny", "secret", &code, &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("denied"), "{err}");

    let code = start_device_flow(&ep, "expire").unwrap();
    let err = wait_for_approval(&ep, "expire", "secret", &code, &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("expired"), "{err}");

    let err = start_device_flow(&ep, "bad").unwrap_err().to_string();
    assert!(err.contains("client ID"), "{err}");
}

#[test]
fn device_flow_can_be_cancelled() {
    let fake = Fake::start();
    let ep = fake.endpoints();
    let code = start_device_flow(&ep, "good").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let err = wait_for_approval(&ep, "good", "secret", &code, &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cancelled"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn drive_errors_are_readable() {
    let fake = Fake::start();
    let mut ep = fake.endpoints();
    ep.api = format!("{}/nowhere", fake.base);
    let drive = DriveClient::with_endpoints(&settings(), ep).unwrap();
    let err = drive.list("").unwrap_err().to_string();
    assert!(err.contains("Google Drive error (404)"), "{err}");
    let _ = HashMap::<u8, u8>::new();
}
