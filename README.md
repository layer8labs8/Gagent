# Gagent

Gagent is a native desktop coding agent for **Gemini**. Open a project folder, describe a task, and the
agent reads your code, edits files, runs them in a **Docker sandbox**, reads the logs, and fixes problems
until the code works. It runs as a normal desktop program (a single `gagent.exe` on Windows): no browser,
no local web server.

Gemini has no built-in "work on my folder" feature, so Gagent provides one:

```
your project folder                       (never touched while the agent works)
        |  private copy
        v
shadow workspace  <---- file tools ----  Gemini agent loop
        |  mounted at /workspace
        v
Docker sandbox (Ubuntu 24.04, no root, memory/CPU limits)  --- logs ---> back to the agent
        |
        v
you review the changes and click "Apply"  (every apply is backed up and can be undone)
```

## Features

- **Agent loop with tools**: list/read/write/edit/delete/search files and run shell commands. Gemini function
  calling, retries on rate limits, stop button, long-session history compaction.
- **Safe by design**
  - The agent works on a private copy; your folder changes only when you apply the result.
  - Path guard: no absolute paths, no `..`, no symlinks (a sandbox process cannot plant a link to host files).
  - The sandbox has no capabilities, runs unprivileged, has memory/CPU/process limits, and never sees your API key.
  - `.env` files and private keys are hidden from the agent by default.
- **Changes tab**: per-file diff, apply selected/all, discard, undo the last apply, optional auto-apply.
- **Sandbox tab**: Docker status, build/reset the image, network on/off, run your own commands.
- **Google Drive** (optional): upload/download the project, back up/restore chat sessions, and let the agent use
  Drive through tools. Uses the OAuth device flow and the narrow `drive.file` scope.
- **Command line tool** `gagent-cli` for scripts and headless use.

## Requirements

- A Gemini API key (free): <https://aistudio.google.com/apikey>
- [Docker](https://docs.docker.com/get-docker/) (Docker Desktop on Windows/macOS) for running code. Without
  Docker the agent can still read and edit files, but cannot run anything.
- To build from source: Rust 1.89 or newer. On Linux the app needs an X11/Wayland session, OpenGL and
  `libxkbcommon-x11`.

## Build and run

```sh
cargo run --release              # desktop app
cargo run --release --bin gagent-cli -- doctor   # check Docker + API key
```

Windows: `cargo build --release` creates `target\release\gagent.exe`. You can also build without installing Rust
using the `Dockerfile` in this repository (see `TODO.md`, not yet verified).

On first use Gagent builds the sandbox image `gagent-sandbox:latest` from `docker/sandbox/Dockerfile`
(about 1 GB, a few minutes). A smaller image without Node.js and C++:

```sh
docker build -t gagent-sandbox:latest --build-arg WITH_NODE=0 --build-arg WITH_CPP=0 docker/sandbox
```

If Docker Hub is rate limited, pass a mirror: `--build-arg BASE_IMAGE=mirror.gcr.io/library/ubuntu:24.04`
(or set `GAGENT_SANDBOX_BASE_IMAGE` for the app).

## Using it

1. Start Gagent, open **Settings**, paste your Gemini API key, press *Test key and load models*, save.
2. Choose a project folder and start a session.
3. Describe a task. Watch the tool calls in the chat and the **Activity** tab.
4. Open **Changes**, review the diff, and **Apply** (or enable auto-apply).

Command line:

```sh
gagent-cli run "fix the failing test" --folder ./myproject --apply
gagent-cli models
gagent-cli doctor --build-image
```

## Google Drive setup (optional)

1. In Google Cloud Console create a project and enable the **Google Drive API**.
2. Create OAuth credentials of type **TVs and Limited Input devices**.
3. Paste the client ID and secret in Settings, then press *Connect Google Drive* in the Drive tab and enter the
   shown code at <https://www.google.com/device>.

Gagent only sees files and folders it creates itself (`drive.file` scope), inside a folder named `Gagent`.

## Data and settings

Stored in the per-user data folder (`%LOCALAPPDATA%\Gagent`, `~/.local/share/Gagent`, ...), or `$GAGENT_HOME`:
`settings.json` (private permissions), `sessions/`, `shadow/` (private project copies), `backups/`, `logs/`.
Environment variables: `GEMINI_API_KEY`, `GAGENT_HOME`, `GAGENT_DOCKER`, `GAGENT_SANDBOX_BASE_IMAGE`,
`GAGENT_GEMINI_BASE_URL`, `GAGENT_GOOGLE_BASE_URL` (the last two are for tests and proxies).

Everything the agent reads is sent to Gemini. The sandbox has network access by default so that `pip`/`npm`
work; turn it off in the Sandbox tab for untrusted code.

## Project layout

```
src/agent.rs     agent loop (Gemini <-> tools)         src/sandbox.rs   Docker sandbox
src/gemini.rs    Gemini REST client                    src/shadow.rs    private copy, diff, apply, undo
src/tools.rs     tools the model can call              src/drive.rs     Google Drive client
src/workspace.rs path guard and file walking           src/engine.rs    ties a session together
src/gui/         egui desktop app                      src/bin/         gagent (GUI), gagent-cli
docker/sandbox/  sandbox image                         tests/           mock Gemini/Drive + real Docker tests
```

## Tests

```sh
cargo test        # unit tests, mock Gemini/Drive servers, and real Docker tests (skipped without Docker)
```

## License

MIT, see `LICENSE`.
