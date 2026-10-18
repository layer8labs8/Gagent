//! Command line front end for Gagent: run a task without the desktop window,
//! check the environment, or list models.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use gagent::agent::{AgentEvent, TurnEnd};
use gagent::config::{AppDirs, Settings};
use gagent::engine::Engine;
use gagent::gemini::GeminiClient;
use gagent::sandbox::{self, docker_status, DockerStatus};
use gagent::util::{truncate_end, unique_id};

#[derive(Parser)]
#[command(name = "gagent-cli", version, about = "Gagent command line tool")]
struct Cli {
    /// Use this folder for settings and data instead of the default location.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
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
        /// Gemini model to use.
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
    /// List the Gemini models available to your key.
    Models,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dirs = match &cli.home {
        Some(h) => AppDirs::at(h),
        None => AppDirs::default_location(),
    };
    let result = match cli.command {
        Command::Run {
            task,
            folder,
            model,
            apply,
            max_steps,
            no_network,
        } => run_task(&dirs, &task, &folder, model, apply, max_steps, no_network),
        Command::Doctor { build_image, clean } => doctor(&dirs, build_image, clean),
        Command::Models => models(&dirs),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run_task(
    dirs: &AppDirs,
    task: &str,
    folder: &PathBuf,
    model: Option<String>,
    apply: bool,
    max_steps: Option<u32>,
    no_network: bool,
) -> Result<ExitCode> {
    let mut settings = Settings::load(dirs);
    if let Some(m) = model {
        settings.model = m;
    }
    if let Some(n) = max_steps {
        settings.max_steps = n;
    }
    if no_network {
        settings.sandbox.network = false;
    }
    settings.normalize();
    if settings.effective_api_key().is_empty() {
        bail!("no Gemini API key: set GEMINI_API_KEY or save one in the desktop app's settings");
    }
    dirs.ensure()?;
    let _instance = gagent::engine::claim_instance(dirs);

    let session_id = unique_id();
    eprintln!("Preparing a private copy of {} ...", folder.display());
    let progress: sandbox::Progress = Arc::new(|line| eprintln!("   [sandbox] {line}"));
    let cloud = drive_store(&settings);
    let engine = Engine::open(dirs, &settings, folder, &session_id, progress, cloud)?;

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

    let changes = engine.changes()?;
    eprintln!("\n{} file(s) changed in the agent's copy:", changes.len());
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
        TurnEnd::Failed(msg) => {
            eprintln!("Failed: {msg}");
            ExitCode::from(2)
        }
    })
}

fn drive_store(settings: &Settings) -> Option<Arc<dyn gagent::drive::CloudStore>> {
    if settings.drive.is_connected() && settings.drive.has_client() {
        gagent::drive::DriveClient::new(&settings.drive)
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn gagent::drive::CloudStore>)
    } else {
        None
    }
}

fn doctor(dirs: &AppDirs, build_image: bool, clean: bool) -> Result<ExitCode> {
    let settings = Settings::load(dirs);
    let mut problems = 0;
    println!("Gagent {}", gagent::VERSION);
    println!("Data folder: {}", dirs.root().display());

    let status = docker_status(&settings.sandbox.image);
    println!("Docker: {}", status.describe());
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
        _ => problems += 1,
    }
    if clean {
        let n = sandbox::remove_all_gagent_containers().context("cannot clean containers")?;
        println!("Removed {n} leftover container(s).");
    }

    let key = settings.effective_api_key();
    if key.is_empty() {
        println!("Gemini API key: missing (set GEMINI_API_KEY or use the desktop app's settings)");
        problems += 1;
    } else {
        match GeminiClient::new(&key, &settings.gemini_base_url()).list_models() {
            Ok(models) => println!(
                "Gemini API key: works ({} models available; using {})",
                models.len(),
                settings.model
            ),
            Err(e) => {
                println!("Gemini API key: {}", e.user_message());
                problems += 1;
            }
        }
    }
    println!(
        "Google Drive: {}",
        if settings.drive.is_connected() {
            "connected"
        } else if settings.drive.has_client() {
            "configured, not connected"
        } else {
            "not configured (optional)"
        }
    );
    Ok(if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn models(dirs: &AppDirs) -> Result<ExitCode> {
    let settings = Settings::load(dirs);
    let key = settings.effective_api_key();
    if key.is_empty() {
        bail!("no Gemini API key: set GEMINI_API_KEY or save one in the desktop app's settings");
    }
    let list = GeminiClient::new(&key, &settings.gemini_base_url())
        .list_models()
        .map_err(|e| anyhow::anyhow!(e.user_message()))?;
    for m in list {
        println!("{:<40} {}", m.id, m.display_name);
    }
    Ok(ExitCode::SUCCESS)
}
