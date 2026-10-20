<div align="center">

<img src="assets/logo.svg" width="96" alt="Gagent logo">

# Gagent

**A coding agent that lives on your desktop and barely touches your computer.**

Open a folder, describe a task, and Gagent reads, edits, runs and tests your code until it works.<br>
Gemini · NVIDIA · Groq · any OpenAI-compatible API · one small native app · no browser · no Docker needed

**No telemetry · no account · nothing is sent to the authors.** Your files and keys go only to the AI service you choose.

[![CI](https://github.com/layer8labs8/Gagent/actions/workflows/ci.yml/badge.svg)](https://github.com/layer8labs8/Gagent/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-free%20to%20use%2C%20not%20for%20sale-blue.svg)](#license)
[![Release](https://img.shields.io/github/v/release/layer8labs8/Gagent?include_prereleases)](https://github.com/layer8labs8/Gagent/releases)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange)
![Telemetry](https://img.shields.io/badge/telemetry-none-brightgreen)
[![Stars](https://img.shields.io/github/stars/layer8labs8/Gagent?style=social)](https://github.com/layer8labs8/Gagent/stargazers)

[Quick start](#quick-start) · [What you need](#what-do-i-need-to-install) · [Safety](#safety-and-permissions) · [Disclaimer](#disclaimer-free-tool-use-at-your-own-risk) · [Providers](#providers) · [Command line](#command-line) · [Roadmap](ROADMAP.md) · [Build](#build-from-source)

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
  (free credits), [Groq](https://console.groq.com/keys) (very fast, free plan), or anything that speaks the OpenAI API: OpenRouter, Ollama, LM Studio, vLLM. Models without function
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

### Get an API key

| Service | Where | Cost |
|---|---|---|
| Google Gemini | [Google AI Studio](https://aistudio.google.com/apikey) ("Create API key"); [guide](https://ai.google.dev/gemini-api/docs/api-key) | free tier with a daily quota |
| Groq | [console.groq.com/keys](https://console.groq.com/keys) ("Create API Key", it starts with `gsk_`) | free plan with limits per minute and per day |
| NVIDIA-hosted models | [build.nvidia.com](https://build.nvidia.com): sign in, open a model, press "Get API Key" (or go to your [API keys](https://build.nvidia.com/settings/api-keys)); [quickstart](https://docs.api.nvidia.com/nim/docs/api-quickstart). The key starts with `nvapi-` | free credits for developers |
| Any OpenAI-compatible server | [OpenRouter](https://openrouter.ai), [Ollama](https://ollama.com) and [LM Studio](https://lmstudio.ai) run locally or in the cloud; the address ends in `/v1` | local servers need no key |

Paste the key in **Settings**. It is stored on your computer and sent only to the service you chose.

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
- **Team projects.** *New project* in the sidebar: add AI agents one by one (provider, model, role, token limit). The lead writes a plan
  with a contract between the parts, you approve it, and each agent does its tasks on its own model, leaving a note for the next one.
  A task that fails is taken over by another agent. A board shows every task and who does it; the Changes tab shows the result.
  Also from the command line: `gagent-cli team`.
- **It chooses for you.** Every free service has its own limits: Gemini a few requests a day, NVIDIA credits and 40 requests a minute,
  Groq fast answers but small token limits. Switch on **Auto** (next to the message box) and Gagent picks the best model that is still
  available for every message: it looks at what each model can do (code, images, size), how fast it answered before, and what is left of
  its quota. When a model runs out of quota or fails, the next one takes over and the chat tells you. In a team project, **Auto team**
  builds the team the same way (a strong lead, coders from different services so their limits are separate, a tester that sees images),
  and the lead gives each task to the model that fits it.
- **Switch model any time.** Provider and model sit next to the message box, in every session. Each model is marked with what it can
  do (`[images]`, `[code]`, `[reasoning]`), and speech, embedding and safety models are hidden unless you ask for them. One NVIDIA key
  works for all of NVIDIA's models; Gemini and other servers use their own keys.
- **Models that can see.** With a model marked `[images]` the agent gets `screenshot` (renders your page with the browser that is
  already installed, no extra download) and `view_image`, so it can check the layout and looks, not only the code. A picture costs
  tokens, so only the newest ones stay in the conversation and sessions are saved without pictures.
- **Request limit.** On NVIDIA's free endpoint Gagent keeps to 35 requests a minute (it allows about 40 for the whole account),
  shared by everything that uses the key. Change it in Settings.
- **Changes tab**: per-file diff, apply selected or all, discard, undo, optional auto-apply.
- **Quick checks** right after a file is written (Python, JavaScript, JSON syntax) so mistakes show up in the same step.
- **Notes for the agent.** Put a `GAGENT.md` (or `AGENTS.md`) in your project: how to build and test, conventions, things to
  avoid. The agent reads it at the start of every task (the first 4000 characters).
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
- **Do you trust this folder?** The first time you open a folder, Gagent asks (the command line asks too; scripts pass
  `--trust`). Files can hide instructions that try to steer the AI, so say yes only to folders you wrote or got from a
  source you trust. Settings → Project files lists how many folders you trust and can forget them.
- **Sending data out is asked about.** In Auto mode `curl -d/-F/-T`, `wget --post-file`, `scp`, `nc` and similar commands
  that send data to another computer ask first; testing your own server on `localhost` does not.
- **You are responsible for what you run.** AI models can be wrong, and Gagent is provided as it is, without warranty (see
  [LICENSE](LICENSE) and [SECURITY.md](SECURITY.md)). Read the Changes before you apply them, keep a backup, and do not open projects
  you do not trust.
- This is a seat belt, not a vault: a command on your own computer is not isolated like a container. If you run untrusted
  projects, choose **Docker** in Settings → Sandbox for real isolation.

## Providers

| Service | Key | Notes |
|---|---|---|
| Gemini | `GEMINI_API_KEY` or Settings | free tier has a daily quota |
| NVIDIA | `NVIDIA_API_KEY` or Settings | e.g. `nvidia/nemotron-3-ultra-550b-a55b` |
| Groq | `GROQ_API_KEY` or Settings | very fast; the free plan limits tokens per minute, so big requests go to other services |
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
gagent-cli run "fix the failing test" --auto       # picks the best available model, moves on when one runs out of quota
gagent-cli doctor

# A team made from the services you have keys for (strong lead, coders from different services, a tester that sees images)
gagent-cli team "a notes page with search" --auto-team --yes --apply

# A team: the lead plans, the agents do the tasks one by one, each on its own model
gagent-cli team "a notes page with search" --yes --apply   -a nvidia:nvidia/nemotron-3.5-super-vl-120b-a12b:lead   -a nvidia:nvidia/nemotron-3.5-lightning-30b-a3b:coder   -a nvidia:nvidia/nemotron-3.5-super-vl-120b-a12b:tester
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

## What is new

See [CHANGELOG.md](CHANGELOG.md): version 0.2.0 brings team projects, Groq, the automatic choice of the model, screenshots and
picture reading, and the model menu with reply times.

## Roadmap

Done: model switching with capability tags, the shared request limit, screenshots and picture reading, team projects (plan,
agents, board). Next: a first-run guide, plan mode, a token meter, streaming and installers. Later: **several agents working together**
(planner, workers and a reviewer, each with its own copy of the project and its own token budget), model routing between a
strong and a cheap model, MCP tools, local-model tuning and benchmarks. The full list, with the places where help is
welcome, is in [ROADMAP.md](ROADMAP.md).

## Contributing

Issues and pull requests are welcome. Please run `cargo test` first and keep changes light: no heavy dependencies and
nothing that starts in the background without being asked. [TODO.md](TODO.md) lists what is verified and what is still open.

## About this project

Gagent is a small independent project by one developer. I work with a modest computer and a small budget, not with the
hardware of a big lab, so I checked it with short tests on my own machine and kept it light on purpose, so that it also
runs on weak ones. If it is useful to you, a star on GitHub would make me happy, and bug reports, ideas and pull requests
help more than anything.

Follow along: [Instagram](https://www.instagram.com/layer8labs8) · [YouTube](https://www.youtube.com/@layer8labs8) ·
[TikTok](https://www.tiktok.com/@layer8labs8) · [Threads](https://www.threads.net/@layer8labs8) · [GitHub](https://github.com/layer8labs8)

Gagent is not affiliated with, endorsed by or sponsored by Google or NVIDIA. Gemini, Google AI Studio, NVIDIA, NIM and
build.nvidia.com are names of their owners and are used only to say which services Gagent can talk to.

## Disclaimer: free tool, use at your own risk

Gagent is a free tool that one person built and shares as it is. It is not a service and not a product with a promise.

- **It runs on your computer, with your keys.** You download it, you start it, you enter your own API keys and you choose the AI
  services and models. The authors run no server, have no account system, collect no data and receive nothing from you: no files,
  no keys, no usage numbers. They cannot see, stop or undo anything you do with it.
- **The AI decides what to write and run, not the authors.** Models are made and operated by other companies (Google, NVIDIA, Groq
  or whoever you connect). They can be wrong, and they can change files and run commands on your machine. What a model writes,
  says or does is not the work, advice or action of the authors of Gagent, and neither are those companies responsible for Gagent.
- **No warranty, no support, no liability.** Gagent is provided "as is". The authors are not responsible for lost or damaged
  files, wrong or harmful code, security problems, costs, quotas or bans at AI services, downtime, or any other loss or damage,
  however it arises. Nobody is obliged to fix a bug, answer a question or keep the project alive.
- **You decide.** Use Gagent only if you accept this. If you do not, do not use it. Read the Changes before you apply them, keep a
  backup, do not point it at data you may not share, and use Docker for projects you do not trust (see [SECURITY.md](SECURITY.md)).

This is the position of the authors, written in plain words. It is not legal advice, and the binding text is the [LICENSE](LICENSE).

## License

**Free to use, not for sale.** Gagent is source-available under the MIT License plus the Commons Clause and a few
additional terms; the full text is in [LICENSE](LICENSE).

- Anyone, including companies, may use it for any purpose and may change it. What you build with it is yours.
- You may not sell it: no selling of a modified version, of a product built from the code, or of paid hosting or paid
  support for it. If you want to, ask in an issue; permission can be given in writing.
- Keep the credit: modified versions and works that include a substantial part of the code must say
  "Based on Gagent by layer8labs8" with a link to this repository.
- The name Gagent, the G logo and layer8labs8 are not part of the license.

Because it limits selling, this is not an OSI-approved "open source" license; it is source-available.
The libraries Gagent is built with keep their own licenses; each release lists them in `THIRD-PARTY-LICENSES.txt`.
