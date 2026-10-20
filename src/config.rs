//! Persistent application settings and data directory layout.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::unique_id;

/// Default Gemini model used when nothing else is configured (an alias that always points at a
/// model that exists).
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";

/// Models offered in the UI before the live model list has been fetched.
pub const PRESET_MODELS: &[&str] = &[
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-flash-latest",
    "gemini-3.1-flash-lite",
];

/// Default NVIDIA model (an id from build.nvidia.com).
pub const DEFAULT_NVIDIA_MODEL: &str = "nvidia/nemotron-3.5-super-vl-120b-a12b";

/// NVIDIA models offered before the live model list has been fetched.
pub const NVIDIA_PRESET_MODELS: &[&str] = &[
    "nvidia/nemotron-3.5-super-vl-120b-a12b",
    "nvidia/nemotron-3.5-lightning-30b-a3b",
    "openai/gpt-oss-20b",
    "poolside/laguna-xs-2.1",
    "stepfun-ai/step-5-preview",
    "meta/muse-glimmer-30b",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "nvidia/nemotron-3-super-120b-a12b",
    "meta/llama-3.2-90b-vision-instruct",
];

/// Requests per minute that Gagent keeps to by itself on NVIDIA's free endpoint, a little
/// below the 40 it allows.
pub const NVIDIA_AUTO_REQUESTS_PER_MINUTE: u32 = 35;

/// Default Groq model.
pub const DEFAULT_GROQ_MODEL: &str = "openai/gpt-oss-120b";

/// Groq models offered before the live list has been fetched.
pub const GROQ_PRESET_MODELS: &[&str] = &[
    "openai/gpt-oss-120b",
    "openai/gpt-oss-20b",
    "qwen/qwen3.8-27b",
];

/// Requests per minute that Gagent keeps to by itself on Groq's free plan (it allows about 30).
pub const GROQ_AUTO_REQUESTS_PER_MINUTE: u32 = 25;

/// Default sandbox image tag built from the bundled Dockerfile.
pub const DEFAULT_SANDBOX_IMAGE: &str = "gagent-sandbox:latest";

/// When a message becomes a team project: the lead plans, several AIs do the tasks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamMode {
    /// Big jobs (an app, a site, a long list of wishes) go to a team, small ones to one agent.
    #[default]
    Auto,
    /// Every message is planned and given to a team.
    Always,
    /// Every message goes to one agent.
    Never,
}

impl TeamMode {
    pub const ALL: [TeamMode; 3] = [TeamMode::Auto, TeamMode::Always, TeamMode::Never];

    pub fn label(self) -> &'static str {
        match self {
            TeamMode::Auto => "Auto: big jobs go to a team",
            TeamMode::Always => "Always a team",
            TeamMode::Never => "Never: one agent",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            TeamMode::Auto => "Team: auto",
            TeamMode::Always => "Team: on",
            TeamMode::Never => "Team: off",
        }
    }
}

/// Which AI service the agent talks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Google Gemini (aistudio.google.com).
    #[default]
    Gemini,
    /// NVIDIA's hosted models (build.nvidia.com).
    Nvidia,
    /// Groq's very fast hosted models (console.groq.com).
    Groq,
    /// Any OpenAI-compatible server: OpenAI, OpenRouter, Ollama, LM Studio, vLLM, ...
    Openai,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Gemini,
        Provider::Nvidia,
        Provider::Groq,
        Provider::Openai,
    ];

    /// Name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Gemini => "Google Gemini",
            Provider::Nvidia => "NVIDIA",
            Provider::Groq => "Groq",
            Provider::Openai => "OpenAI-compatible",
        }
    }

    /// Short name used on the command line and in messages.
    pub fn id(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::Nvidia => "nvidia",
            Provider::Groq => "groq",
            Provider::Openai => "openai",
        }
    }

    pub fn from_name(name: &str) -> Option<Provider> {
        match name.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" => Some(Provider::Gemini),
            "nvidia" | "nim" => Some(Provider::Nvidia),
            "groq" => Some(Provider::Groq),
            "openai" | "custom" | "ollama" | "openrouter" => Some(Provider::Openai),
            _ => None,
        }
    }

    /// Environment variables that may hold the API key, in order.
    fn key_env_vars(self) -> &'static [&'static str] {
        match self {
            Provider::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Provider::Nvidia => &["NVIDIA_API_KEY", "NGC_API_KEY"],
            Provider::Groq => &["GROQ_API_KEY"],
            Provider::Openai => &["OPENAI_API_KEY"],
        }
    }

    /// Where to get a key, for messages.
    pub fn key_help(self) -> &'static str {
        match self {
            Provider::Gemini => "get a free key at https://aistudio.google.com/apikey",
            Provider::Nvidia => "create a key at https://build.nvidia.com (it starts with nvapi-)",
            Provider::Groq => "create a key at https://console.groq.com/keys (it starts with gsk_)",
            Provider::Openai => "enter the server address (and key, if it needs one)",
        }
    }
}

/// How tools (read file, run command, ...) are offered to a model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    /// Use the API's tool calling; switch to the text format if the model rejects it.
    #[default]
    Auto,
    /// Always use the API's tool calling.
    Native,
    /// Always describe the tools in the prompt and read `<tool_call>` blocks from the
    /// answer. Works with any text-to-text model.
    Text,
}

impl ToolMode {
    pub const ALL: [ToolMode; 3] = [ToolMode::Auto, ToolMode::Native, ToolMode::Text];

    pub fn label(self) -> &'static str {
        match self {
            ToolMode::Auto => "Automatic",
            ToolMode::Native => "API tool calling",
            ToolMode::Text => "Text format (any model)",
        }
    }
}

/// Credentials and model of an OpenAI-compatible provider.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProviderConfig {
    pub api_key: String,
    pub model: String,
    /// Server address ending in `/v1`, e.g. `https://integrate.api.nvidia.com/v1`.
    pub base_url: String,
}

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

/// Light or dark look of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the Windows / desktop setting.
    #[default]
    Auto,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::Auto, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::Auto => "Same as the system",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// Which quick checks run on a file right after the agent has written it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Off,
    /// Python and JavaScript syntax, JSON validity. Cheap.
    #[default]
    Light,
    /// Also `cargo check` after edits of Rust files. Costs CPU time.
    Rust,
}

impl CheckLevel {
    pub const ALL: [CheckLevel; 3] = [CheckLevel::Off, CheckLevel::Light, CheckLevel::Rust];

    pub fn label(self) -> &'static str {
        match self {
            CheckLevel::Off => "Off",
            CheckLevel::Light => "Python, JavaScript, JSON",
            CheckLevel::Rust => "Also Rust (cargo check after edits)",
        }
    }
}

/// When the agent has to ask before it runs a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Before every command.
    Ask,
    /// Only for installing packages, stopping programs, downloaded scripts and anything
    /// outside the workspace. The dangerous commands are always refused.
    #[default]
    Auto,
    /// Never; only the dangerous commands are refused.
    All,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 3] = [PermissionMode::Ask, PermissionMode::Auto, PermissionMode::All];

    /// Short name for the button next to the message box.
    pub fn short_label(self) -> &'static str {
        match self {
            PermissionMode::Ask => "Ask",
            PermissionMode::Auto => "Auto",
            PermissionMode::All => "Allow all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Ask => "Ask before every command",
            PermissionMode::Auto => "Auto: ask for installs and risky things",
            PermissionMode::All => "Allow all: never ask (dangerous ones stay refused)",
        }
    }

    pub fn from_name(name: &str) -> Option<PermissionMode> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ask" | "manual" => Some(PermissionMode::Ask),
            "auto" => Some(PermissionMode::Auto),
            "all" | "allow" | "yes" => Some(PermissionMode::All),
            _ => None,
        }
    }
}

/// Where the agent's shell commands run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    /// This computer's own bash if there is one (fastest), otherwise Docker, otherwise WSL.
    #[default]
    Auto,
    /// Bash on this computer (Git Bash on Windows): no virtual machine, nothing to
    /// install, but not isolated. Dangerous commands are refused.
    Host,
    /// A Docker container: isolated, but Docker Desktop needs a lot of RAM and disk.
    Docker,
    /// An Ubuntu distribution of the Windows Subsystem for Linux: light on RAM and
    /// disk, but not isolated from your Windows files.
    Wsl,
}

impl SandboxBackend {
    pub const ALL: [SandboxBackend; 4] = [
        SandboxBackend::Auto,
        SandboxBackend::Host,
        SandboxBackend::Docker,
        SandboxBackend::Wsl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SandboxBackend::Auto => "Automatic",
            SandboxBackend::Host => "This computer (fast, not isolated)",
            SandboxBackend::Docker => "Docker (isolated)",
            SandboxBackend::Wsl => "WSL Ubuntu (light, Windows only)",
        }
    }

    pub fn from_name(name: &str) -> Option<SandboxBackend> {
        match name.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(SandboxBackend::Auto),
            "host" | "local" | "native" => Some(SandboxBackend::Host),
            "docker" => Some(SandboxBackend::Docker),
            "wsl" => Some(SandboxBackend::Wsl),
            _ => None,
        }
    }
}

/// Sandbox configuration (Docker container or WSL Ubuntu).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SandboxSettings {
    /// Where commands run.
    pub backend: SandboxBackend,
    /// Name of the WSL distribution to use; empty means the default one.
    pub wsl_distro: String,
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
            backend: SandboxBackend::Auto,
            wsl_distro: String::new(),
            image: DEFAULT_SANDBOX_IMAGE.to_string(),
            network: true,
            memory_mb: 2048,
            cpus: 2.0,
            pids_limit: 512,
            command_timeout_secs: 120,
        }
    }
}

/// All user-editable settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// The AI service in use.
    pub provider: Provider,
    /// Gemini API key. The `GEMINI_API_KEY` environment variable is used when empty.
    pub api_key: String,
    /// Gemini model identifier.
    pub model: String,
    /// NVIDIA hosted models (`NVIDIA_API_KEY` is used when the key is empty).
    pub nvidia: ProviderConfig,
    /// Groq's hosted models (`GROQ_API_KEY` is used when the key is empty).
    pub groq: ProviderConfig,
    /// Any other OpenAI-compatible server.
    pub openai: ProviderConfig,
    /// Longest answer (including reasoning) requested from OpenAI-compatible providers.
    pub max_output_tokens: u32,
    /// How tools are offered to OpenAI-compatible models.
    pub tool_mode: ToolMode,
    /// Maximum number of model calls per user message.
    pub max_steps: u32,
    /// Stop a task after this many tokens (all of its requests added up). 0 = no limit.
    pub max_task_tokens: u64,
    /// Choose the best available model for every message by itself (Gemini, NVIDIA, Groq...),
    /// looking at what each can do and at the limits that are left.
    pub auto_pick: bool,
    /// When a model runs out of quota or fails, go on with another one instead of stopping.
    pub auto_failover: bool,
    /// Requests per minute to the AI service. 0 = automatic (35 on NVIDIA's free endpoint,
    /// no limit on the others).
    pub requests_per_minute: u32,
    /// Quick checks of files the agent writes.
    pub auto_check: CheckLevel,
    /// When the agent asks before running a command.
    pub permission: PermissionMode,
    /// Copy agent changes into the real project automatically when a task ends.
    pub auto_apply: bool,
    /// Let the agent see files such as `.env` and private keys.
    pub share_secret_files: bool,
    pub sandbox: SandboxSettings,
    /// Light, dark, or follow the system.
    pub appearance: Appearance,
    /// Last project folder that was opened.
    pub last_project: String,
    /// Folders the user said "I trust this folder" for. The agent does not work in any other.
    pub trusted_folders: Vec<String>,
    /// The small "a star on GitHub helps" line under the message box was hidden by the user.
    pub star_hint_hidden: bool,
    /// When a message is planned and given to a team of AIs.
    pub team_mode: TeamMode,
    /// Start the plan of a team without waiting for the user's yes.
    pub team_auto_start: bool,
    /// Read the answers aloud (a Gemini voice).
    pub speak_answers: bool,
    /// The language the user speaks to the microphone (`az`, `tr`, `en`...); empty finds it out.
    pub voice_language: String,
    /// Save a git checkpoint of a git project before the agent's changes reach it.
    pub git_checkpoint: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::Gemini,
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            nvidia: ProviderConfig {
                api_key: String::new(),
                model: DEFAULT_NVIDIA_MODEL.to_string(),
                base_url: crate::openai::NVIDIA_BASE_URL.to_string(),
            },
            groq: ProviderConfig {
                api_key: String::new(),
                model: DEFAULT_GROQ_MODEL.to_string(),
                base_url: crate::openai::GROQ_BASE_URL.to_string(),
            },
            openai: ProviderConfig::default(),
            max_output_tokens: 16384,
            tool_mode: ToolMode::Auto,
            max_steps: 40,
            max_task_tokens: 1_500_000,
            requests_per_minute: 0,
            auto_pick: true,
            auto_failover: true,
            auto_check: CheckLevel::Light,
            permission: PermissionMode::Auto,
            auto_apply: true,
            share_secret_files: false,
            sandbox: SandboxSettings::default(),
            appearance: Appearance::Auto,
            last_project: String::new(),
            trusted_folders: Vec::new(),
            star_hint_hidden: false,
            team_mode: TeamMode::Auto,
            team_auto_start: false,
            speak_answers: false,
            voice_language: String::new(),
            git_checkpoint: true,
        }
    }
}

impl Settings {
    /// A folder path in the form used to compare folders (`C:\Work\App\` and `c:/work/app` are one folder on Windows).
    fn folder_key(folder: &str) -> String {
        let path = folder.trim().replace('\\', "/");
        let path = path.trim_end_matches('/');
        if cfg!(windows) {
            path.to_ascii_lowercase()
        } else {
            path.to_string()
        }
    }

    /// True when the user said the agent may work in this folder.
    pub fn is_trusted(&self, folder: &str) -> bool {
        let key = Self::folder_key(folder);
        !key.is_empty()
            && self
                .trusted_folders
                .iter()
                .any(|t| Self::folder_key(t) == key)
    }

    /// Remembers that the user trusts this folder.
    pub fn trust_folder(&mut self, folder: &str) {
        if !folder.trim().is_empty() && !self.is_trusted(folder) {
            self.trusted_folders.push(folder.trim().to_string());
        }
    }

    /// Loads settings; a missing or unreadable file yields defaults.
    /// A corrupt file is moved aside so the user does not lose it.
    pub fn load(dirs: &AppDirs) -> Settings {
        let path = dirs.settings_file();
        let Ok(text) = fs::read_to_string(&path) else {
            return Settings::default();
        };
        // A file saved by Notepad starts with a byte order mark; that is not a damaged file.
        match serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')) {
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
        self.nvidia.model = self.nvidia.model.trim().to_string();
        if self.nvidia.model.is_empty() {
            self.nvidia.model = DEFAULT_NVIDIA_MODEL.to_string();
        }
        self.nvidia.base_url = self
            .nvidia
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        if self.nvidia.base_url.is_empty() {
            self.nvidia.base_url = crate::openai::NVIDIA_BASE_URL.to_string();
        }
        self.groq.model = self.groq.model.trim().to_string();
        if self.groq.model.is_empty() {
            self.groq.model = DEFAULT_GROQ_MODEL.to_string();
        }
        self.groq.base_url = self.groq.base_url.trim().trim_end_matches('/').to_string();
        if self.groq.base_url.is_empty() {
            self.groq.base_url = crate::openai::GROQ_BASE_URL.to_string();
        }
        self.openai.model = self.openai.model.trim().to_string();
        self.openai.base_url = self
            .openai
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        self.sandbox.wsl_distro = self.sandbox.wsl_distro.trim().to_string();
        self.max_output_tokens = self.max_output_tokens.clamp(256, 131_072);
        self.max_steps = self.max_steps.clamp(1, 200);
        self.requests_per_minute = self.requests_per_minute.min(600);
        if self.max_task_tokens != 0 {
            self.max_task_tokens = self.max_task_tokens.clamp(50_000, 100_000_000);
        }
        self.sandbox.memory_mb = self.sandbox.memory_mb.clamp(256, 65536);
        self.sandbox.cpus = self.sandbox.cpus.clamp(0.25, 64.0);
        self.sandbox.pids_limit = self.sandbox.pids_limit.clamp(32, 32768);
        self.sandbox.command_timeout_secs = self.sandbox.command_timeout_secs.clamp(5, 3600);
        if self.sandbox.image.trim().is_empty() {
            self.sandbox.image = DEFAULT_SANDBOX_IMAGE.to_string();
        }
    }

    /// Name of the provider in use, for messages.
    pub fn provider_label(&self) -> &'static str {
        match self.provider {
            Provider::Gemini => "Gemini",
            Provider::Nvidia => "NVIDIA",
            Provider::Groq => "Groq",
            Provider::Openai => "AI server",
        }
    }

    /// Requests per minute to keep to for the provider in use; 0 means no limit.
    pub fn rate_limit(&self) -> u32 {
        if self.requests_per_minute > 0 {
            self.requests_per_minute
        } else if self.provider == Provider::Nvidia {
            NVIDIA_AUTO_REQUESTS_PER_MINUTE
        } else if self.provider == Provider::Groq {
            GROQ_AUTO_REQUESTS_PER_MINUTE
        } else {
            0
        }
    }

    /// True when `provider` has what it needs (a key, or an address and a model) to be used.
    pub fn provider_ready(&self, provider: Provider) -> bool {
        let mut s = self.clone();
        s.provider = provider;
        s.is_configured()
    }

    /// The model of the provider in use.
    pub fn active_model(&self) -> &str {
        match self.provider {
            Provider::Gemini => &self.model,
            Provider::Nvidia => &self.nvidia.model,
            Provider::Groq => &self.groq.model,
            Provider::Openai => &self.openai.model,
        }
    }

    pub fn set_active_model(&mut self, model: String) {
        match self.provider {
            Provider::Gemini => self.model = model,
            Provider::Nvidia => self.nvidia.model = model,
            Provider::Groq => self.groq.model = model,
            Provider::Openai => self.openai.model = model,
        }
    }

    /// Models to offer before the live list has been fetched.
    pub fn preset_models(&self) -> Vec<String> {
        match self.provider {
            Provider::Gemini => PRESET_MODELS.iter().map(|s| s.to_string()).collect(),
            Provider::Nvidia => NVIDIA_PRESET_MODELS.iter().map(|s| s.to_string()).collect(),
            Provider::Groq => GROQ_PRESET_MODELS.iter().map(|s| s.to_string()).collect(),
            Provider::Openai => Vec::new(),
        }
    }

    /// The key to use: the saved one, or the environment fallback.
    pub fn effective_api_key(&self) -> String {
        let saved = match self.provider {
            Provider::Gemini => &self.api_key,
            Provider::Nvidia => &self.nvidia.api_key,
            Provider::Groq => &self.groq.api_key,
            Provider::Openai => &self.openai.api_key,
        }
        .trim();
        if !saved.is_empty() {
            return saved.to_string();
        }
        for var in self.provider.key_env_vars() {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    return v.trim().to_string();
                }
            }
        }
        String::new()
    }

    /// True when the provider has everything it needs to answer. Local
    /// OpenAI-compatible servers (Ollama, LM Studio) work without a key.
    pub fn is_configured(&self) -> bool {
        match self.provider {
            Provider::Gemini | Provider::Nvidia | Provider::Groq => {
                !self.effective_api_key().is_empty()
            }
            Provider::Openai => !self.api_base_url().is_empty() && !self.openai.model.is_empty(),
        }
    }

    /// What is missing, as a sentence for the user.
    pub fn setup_hint(&self) -> String {
        match self.provider {
            Provider::Gemini | Provider::Nvidia | Provider::Groq => format!(
                "No {} API key is set. Open Settings and paste your key ({}).",
                self.provider_label(),
                self.provider.key_help()
            ),
            Provider::Openai => {
                "The AI server is not set up. Open Settings and enter the server address and the model name."
                    .to_string()
            }
        }
    }

    /// Base URL of the provider's API. The `GAGENT_*_BASE_URL` environment
    /// variables override it (used by tests and corporate proxies).
    pub fn api_base_url(&self) -> String {
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        };
        match self.provider {
            Provider::Gemini => env("GAGENT_GEMINI_BASE_URL")
                .unwrap_or_else(|| crate::gemini::DEFAULT_BASE_URL.to_string()),
            Provider::Nvidia => env("GAGENT_NVIDIA_BASE_URL").unwrap_or_else(|| {
                if self.nvidia.base_url.is_empty() {
                    crate::openai::NVIDIA_BASE_URL.to_string()
                } else {
                    self.nvidia.base_url.clone()
                }
            }),
            Provider::Groq => env("GAGENT_GROQ_BASE_URL").unwrap_or_else(|| {
                if self.groq.base_url.is_empty() {
                    crate::openai::GROQ_BASE_URL.to_string()
                } else {
                    self.groq.base_url.clone()
                }
            }),
            Provider::Openai => {
                env("GAGENT_OPENAI_BASE_URL").unwrap_or_else(|| self.openai.base_url.clone())
            }
        }
    }

    /// Base URL of the Gemini API (kept for callers that always mean Gemini).
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
    fn old_files_without_a_provider_use_gemini() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        fs::write(
            dirs.settings_file(),
            r#"{"api_key":"AIza-old","model":"gemini-2.5-pro"}"#,
        )
        .unwrap();
        let s = Settings::load(&dirs);
        assert_eq!(s.provider, Provider::Gemini);
        assert_eq!(s.effective_api_key(), "AIza-old");
        assert_eq!(s.active_model(), "gemini-2.5-pro");
        // NVIDIA defaults are filled in even though the file never mentioned them.
        assert_eq!(s.nvidia.model, DEFAULT_NVIDIA_MODEL);
        assert_eq!(s.nvidia.base_url, crate::openai::NVIDIA_BASE_URL);
    }

    #[test]
    fn each_provider_keeps_its_own_key_and_model() {
        let mut s = Settings::default();
        s.api_key = "gem-key".into();
        s.nvidia.api_key = "nvapi-key".into();
        s.openai.base_url = "http://localhost:11434/v1/".into();
        s.openai.model = "qwen2.5-coder".into();
        s.normalize();

        s.provider = Provider::Nvidia;
        assert_eq!(s.effective_api_key(), "nvapi-key");
        assert_eq!(s.active_model(), DEFAULT_NVIDIA_MODEL);
        assert!(s.is_configured());
        s.set_active_model("meta/llama-3.3-70b-instruct".into());
        assert_eq!(s.nvidia.model, "meta/llama-3.3-70b-instruct");
        assert_eq!(s.model, DEFAULT_MODEL, "the Gemini model is untouched");

        s.provider = Provider::Openai;
        // A local server needs no key, only an address and a model.
        assert!(s.is_configured());
        assert_eq!(s.api_base_url(), "http://localhost:11434/v1");

        s.provider = Provider::Gemini;
        assert_eq!(s.effective_api_key(), "gem-key");
        s.api_key.clear();
        s.nvidia.api_key.clear();
        s.provider = Provider::Nvidia;
        if std::env::var("NVIDIA_API_KEY").is_err() && std::env::var("NGC_API_KEY").is_err() {
            assert!(!s.is_configured());
            assert!(s.setup_hint().contains("build.nvidia.com"));
        }
    }

    #[test]
    fn token_budget_and_checks_have_safe_defaults() {
        let mut s = Settings::default();
        assert_eq!(s.max_task_tokens, 1_500_000);
        assert_eq!(s.auto_check, CheckLevel::Light);
        s.max_task_tokens = 10;
        s.normalize();
        assert_eq!(s.max_task_tokens, 50_000, "tiny budgets are raised");
        s.max_task_tokens = 0;
        s.normalize();
        assert_eq!(s.max_task_tokens, 0, "0 means no limit");
        // Old settings files without the new fields still load.
        let loaded: Settings = serde_json::from_str(r#"{"max_steps": 10}"#).unwrap();
        assert_eq!(loaded.max_task_tokens, 1_500_000);
    }

    #[test]
    fn provider_names_parse() {
        assert_eq!(Provider::from_name("NVIDIA"), Some(Provider::Nvidia));
        assert_eq!(Provider::from_name(" ollama "), Some(Provider::Openai));
        assert_eq!(Provider::from_name("google"), Some(Provider::Gemini));
        assert_eq!(Provider::from_name("nope"), None);
        let json = serde_json::to_string(&Provider::Openai).unwrap();
        assert_eq!(json, "\"openai\"");
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
    fn the_request_limit_is_automatic_on_nvidia_only() {
        let mut s = Settings::default();
        s.provider = Provider::Nvidia;
        assert_eq!(s.rate_limit(), NVIDIA_AUTO_REQUESTS_PER_MINUTE);
        s.requests_per_minute = 12;
        assert_eq!(s.rate_limit(), 12);
        s.provider = Provider::Gemini;
        s.requests_per_minute = 0;
        assert_eq!(s.rate_limit(), 0);
        s.requests_per_minute = 100_000;
        s.normalize();
        assert_eq!(s.requests_per_minute, 600);
    }

    #[test]
    fn a_provider_is_ready_when_it_has_a_key() {
        let mut s = Settings::default();
        s.api_key = "AIza-test".into();
        s.nvidia.api_key = String::new();
        s.openai.base_url = String::new();
        assert!(s.provider_ready(Provider::Gemini));
        assert!(!s.provider_ready(Provider::Openai));
        s.openai.base_url = "http://localhost:11434/v1".into();
        s.openai.model = "llama3".into();
        assert!(s.provider_ready(Provider::Openai));
    }

    #[test]
    fn groq_is_a_provider_with_its_own_key_and_address() {
        assert_eq!(Provider::from_name("groq"), Some(Provider::Groq));
        assert_eq!(Provider::Groq.id(), "groq");
        assert!(Provider::ALL.contains(&Provider::Groq));
        let mut s = Settings::default();
        s.provider = Provider::Groq;
        s.groq.api_key = String::new();
        if std::env::var("GROQ_API_KEY").is_err() {
            assert!(!s.is_configured());
        }
        s.groq.api_key = "gsk_test".into();
        assert!(s.is_configured());
        assert_eq!(s.api_base_url(), crate::openai::GROQ_BASE_URL);
        assert_eq!(s.active_model(), DEFAULT_GROQ_MODEL);
        assert_eq!(s.rate_limit(), GROQ_AUTO_REQUESTS_PER_MINUTE);
        assert!(s.preset_models().iter().any(|m| m == DEFAULT_GROQ_MODEL));
        assert!(s.setup_hint().contains("console.groq.com/keys"), "{}", s.setup_hint());
        // Settings saved by an older version, without Groq, still load with Groq's defaults.
        let old: Settings = serde_json::from_str(r#"{"provider":"nvidia"}"#).unwrap();
        assert_eq!(old.groq.model, DEFAULT_GROQ_MODEL);
        assert_eq!(old.groq.base_url, crate::openai::GROQ_BASE_URL);
        // Automatic choice of the model is the default.
        assert!(old.auto_failover && old.auto_pick);
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

#[cfg(test)]
mod trusted_folder_tests {
    use super::*;

    #[test]
    fn a_trusted_folder_is_remembered_once() {
        let mut s = Settings::default();
        assert!(!s.is_trusted("/work/app"));
        assert!(!s.is_trusted(""));
        s.trust_folder("/work/app");
        s.trust_folder("/work/app/");
        assert!(s.is_trusted("/work/app"));
        assert_eq!(s.trusted_folders.len(), 1);
        // A folder next to it, or inside it, is a different folder.
        assert!(!s.is_trusted("/work/app2"));
        assert!(!s.is_trusted("/work/app/sub"));
        assert!(!s.is_trusted("/work"));
        s.trust_folder("   ");
        assert_eq!(s.trusted_folders.len(), 1);
    }

    #[test]
    fn slashes_and_case_do_not_matter_on_windows_paths() {
        let mut s = Settings::default();
        s.trust_folder(r"C:\Work\App");
        assert!(s.is_trusted(r"C:\Work\App\"));
        assert!(s.is_trusted("C:/Work/App"));
        if cfg!(windows) {
            assert!(s.is_trusted(r"c:\work\app"));
        }
    }

    #[test]
    fn a_settings_file_with_a_byte_order_mark_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::at(dir.path());
        dirs.ensure().unwrap();
        std::fs::write(
            dirs.settings_file(),
            "\u{feff}{\"provider\":\"groq\",\"last_project\":\"x\"}",
        )
        .unwrap();
        let s = Settings::load(&dirs);
        assert_eq!(s.provider, Provider::Groq);
        assert_eq!(s.last_project, "x");
        // Nothing was moved aside as damaged.
        assert!(dirs.settings_file().exists());
    }

    #[test]
    fn old_settings_files_have_no_trusted_folders() {
        let s: Settings = serde_json::from_str(r#"{"last_project":"x"}"#).unwrap();
        assert!(s.trusted_folders.is_empty());
        assert!(!s.is_trusted("x"));
    }
}
