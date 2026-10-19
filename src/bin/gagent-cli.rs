//! Command line front end for Gagent: run a task without the desktop window,
//! check the environment, or list models.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use gagent::agent::{AgentEvent, TurnEnd};
use gagent::config::{PermissionMode, Provider};
use gagent::permission::{AskRequest, Decision};
use gagent::config::{AppDirs, Settings};
use gagent::engine::Engine;
use gagent::sandbox::{self, DockerStatus};
use gagent::util::{truncate_end, unique_id};

#[derive(Parser)]
#[command(name = "gagent-cli", version, about = "Gagent command line tool")]
struct Cli {
    /// Use this folder for settings and data instead of the default location.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// AI service to use: gemini, nvidia or openai (any OpenAI-compatible server).
    #[arg(long, short = 'p', global = true)]
    provider: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one task on a project folder.
    Run {
        /// What the agent should do.
        task: String,
        /// The project folder.
        #[arg(short, long, default_value = ".")]
        folder: PathBuf,
        /// Model to use (for example nvidia/nemotron-3.5-super-vl-120b-a12b).
        #[arg(short, long)]
        model: Option<String>,
        /// Copy the agent's changes into the project when the task is done.
        #[arg(long)]
        apply: bool,
        /// Maximum number of model calls.
        #[arg(long)]
        max_steps: Option<u32>,
        /// Disable network access inside the sandbox.
        #[arg(long)]
        no_network: bool,
        /// Do not ask before installs and other risky commands (dangerous ones stay refused).
        #[arg(long, conflicts_with = "ask")]
        yes: bool,
        /// Ask before every command.
        #[arg(long)]
        ask: bool,
    },
    /// Check Docker, the API key and the sandbox image.
    Doctor {
        /// Build the sandbox image if it is missing.
        #[arg(long)]
        build_image: bool,
        /// Remove leftover Gagent containers.
        #[arg(long)]
        clean: bool,
    },
    /// List the models available to your key.
    Models,
}

/// The Windows console prints UTF-8 text as garbage (for example accented
/// letters) unless its output code page is switched to UTF-8.
#[cfg(windows)]
fn use_utf8_console() {
    extern "system" {
        fn SetConsoleOutputCP(code_page: u32) -> i32;
    }
    // SAFETY: a plain Win32 call with an integer argument.
    unsafe {
        SetConsoleOutputCP(65001);
    }
}

#[cfg(not(windows))]
fn use_utf8_console() {}

fn main() -> ExitCode {
    use_utf8_console();
    let cli = Cli::parse();
    let dirs = match &cli.home {
        Some(h) => AppDirs::at(h),
        None => AppDirs::default_location(),
    };
    let provider = match cli.provider.as_deref().map(Provider::from_name) {
        None => None,
        Some(Some(p)) => Some(p),
        Some(None) => {
            eprintln!("error: unknown provider; use gemini, nvidia or openai");
            return ExitCode::from(2);
        }
    };
    let result = match cli.command {
        Command::Run {
            task,
            folder,
            model,
            apply,
            max_steps,
            no_network,
            yes,
            ask,
        } => {
            let mode = if yes {
                Some(PermissionMode::All)
            } else if ask {
                Some(PermissionMode::Ask)
            } else {
                None
            };
            run_task(
                &dirs, &task, &folder, provider, model, apply, max_steps, no_network, mode,
            )
        }
        Command::Doctor { build_image, clean } => doctor(&dirs, provider, build_image, clean),
        Command::Models => models(&dirs, provider),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// Asks the person at the terminal. Without a terminal (a script, CI) the answer is no, so
/// that nothing is installed or touched behind anyone's back; use `--yes` to allow.
fn ask_on_terminal(request: &AskRequest) -> Decision {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "!! denied (no terminal to ask; use --yes to allow): {}",
            truncate_end(&request.command, 200)
        );
        return Decision::Deny;
    }
    eprintln!();
    eprintln!("?? The agent asks for permission: it {}.", request.reason);
    eprintln!("   {}", truncate_end(&request.command, 600));
    eprint!("   Allow? [y]es / [a]lways for this kind / [N]o: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return Decision::Deny;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Decision::Allow,
        "a" | "always" => Decision::Always,
        _ => Decision::Deny,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_task(
    dirs: &AppDirs,
    task: &str,
    folder: &Path,
    provider: Option<Provider>,
    model: Option<String>,
    apply: bool,
    max_steps: Option<u32>,
    no_network: bool,
    permission: Option<PermissionMode>,
) -> Result<ExitCode> {
    let mut settings = Settings::load(dirs);
    if let Some(p) = provider {
        settings.provider = p;
    }
    if let Some(m) = model {
        settings.set_active_model(m);
    }
    if let Some(n) = max_steps {
        settings.max_steps = n;
    }
    if no_network {
        settings.sandbox.network = false;
    }
    if let Some(mode) = permission {
        settings.permission = mode;
    }
    settings.normalize();
    if !settings.is_configured() {
        bail!("{}", cli_setup_hint(&settings));
    }
    dirs.ensure()?;
    let _instance = gagent::engine::claim_instance(dirs);

    let session_id = unique_id();
    eprintln!("Preparing a private copy of {} ...", folder.display());
    let progress: sandbox::Progress = Arc::new(|line| eprintln!("   [sandbox] {line}"));
    let engine = Engine::open(dirs, &settings, folder, &session_id, progress)?;
    engine.approvals.set_responder(ask_on_terminal);
    eprintln!(
        "{} / {}  |  sandbox: {}",
        settings.provider_label(),
        settings.active_model(),
        match &engine.backend {
            sandbox::Backend::Docker => "Docker".to_string(),
            sandbox::Backend::Wsl(distro) => format!("WSL {distro}"),
            sandbox::Backend::Host(_) => "this computer".to_string(),
        }
    );

    let emit = |event: AgentEvent| match event {
        AgentEvent::Status(_) | AgentEvent::Usage(_) | AgentEvent::FilesChanged => {}
        AgentEvent::Text(t) => println!("\n{t}\n"),
        AgentEvent::ToolStart { summary, .. } => eprintln!(">> {summary}"),
        AgentEvent::ToolEnd {
            ok,
            summary,
            output,
            ..
        } => {
            eprintln!("   {} {summary}", if ok { "ok  " } else { "FAIL" });
            if !ok && output.lines().count() > 1 {
                for line in output.lines().take(12) {
                    eprintln!("      {}", truncate_end(line, 160));
                }
            }
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let mut history = Vec::new();
    let outcome = engine.run_message(&mut history, task, &emit, &cancel)?;

    // With auto-apply on (the default) the changes are already in the project.
    if let Some(report) = &outcome.auto_applied {
        eprintln!(
            "\nApplied {} file(s) to {}:",
            report.applied.len(),
            folder.display()
        );
        for file in &report.applied {
            eprintln!("  {file}");
        }
        for (file, why) in &report.skipped {
            eprintln!("  skipped {file}: {why}");
        }
    }
    let changes = engine.changes()?;
    eprintln!(
        "\n{} file(s) still pending in the agent's copy:",
        changes.len()
    );
    for c in &changes {
        eprintln!("  {:<8} {}", c.kind.label(), c.rel);
    }
    let mut keep = !apply && !changes.is_empty();
    if apply && !changes.is_empty() {
        let report = engine.apply_all()?;
        eprintln!(
            "Applied {} file(s) to {}",
            report.applied.len(),
            folder.display()
        );
        for (file, why) in &report.skipped {
            eprintln!("  skipped {file}: {why}");
            keep = true;
        }
    }
    if keep {
        eprintln!(
            "The agent's copy is kept at {}\nCopy the files you want from there, or run the task again with --apply to write the changes into your project.",
            engine.work_dir().display()
        );
    }
    engine.shutdown();
    if !keep {
        let _ = std::fs::remove_dir_all(dirs.shadow_dir(&session_id));
    }

    Ok(match outcome.turn.end {
        TurnEnd::Finished => ExitCode::SUCCESS,
        TurnEnd::Cancelled => ExitCode::from(130),
        TurnEnd::StepLimit => {
            eprintln!("Stopped: step limit reached.");
            ExitCode::from(3)
        }
        TurnEnd::TokenLimit => {
            eprintln!("Stopped: token budget reached (change it in the desktop app's settings).");
            ExitCode::from(4)
        }
        TurnEnd::Failed(msg) => {
            eprintln!("Failed: {msg}");
            ExitCode::from(2)
        }
    })
}

/// What to do when the chosen provider has no key, phrased for the terminal.
fn cli_setup_hint(settings: &Settings) -> String {
    match settings.provider {
        Provider::Gemini => "no Gemini API key: set GEMINI_API_KEY or save one in the desktop app's settings".into(),
        Provider::Nvidia => "no NVIDIA API key: set NVIDIA_API_KEY or save one in the desktop app's settings (create one at https://build.nvidia.com)".into(),
        Provider::Openai => "the OpenAI-compatible server is not set up: save its address and model in the desktop app's settings, or set GAGENT_OPENAI_BASE_URL and use --model".into(),
    }
}

fn doctor(
    dirs: &AppDirs,
    provider: Option<Provider>,
    build_image: bool,
    clean: bool,
) -> Result<ExitCode> {
    let mut settings = Settings::load(dirs);
    if let Some(p) = provider {
        settings.provider = p;
    }
    let mut problems = 0;
    println!("Gagent {}", gagent::VERSION);
    println!("Data folder: {}", dirs.root().display());

    let status = sandbox::sandbox_status(&settings.sandbox);
    println!("Sandbox: {}", status.describe());
    match status {
        DockerStatus::Ready { image_present, .. } => {
            if !image_present {
                if build_image {
                    println!(
                        "Building {} (this takes a few minutes)...",
                        settings.sandbox.image
                    );
                    let progress: sandbox::Progress = Arc::new(|line| println!("   {line}"));
                    sandbox::build_image(
                        &sandbox::docker_binary(),
                        &settings.sandbox.image,
                        &progress,
                    )?;
                    println!("Sandbox image built.");
                } else {
                    println!(
                        "Sandbox image {} is missing. Run `gagent-cli doctor --build-image`.",
                        settings.sandbox.image
                    );
                    problems += 1;
                }
            } else {
                println!("Sandbox image {}: present", settings.sandbox.image);
            }
        }
        DockerStatus::Wsl { .. } | DockerStatus::Host { .. } => {}
        _ => problems += 1,
    }
    if clean {
        let n = sandbox::remove_all_gagent_containers().context("cannot clean containers")?;
        println!("Removed {n} leftover container(s).");
    }

    let name = settings.provider.label();
    if !settings.is_configured() {
        println!("{name}: {}", cli_setup_hint(&settings));
        problems += 1;
    } else {
        match gagent::llm::make_client(&settings).list_models() {
            Ok(models) => println!(
                "{name}: works ({} models available; using {})",
                models.len(),
                settings.active_model()
            ),
            Err(e) => {
                println!("{name}: {}", e.user_message());
                problems += 1;
            }
        }
    }
    Ok(if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn models(dirs: &AppDirs, provider: Option<Provider>) -> Result<ExitCode> {
    let mut settings = Settings::load(dirs);
    if let Some(p) = provider {
        settings.provider = p;
    }
    if !settings.is_configured() {
        bail!("{}", cli_setup_hint(&settings));
    }
    let list = gagent::llm::make_client(&settings)
        .list_models()
        .map_err(|e| anyhow::anyhow!(e.user_message()))?;
    for m in list {
        println!("{:<48} {}", m.id, m.display_name);
    }
    Ok(ExitCode::SUCCESS)
}
