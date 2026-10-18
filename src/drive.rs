//! Google Drive integration.
//!
//! Gagent uses the OAuth *device flow*: the app shows a short code, you enter
//! it on google.com/device, and no local web server or embedded browser is
//! needed. Only the narrow `drive.file` scope is requested, so Gagent can see
//! just the files and folders it created itself.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::DriveSettings;
use crate::http::{url_encode, Http};
use crate::util::{sha256_hex, write_atomic};
use crate::workspace::{walk_files, WalkOptions};

pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Largest file that is uploaded in one request.
pub const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Service URLs. Overridable so tests can point at a local server.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub api: String,
    pub upload: String,
    pub device_code: String,
    pub token: String,
}

impl Default for Endpoints {
    /// Google's real endpoints. Setting `GAGENT_GOOGLE_BASE_URL` redirects all
    /// of them to one server (used for tests and for corporate proxies).
    fn default() -> Self {
        if let Ok(base) = std::env::var("GAGENT_GOOGLE_BASE_URL") {
            let base = base.trim().trim_end_matches('/');
            if !base.is_empty() {
                return Self {
                    api: format!("{base}/drive/v3"),
                    upload: format!("{base}/upload/drive/v3"),
                    device_code: format!("{base}/device/code"),
                    token: format!("{base}/token"),
                };
            }
        }
        Self {
            api: "https://www.googleapis.com/drive/v3".into(),
            upload: "https://www.googleapis.com/upload/drive/v3".into(),
            device_code: "https://oauth2.googleapis.com/device/code".into(),
            token: "https://oauth2.googleapis.com/token".into(),
        }
    }
}

/// A file or folder stored in Drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    /// Path relative to the Gagent folder, `/` separated.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: String,
    /// SHA-256 recorded by Gagent when the file was uploaded.
    pub sha256: Option<String>,
}

/// Cloud storage used by the agent tools and the UI. Implemented by [`DriveClient`].
pub trait CloudStore: Send + Sync {
    /// Lists the entries of a folder (`""` is the Gagent root folder).
    fn list(&self, remote_dir: &str) -> Result<Vec<RemoteFile>>;
    /// Creates or replaces a file.
    fn upload(&self, remote_path: &str, data: &[u8]) -> Result<RemoteFile>;
    /// Reads a file.
    fn download(&self, remote_path: &str) -> Result<Vec<u8>>;
}

// ---------------------------------------------------------------------------
// OAuth device flow
// ---------------------------------------------------------------------------

/// What the user must do to approve access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Tokens returned after approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// Step 1: ask Google for a code to show to the user.
pub fn start_device_flow(endpoints: &Endpoints, client_id: &str) -> Result<DeviceCode> {
    let http = Http::new(Duration::from_secs(30), 1 << 20);
    let response = http
        .post_form(
            &endpoints.device_code,
            &[("client_id", client_id.trim()), ("scope", DRIVE_SCOPE)],
        )
        .map_err(|e| anyhow!(e.0))?;
    let value = response.json().map_err(|e| anyhow!(e))?;
    if !response.is_success() {
        bail!("{}", oauth_error(&value, response.status));
    }
    Ok(DeviceCode {
        device_code: value["device_code"]
            .as_str()
            .context("missing device_code")?
            .to_string(),
        user_code: value["user_code"]
            .as_str()
            .context("missing user_code")?
            .to_string(),
        verification_url: value["verification_url"]
            .as_str()
            .or_else(|| value["verification_uri"].as_str())
            .unwrap_or("https://www.google.com/device")
            .to_string(),
        expires_in: value["expires_in"].as_u64().unwrap_or(1800),
        interval: value["interval"].as_u64().unwrap_or(5).max(1),
    })
}

/// Step 2: poll until the user approves, denies, or the code expires.
pub fn wait_for_approval(
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: &str,
    code: &DeviceCode,
    cancel: &AtomicBool,
) -> Result<TokenSet> {
    let http = Http::new(Duration::from_secs(30), 1 << 20);
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = Duration::from_secs(code.interval);
    loop {
        sleep_cancellable(interval, cancel)?;
        if Instant::now() >= deadline {
            bail!("the code expired before it was approved; start again");
        }
        let response = http
            .post_form(
                &endpoints.token,
                &[
                    ("client_id", client_id.trim()),
                    ("client_secret", client_secret.trim()),
                    ("device_code", &code.device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ],
            )
            .map_err(|e| anyhow!(e.0))?;
        let value = response.json().map_err(|e| anyhow!(e))?;
        if response.is_success() {
            let refresh = value["refresh_token"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if refresh.is_empty() {
                bail!("Google did not return a refresh token");
            }
            return Ok(TokenSet {
                access_token: value["access_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                refresh_token: refresh,
                expires_in: value["expires_in"].as_u64().unwrap_or(3600),
            });
        }
        match value["error"].as_str().unwrap_or_default() {
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            "access_denied" => bail!("access was denied"),
            "expired_token" => bail!("the code expired before it was approved; start again"),
            _ => bail!("{}", oauth_error(&value, response.status)),
        }
    }
}

fn sleep_cancellable(total: Duration, cancel: &AtomicBool) -> Result<()> {
    let end = Instant::now() + total;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        std::thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    Ok(())
}

fn oauth_error(value: &Value, status: u16) -> String {
    let code = value["error"].as_str().unwrap_or("error");
    let desc = value["error_description"].as_str().unwrap_or("");
    match code {
        "invalid_client" => "Google does not know this client ID. Check the Drive settings (use an OAuth client of type \"TVs and Limited Input devices\").".to_string(),
        "invalid_grant" => "The saved Google login is no longer valid. Connect Drive again.".to_string(),
        "invalid_scope" => "This OAuth client cannot use the Drive scope. Enable the Drive API in your Google Cloud project.".to_string(),
        _ if desc.is_empty() => format!("Google returned an error ({status}): {code}"),
        _ => format!("Google returned an error ({status}): {code}: {desc}"),
    }
}

// ---------------------------------------------------------------------------
// Drive client
// ---------------------------------------------------------------------------

struct AccessToken {
    value: String,
    expires_at: Instant,
}

/// A connected Google Drive account.
pub struct DriveClient {
    http: Http,
    endpoints: Endpoints,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    root_name: String,
    token: Mutex<Option<AccessToken>>,
    dir_cache: Mutex<HashMap<String, String>>,
}

impl DriveClient {
    pub fn new(settings: &DriveSettings) -> Result<Self> {
        Self::with_endpoints(settings, Endpoints::default())
    }

    pub fn with_endpoints(settings: &DriveSettings, endpoints: Endpoints) -> Result<Self> {
        if !settings.has_client() {
            bail!("Drive is not configured: enter the Google client ID and secret in Settings");
        }
        if !settings.is_connected() {
            bail!("Drive is not connected");
        }
        Ok(Self {
            http: Http::new(Duration::from_secs(300), 512 << 20),
            endpoints,
            client_id: settings.client_id.trim().to_string(),
            client_secret: settings.client_secret.trim().to_string(),
            refresh_token: settings.refresh_token.clone(),
            root_name: settings.folder().to_string(),
            token: Mutex::new(None),
            dir_cache: Mutex::new(HashMap::new()),
        })
    }

    fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut guard = self.token.lock().unwrap_or_else(|p| p.into_inner());
        if !force_refresh {
            if let Some(t) = guard.as_ref() {
                if t.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(t.value.clone());
                }
            }
        }
        let response = self
            .http
            .post_form(
                &self.endpoints.token,
                &[
                    ("client_id", &self.client_id),
                    ("client_secret", &self.client_secret),
                    ("refresh_token", &self.refresh_token),
                    ("grant_type", "refresh_token"),
                ],
            )
            .map_err(|e| anyhow!(e.0))?;
        let value = response.json().map_err(|e| anyhow!(e))?;
        if !response.is_success() {
            bail!("{}", oauth_error(&value, response.status));
        }
        let token = value["access_token"]
            .as_str()
            .context("no access token in reply")?
            .to_string();
        let ttl = value["expires_in"].as_u64().unwrap_or(3600);
        *guard = Some(AccessToken {
            value: token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(token)
    }

    /// Sends an authorized request, refreshing the token once on 401.
    fn call(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<crate::http::Response> {
        for attempt in 0..2 {
            let token = self.access_token(attempt == 1)?;
            let auth = format!("Bearer {token}");
            let response = self
                .http
                .send(method, url, &[("Authorization", &auth)], content_type, body)
                .map_err(|e| anyhow!(e.0))?;
            if response.status == 401 && attempt == 0 {
                continue;
            }
            if response.is_success() {
                return Ok(response);
            }
            let message = serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| crate::util::truncate_end(response.text().trim(), 200));
            bail!("Google Drive error ({}): {message}", response.status);
        }
        unreachable!("the loop always returns")
    }

    fn get_json(&self, url: &str) -> Result<Value> {
        self.call("GET", url, None, &[])?
            .json()
            .map_err(|e| anyhow!(e))
    }

    /// Display name of the connected account, when available.
    pub fn account_name(&self) -> Option<String> {
        let url = format!(
            "{}/about?fields=user(displayName,emailAddress)",
            self.endpoints.api
        );
        let v = self.get_json(&url).ok()?;
        v["user"]["emailAddress"]
            .as_str()
            .or_else(|| v["user"]["displayName"].as_str())
            .map(str::to_string)
    }

    fn query(&self, q: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/files?q={}&pageSize=200&fields={}",
                self.endpoints.api,
                url_encode(q),
                url_encode("nextPageToken,files(id,name,mimeType,size,modifiedTime,appProperties)")
            );
            if let Some(p) = &page {
                url.push_str(&format!("&pageToken={}", url_encode(p)));
            }
            let v = self.get_json(&url)?;
            if let Some(files) = v["files"].as_array() {
                out.extend(files.iter().cloned());
            }
            page = v["nextPageToken"]
                .as_str()
                .filter(|p| !p.is_empty())
                .map(str::to_string);
            if page.is_none() {
                return Ok(out);
            }
        }
    }

    fn find_child(&self, parent: &str, name: &str, folder: Option<bool>) -> Result<Option<Value>> {
        let mut q = format!(
            "'{}' in parents and name = '{}' and trashed = false",
            escape_q(parent),
            escape_q(name)
        );
        match folder {
            Some(true) => q.push_str(&format!(" and mimeType = '{FOLDER_MIME}'")),
            Some(false) => q.push_str(&format!(" and mimeType != '{FOLDER_MIME}'")),
            None => {}
        }
        Ok(self.query(&q)?.into_iter().next())
    }

    fn create_folder(&self, parent: &str, name: &str) -> Result<String> {
        let body = json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] });
        let url = format!("{}/files?fields=id", self.endpoints.api);
        let v = self
            .call(
                "POST",
                &url,
                Some("application/json"),
                &serde_json::to_vec(&body)?,
            )?
            .json()
            .map_err(|e| anyhow!(e))?;
        Ok(v["id"]
            .as_str()
            .context("Drive did not return a folder id")?
            .to_string())
    }

    /// Finds (and optionally creates) the folder for a path below the Gagent folder.
    fn resolve_dir(&self, path: &str, create: bool) -> Result<Option<String>> {
        let parts = split_remote_path(path)?;
        let mut current_path = String::new();
        let mut current_id = {
            let cached = self
                .dir_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get("")
                .cloned();
            match cached {
                Some(id) => id,
                None => {
                    let found = self.find_child("root", &self.root_name, Some(true))?;
                    let id = match found {
                        Some(v) => v["id"].as_str().unwrap_or_default().to_string(),
                        None if create => self.create_folder("root", &self.root_name)?,
                        None => return Ok(None),
                    };
                    self.dir_cache
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(String::new(), id.clone());
                    id
                }
            }
        };
        for part in parts {
            if !current_path.is_empty() {
                current_path.push('/');
            }
            current_path.push_str(&part);
            let cached = self
                .dir_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&current_path)
                .cloned();
            current_id = match cached {
                Some(id) => id,
                None => {
                    let id = match self.find_child(&current_id, &part, Some(true))? {
                        Some(v) => v["id"].as_str().unwrap_or_default().to_string(),
                        None if create => self.create_folder(&current_id, &part)?,
                        None => return Ok(None),
                    };
                    self.dir_cache
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(current_path.clone(), id.clone());
                    id
                }
            };
        }
        Ok(Some(current_id))
    }

    fn to_remote(&self, v: &Value, dir: &str) -> RemoteFile {
        let name = v["name"].as_str().unwrap_or_default().to_string();
        RemoteFile {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            path: if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            },
            name,
            is_dir: v["mimeType"].as_str() == Some(FOLDER_MIME),
            size: v["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            modified: v["modifiedTime"].as_str().unwrap_or_default().to_string(),
            sha256: v["appProperties"]["sha256"].as_str().map(str::to_string),
        }
    }

    /// Lists every file below a folder, recursively.
    pub fn list_recursive(&self, remote_dir: &str) -> Result<Vec<RemoteFile>> {
        let mut out = Vec::new();
        let mut stack = vec![split_remote_path(remote_dir)?.join("/")];
        while let Some(dir) = stack.pop() {
            for entry in self.list(&dir)? {
                if entry.is_dir {
                    stack.push(entry.path.clone());
                } else {
                    out.push(entry);
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Uploads every file of a local folder to `remote_dir`, skipping files
    /// that are already up to date. Returns (uploaded, unchanged).
    pub fn upload_tree(
        &self,
        local_root: &Path,
        remote_dir: &str,
        opts: WalkOptions,
        progress: &dyn Fn(&str),
        cancel: &AtomicBool,
    ) -> Result<(usize, usize)> {
        let remote_dir = split_remote_path(remote_dir)?.join("/");
        let existing: HashMap<String, Option<String>> = self
            .list_recursive(&remote_dir)?
            .into_iter()
            .map(|f| (f.path, f.sha256))
            .collect();
        let files = walk_files(local_root, opts)?;
        let (mut uploaded, mut unchanged) = (0, 0);
        for file in files {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let remote_path = if remote_dir.is_empty() {
                file.rel.clone()
            } else {
                format!("{remote_dir}/{}", file.rel)
            };
            let data = std::fs::read(local_root.join(&file.rel))
                .with_context(|| format!("cannot read {}", file.rel))?;
            if existing.get(&remote_path).and_then(|s| s.as_deref())
                == Some(sha256_hex(&data).as_str())
            {
                unchanged += 1;
                continue;
            }
            progress(&format!("Uploading {}", file.rel));
            self.upload(&remote_path, &data)?;
            uploaded += 1;
        }
        Ok((uploaded, unchanged))
    }

    /// Downloads every file below `remote_dir` into a local folder.
    pub fn download_tree(
        &self,
        remote_dir: &str,
        local_root: &Path,
        progress: &dyn Fn(&str),
        cancel: &AtomicBool,
    ) -> Result<usize> {
        let base = split_remote_path(remote_dir)?.join("/");
        let ws = crate::workspace::Workspace::new(local_root)?;
        let mut count = 0;
        for file in self.list_recursive(&base)? {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let rel = if base.is_empty() {
                file.path.clone()
            } else {
                file.path
                    .strip_prefix(&format!("{base}/"))
                    .unwrap_or(&file.path)
                    .to_string()
            };
            progress(&format!("Downloading {rel}"));
            let target = ws.resolve(&rel)?;
            let data = self.fetch(&file.id)?;
            write_atomic(&target, &data)?;
            count += 1;
        }
        Ok(count)
    }

    fn fetch(&self, id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/files/{}?alt=media", self.endpoints.api, url_encode(id));
        Ok(self.call("GET", &url, None, &[])?.body)
    }
}

impl CloudStore for DriveClient {
    fn list(&self, remote_dir: &str) -> Result<Vec<RemoteFile>> {
        let dir = split_remote_path(remote_dir)?.join("/");
        let Some(id) = self.resolve_dir(&dir, false)? else {
            return Ok(Vec::new());
        };
        let q = format!("'{}' in parents and trashed = false", escape_q(&id));
        let mut files: Vec<RemoteFile> = self
            .query(&q)?
            .iter()
            .map(|v| self.to_remote(v, &dir))
            .collect();
        files.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(files)
    }

    fn upload(&self, remote_path: &str, data: &[u8]) -> Result<RemoteFile> {
        if data.len() > MAX_UPLOAD_BYTES {
            bail!("file is too large to upload ({} bytes)", data.len());
        }
        let mut parts = split_remote_path(remote_path)?;
        let name = parts.pop().context("the remote path is empty")?;
        let dir = parts.join("/");
        let parent = self
            .resolve_dir(&dir, true)?
            .context("could not create the remote folder")?;

        let existing = self.find_child(&parent, &name, Some(false))?;
        let mut metadata = json!({ "appProperties": { "sha256": sha256_hex(data) } });
        let (method, url) = match &existing {
            Some(v) => (
                "PATCH",
                format!(
                    "{}/files/{}?uploadType=multipart&fields=id,name,mimeType,size,modifiedTime,appProperties",
                    self.endpoints.upload,
                    url_encode(v["id"].as_str().unwrap_or_default())
                ),
            ),
            None => {
                metadata["name"] = json!(name);
                metadata["parents"] = json!([parent]);
                (
                    "POST",
                    format!(
                        "{}/files?uploadType=multipart&fields=id,name,mimeType,size,modifiedTime,appProperties",
                        self.endpoints.upload
                    ),
                )
            }
        };
        let boundary = format!("gagent-{}", crate::util::unique_id());
        let mut body = Vec::with_capacity(data.len() + 512);
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&serde_json::to_vec(&metadata)?);
        body.extend_from_slice(
            format!(
                "\r\n--{boundary}\r\nContent-Type: {}\r\n\r\n",
                guess_mime(&name)
            )
            .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        let content_type = format!("multipart/related; boundary={boundary}");
        let v = self
            .call(method, &url, Some(&content_type), &body)?
            .json()
            .map_err(|e| anyhow!(e))?;
        Ok(self.to_remote(&v, &dir))
    }

    fn download(&self, remote_path: &str) -> Result<Vec<u8>> {
        let mut parts = split_remote_path(remote_path)?;
        let name = parts.pop().context("the remote path is empty")?;
        let dir = parts.join("/");
        let parent = self
            .resolve_dir(&dir, false)?
            .ok_or_else(|| anyhow!("remote folder '{dir}' does not exist"))?;
        let file = self
            .find_child(&parent, &name, Some(false))?
            .ok_or_else(|| anyhow!("remote file '{remote_path}' does not exist"))?;
        self.fetch(file["id"].as_str().unwrap_or_default())
    }
}

/// Splits and validates a remote path. Returns its non-empty components.
pub fn split_remote_path(path: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for part in path.replace('\\', "/").split('/') {
        let part = part.trim();
        match part {
            "" | "." => {}
            ".." => bail!("'..' is not allowed in remote paths"),
            p if p.contains('\0') => bail!("invalid character in remote path"),
            p => out.push(p.to_string()),
        }
    }
    Ok(out)
}

fn escape_q(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "py" | "rs" | "c" | "h" | "cpp" | "js" | "ts" | "java" | "go" | "sh"
        | "toml" | "yaml" | "yml" | "log" | "csv" => "text/plain",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_are_validated() {
        assert_eq!(split_remote_path("a/b//c/").unwrap(), ["a", "b", "c"]);
        assert_eq!(split_remote_path("").unwrap(), Vec::<String>::new());
        assert_eq!(split_remote_path("a\\b").unwrap(), ["a", "b"]);
        assert!(split_remote_path("a/../b").is_err());
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(escape_q("it's a \\ test"), "it\\'s a \\\\ test");
    }

    #[test]
    fn client_requires_configuration() {
        let mut s = DriveSettings::default();
        assert!(DriveClient::new(&s).is_err());
        s.client_id = "id".into();
        s.client_secret = "secret".into();
        assert!(DriveClient::new(&s).is_err());
        s.refresh_token = "token".into();
        assert!(DriveClient::new(&s).is_ok());
    }

    #[test]
    fn oauth_errors_are_explained() {
        let msg = oauth_error(&json!({"error":"invalid_client"}), 401);
        assert!(msg.contains("client ID"));
        let msg = oauth_error(&json!({"error":"invalid_grant"}), 400);
        assert!(msg.contains("Connect Drive again"));
    }
}
