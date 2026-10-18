//! Persistent application settings and data directory layout.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::unique_id;

/// Default Gemini model used when nothing else is configured.
pub const DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Models offered in the UI before the live model list has been fetched.
pub const PRESET_MODELS: &[&str] = &[
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
];

/// Default sandbox image tag built from the bundled Dockerfile.
pub const DEFAULT_SANDBOX_IMAGE: &str = "gagent-sandbox:latest";

/// Locations of everything Gagent stores on disk.
#[derive(Clone, Debug)]
pub struct AppDirs {
    root: PathBuf,
}

impl AppDirs {
    /// Uses an explicit root directory (used by tests and `GAGENT_HOME`).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default per-user data directory, or `$GAGENT_HOME` when set.
    pub fn default_location() -> Self {
        if let Some(home) = std::env::var_os("GAGENT_HOME") {
            if !home.is_empty() {
                return Self::at(PathBuf::from(home));
            }
        }
        // Local (not roaming) so large private project copies never sync across machines.
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::at(base.join(crate::APP_NAME))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Private working copies, one directory per session.
    pub fn shadow_dir(&self, session_id: &str) -> PathBuf {
        self.root.join("shadow").join(session_id)
    }

    /// Backups taken before changes are applied to a real project folder.
    pub fn backups_dir(&self, session_id: &str) -> PathBuf {
        self.root.join("backups").join(session_id)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.root.join("app.lock")
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [self.root.clone(), self.sessions_dir(), self.logs_dir()] {
            fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        Ok(())
    }
}

/// Docker sandbox configuration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SandboxSettings {
    /// Docker image used for the sandbox.
    pub image: String,
    /// Allow outgoing network access from the sandbox (package installs, downloads).
    pub network: bool,
    /// Memory limit in megabytes.
    pub memory_mb: u32,
    /// CPU limit (number of cores, fractions allowed).
    pub cpus: f32,
    /// Maximum number of processes inside the sandbox.
    pub pids_limit: u32,
    /// Default timeout for a single command, in seconds.
    pub command_timeout_secs: u32,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            image: DEFAULT_SANDBOX_IMAGE.to_string(),
            network: true,
            memory_mb: 2048,
            cpus: 2.0,
            pids_limit: 512,
            command_timeout_secs: 120,
        }
    }
}

/// Google Drive configuration and credentials.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DriveSettings {
    /// OAuth client ID of type "TVs and Limited Input devices".
    pub client_id: String,
    /// OAuth client secret belonging to the client ID.
    pub client_secret: String,
    /// Long-lived refresh token obtained after the user approved access.
    pub refresh_token: String,
    /// Name of the folder created in the user's Drive.
    pub folder_name: String,
}

impl DriveSettings {
    pub fn folder(&self) -> &str {
        if self.folder_name.trim().is_empty() {
            "Gagent"
        } else {
            self.folder_name.trim()
        }
    }

    pub fn is_connected(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    pub fn has_client(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

/// All user-editable settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Gemini API key. The `GEMINI_API_KEY` environment variable is used when empty.
    pub api_key: String,
    /// Gemini model identifier.
    pub model: String,
    /// Maximum number of model calls per user message.
    pub max_steps: u32,
    /// Copy agent changes into the real project automatically when a task ends.
    pub auto_apply: bool,
    /// Let the agent see files such as `.env` and private keys.
    pub share_secret_files: bool,
    pub sandbox: SandboxSettings,
    pub drive: DriveSettings,
    /// Last project folder that was opened.
    pub last_project: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            max_steps: 40,
            auto_apply: false,
            share_secret_files: false,
            sandbox: SandboxSettings::default(),
            drive: DriveSettings::default(),
            last_project: String::new(),
        }
    }
}

impl Settings {
    /// Loads settings; a missing or unreadable file yields defaults.
    /// A corrupt file is moved aside so the user does not lose it.
    pub fn load(dirs: &AppDirs) -> Settings {
        let path = dirs.settings_file();
        let Ok(text) = fs::read_to_string(&path) else {
            return Settings::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut s) => {
                s.normalize();
                s
            }
            Err(_) => {
                let _ = fs::rename(
                    &path,
                    path.with_extension(format!("corrupt-{}", unique_id())),
                );
                Settings::default()
            }
        }
    }

    /// Saves settings atomically with owner-only permissions where supported.
    pub fn save(&self, dirs: &AppDirs) -> Result<()> {
        dirs.ensure()?;
        let path = dirs.settings_file();
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension(format!("tmp-{}", unique_id()));
        fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        restrict_permissions(&tmp);
        fs::rename(&tmp, &path).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    /// Clamps values into sane ranges.
    pub fn normalize(&mut self) {
        if self.model.trim().is_empty() {
            self.model = DEFAULT_MODEL.to_string();
        }
        self.model = self.model.trim().trim_start_matches("models/").to_string();
        self.max_steps = self.max_steps.clamp(1, 200);
        self.sandbox.memory_mb = self.sandbox.memory_mb.clamp(256, 65536);
        self.sandbox.cpus = self.sandbox.cpus.clamp(0.25, 64.0);
        self.sandbox.pids_limit = self.sandbox.pids_limit.clamp(32, 32768);
        self.sandbox.command_timeout_secs = self.sandbox.command_timeout_secs.clamp(5, 3600);
        if self.sandbox.image.trim().is_empty() {
            self.sandbox.image = DEFAULT_SANDBOX_IMAGE.to_string();
        }
    }

    /// The key to use: the saved one, or the environment fallback.
    pub fn effective_api_key(&self) -> String {
        let saved = self.api_key.trim();
        if !saved.is_empty() {
            return saved.to_string();
        }
        for var in ["GEMINI_API_KEY", "GOOGLE_API_KEY"] {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    return v.trim().to_string();
                }
            }
        }
        String::new()
    }

    /// Base URL of the Gemini API (overridable for tests and proxies).
    pub fn gemini_base_url(&self) -> String {
        std::env::var("GAGENT_GEMINI_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim_end_matches('/').to_string())
            .unwrap_or_else(|| crate::gemini::DEFAULT_BASE_URL.to_string())
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        let mut s = Settings::default();
        s.api_key = "k".into();
        s.sandbox.network = false;
        s.save(&dirs).unwrap();
        let loaded = Settings::load(&dirs);
        assert_eq!(loaded, s);
    }

    #[test]
    fn partial_files_fill_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        fs::write(dirs.settings_file(), r#"{"model":"models/gemini-2.5-pro"}"#).unwrap();
        let s = Settings::load(&dirs);
        assert_eq!(s.model, "gemini-2.5-pro");
        assert_eq!(s.max_steps, 40);
        assert!(s.sandbox.network);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        fs::write(dirs.settings_file(), "{not json").unwrap();
        let s = Settings::load(&dirs);
        assert_eq!(s, Settings::default());
        assert!(!dirs.settings_file().exists());
        let moved = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(moved);
    }

    #[test]
    fn normalize_clamps() {
        let mut s = Settings::default();
        s.max_steps = 0;
        s.sandbox.memory_mb = 1;
        s.model = "  ".into();
        s.normalize();
        assert_eq!(s.max_steps, 1);
        assert_eq!(s.sandbox.memory_mb, 256);
        assert_eq!(s.model, DEFAULT_MODEL);
    }

    #[cfg(unix)]
    #[test]
    fn settings_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        Settings::default().save(&dirs).unwrap();
        let mode = fs::metadata(dirs.settings_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}
