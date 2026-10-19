<div align="center">

<img src="assets/logo.svg" width="96" alt="Gagent logo">

# Gagent

**A coding agent that lives on your desktop and barely touches your computer.**

Open a folder, describe a task, and Gagent reads, edits, runs and tests your code until it works.<br>
Gemini · NVIDIA · any OpenAI-compatible API · one small native app · no browser · no Docker needed

[![CI](https://github.com/layer8labs8/Gagent/actions/workflows/ci.yml/badge.svg)](https://github.com/layer8labs8/Gagent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/layer8labs8/Gagent?include_prereleases)](https://github.com/layer8labs8/Gagent/releases)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange)

[Quick start](#quick-start) · [What you need](#what-do-i-need-to-install) · [Safety](#safety-and-permissions) · [Providers](#providers) · [Command line](#command-line) · [Build](#build-from-source)

</div>

<!--
  DEMO SLOT. A screen recording goes here (for example docs/demo.gif or an mp4).
  Left empty on purpose for now.
-->

<br>

## Why Gagent

- **Light.** One native program written in Rust (egui). No Electron, no browser, no local web server, no virtual machine.
  Commands run directly on your own computer, at low priority, so a weak laptop stays cool and usable.
- **Bring your own model.** Gemini (free tier), NVIDIA's hosted models on [build.nvidia.com](https://build.nvidia.com)
  (free credits), or anything that speaks the OpenAI API: OpenRouter, Ollama, LM Studio, vLLM. Models without function
  calling work too, through a text fallback.
- **Your project is never touched while it works.** The agent edits a private copy. You review a diff, apply it, and can
  undo the last apply.
- **It asks first.** Installing packages, stopping programs, running downloaded scripts or touching files outside the
  project opens a question. Truly dangerous commands are refused, always.
- **Frugal with tokens.** Old file contents leave the conversation, long output is shortened, repeated reads of unchanged
  files cost almost nothing, and a task stops at a token budget you choose.
- **Your keys stay yours.** They live in a settings file on your computer and go only to the service you picked.

## Quick start

**Windows 10 / 11**

1. Download `Gagent.exe` from the [Releases](https://github.com/layer8labs8/Gagent/releases) page.
2. The agent runs commands with bash, which comes with [Git for Windows](https://git-scm.com). If you do not have it:
   `winget install Git.Git` (or run `scripts\install-windows.ps1 -Install`).
3. Start `Gagent.exe`, open **Settings**, pick a service, paste your API key, press *Test key and load models*.
4. Choose a project folder, describe a task, review the **Changes** tab, press **Apply**.

**Ubuntu / Debian**

```sh
scripts/install-ubuntu.sh --build     # window libraries, Rust, build, install to ~/.local/bin
gagent
```

**macOS** should work (`cargo build --release`) but has not been tested yet.

Get a key: [Gemini](https://aistudio.google.com/apikey) (free) · [NVIDIA](https://build.nvidia.com) (free credits, starts
with `nvapi-`) · any OpenAI-compatible server (its address ends in `/v1`; local servers such as Ollama need no key).

## What do I need to install?

| | Needed? |
|---|---|
| An API key (or a local model server) | **Yes** |
| Git for Windows (gives the agent bash) | **Windows only.** Linux and macOS already have bash |
| Python, Node.js, a C compiler, ... | Only what *your project* needs. Gagent uses what is installed |
| Docker or WSL | **No.** Optional, for people who want isolation (see below) |

Gagent never installs anything silently. If the agent needs a package that is missing, it **asks you** and shows the exact
command (*Allow*, *Always allow this kind*, *Deny*). A tool that is missing and not allowed simply fails and the agent
tells you what it needs.

## Features

- Agent loop with tools: list, read, search, write, edit, delete files and run commands; retries on rate limits, stop
  button, long-session history compaction, switch provider at any time.
- **Changes tab**: per-file diff, apply selected or all, discard, undo, optional auto-apply.
- **Quick checks** right after a file is written (Python, JavaScript, JSON syntax) so mistakes show up in the same step.
- Sandbox tab, activity log, light and dark blue theme, drag and drop files into the workspace.
- `gagent-cli` for scripts and headless use.

## How it works

```
your project folder                  (never touched while the agent works)
        |  private copy
        v
shadow workspace  <--- file tools --- AI agent loop (Gemini / NVIDIA / OpenAI-compatible)
        |  commands start here
        v
bash on your computer (or Docker / WSL)  --- logs --->  back to the agent
        |
        v
you review the changes and press "Apply"   (every apply is backed up and can be undone)
```

## Safety and permissions

Commands run on your own computer by default, so Gagent is careful:

- The agent only sees a private copy of the project. Paths are checked: no absolute paths, no `..`, no symlinks.
  `.env` files and private keys are hidden from the agent unless you allow them.
- Commands run at low priority with your keys and passwords removed from their environment.
- **Three modes** (button next to the message box, or Settings):

  | Mode | Asks before |
  |---|---|
  | **Ask** | every command |
  | **Auto** (default) | installing packages, stopping programs by name, running downloaded scripts, anything outside the project |
  | **Allow all** | nothing |

- **Always refused**, in every mode: shutting down or restarting, formatting or partitioning disks, registry edits,
  `sudo`, `rm -rf` on the root or home folder, fork bombs, encoded PowerShell.
- This is a seat belt, not a vault: a command on your own computer is not isolated like a container. If you run untrusted
  projects, choose **Docker** in Settings → Sandbox for real isolation.

## Providers

| Service | Key | Notes |
|---|---|---|
| Gemini | `GEMINI_API_KEY` or Settings | free tier has a daily quota |
| NVIDIA | `NVIDIA_API_KEY` or Settings | e.g. `nvidia/nemotron-3-ultra-550b-a55b` |
| OpenAI-compatible | `OPENAI_API_KEY` or Settings + base URL | OpenAI, OpenRouter, Ollama, LM Studio, vLLM |

Text-only models (no function calling) are supported through a `<tool_call>` text fallback; *Settings → Tool mode* lets you
force it.

## Where commands run

| Option | Memory | Isolated | Needs |
|---|---|---|---|
| **This computer** (default) | none extra | no, but guarded | bash (Git for Windows on Windows) |
| WSL Ubuntu | VM, about 1 GB | no (reaches `/mnt/c`) | WSL |
| Docker | Docker Desktop is heavy | **yes** | Docker |

*Settings → Sandbox → Run commands in:* Automatic (this computer, otherwise Docker, otherwise WSL) · This computer ·
Docker · WSL.

## Command line

```sh
gagent-cli run "fix the failing test" --folder ./myproject --apply
gagent-cli run "add tests" --yes                 # do not ask before installs
gagent-cli --provider nvidia run "add tests" --model nvidia/nemotron-3-ultra-550b-a55b
gagent-cli --provider nvidia models
gagent-cli doctor
```

In a terminal the CLI asks the same questions as the app. Without a terminal (scripts, CI) the answer is *no* unless you
pass `--yes`.

## Build from source

Rust 1.89 or newer. On Linux the window needs X11 or Wayland, OpenGL and `libxkbcommon-x11`.

```sh
cargo run --release                          # desktop app
cargo run --release --bin gagent-cli -- doctor
cargo test                                   # unit tests, mock model servers, real sandbox tests (skipped when unavailable)
```

Docker is optional. To use it, the first run builds the image `gagent-sandbox:latest` from `docker/sandbox/Dockerfile`.

## Data and settings

Stored per user (`%LOCALAPPDATA%\Gagent`, `~/.local/share/Gagent`, ...) or in `$GAGENT_HOME`: `settings.json`, `sessions/`,
`shadow/` (private project copies), `backups/`, `logs/`. API keys are saved in `settings.json` as plain text, so keep that
folder private. Environment variables: `GEMINI_API_KEY`, `NVIDIA_API_KEY`, `OPENAI_API_KEY` (used when no key is saved),
`GAGENT_HOME`, `GAGENT_SHELL` (use a specific bash), `GAGENT_DOCKER`, `GAGENT_SANDBOX_BASE_IMAGE`.

## Project layout

```
src/agent.rs       agent loop (model <-> tools)         src/policy.rs      allowed / ask / refused commands
src/tools.rs       tools the model can call             src/permission.rs  the questions to the user
src/host.rs        bash on this computer                src/sandbox.rs     Docker sandbox, engine choice
src/wsl.rs         WSL sandbox                          src/shadow.rs      private copy, diff, apply, undo
src/gemini.rs      Gemini client                        src/openai.rs      NVIDIA / OpenAI-compatible client
src/gui/           egui desktop app                     src/bin/           gagent (GUI), gagent-cli
tests/             mock model servers and real sandbox tests
```

## Contributing

Issues and pull requests are welcome. Please run `cargo test` first. See [TODO.md](TODO.md) for what is verified and what is
still open.

## License

MIT, see [LICENSE](LICENSE).
