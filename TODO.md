# Status and remaining work

## Verified

- Windows 10 (cross-built `x86_64-pc-windows-gnu` test programs, run natively): unit tests, mock model servers, the
  real Git Bash sandbox tests, the real WSL tests and the end-to-end pipeline (read -> run -> traceback -> edit -> run ->
  apply -> undo) pass. `gagent.exe` and `gagent-cli.exe` start and need only Windows system DLLs.
- Linux: `cargo check` of everything passes. The Docker and host-bash tests passed in an earlier round on Ubuntu.
- A real NVIDIA key with `nvidia/nemotron-3-ultra-550b-a55b`: native tool calling works; a bug fix and small
  multi-step projects were completed.
- Permissions (ask before installs and risky commands): unit tests and tool-level tests with an answering stub. The
  question card in the window and the terminal prompt are written but have not been clicked through by hand yet.

- Team projects: unit tests for plans and the window's event handling, tests against a scripted server (plan repair, order, handoff
  notes, retry by another agent, resume), and real runs with four NVIDIA models that built a calculator page with passing tests.
  The New project screen was looked at in a screenshot; the clicks (Create plan, Start, Stop, Continue) have not been tried by hand yet.

## Not verified

1. Gemini with a real key (only a mock server and an invalid-key request were used).
2. macOS, Docker Desktop on Windows and macOS, the MSVC build.
3. The text-format tool calling fallback against a model that really lacks function calling.
4. `release.yml` (builds the release programs when a version tag is pushed) has not run yet. `ci.yml` passed for the first Rust version.

## Ideas

- A "set up" button that installs a missing Git or Python after asking (the permission system exists now).
- Plan mode and a token meter in the window; stream the model's text while it is written.
- Syntax highlighting in code blocks and diffs; an icon embedded in `gagent.exe`.
- Publish the optional Docker image so the first Docker run does not build it.
