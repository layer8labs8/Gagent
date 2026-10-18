# Status and remaining work

Written so the work can be resumed later. Everything below is in English on purpose.

## What was verified

- Unit tests and integration tests pass (`cargo test`): path guard, shadow copy/diff/apply/undo, tools,
  Gemini client + agent loop against a scripted mock server (retries, cancel, parallel calls, thought
  signatures preserved), Google Drive client against an in-memory fake (device flow, upload/update/download,
  token refresh), and the **real Docker sandbox** (limits, no root, network off, timeout, cancel, symlink
  safety, host user ownership, huge output).
- Full pipeline with a mock Gemini and real Docker: read -> run -> traceback -> edit -> run -> apply -> undo.
- The desktop GUI was started under Xvfb and driven with xdotool: session start, agent run, Activity,
  Changes (diff, apply), Sandbox terminal, Drive connect/upload/browse (fake Google server), session restore.
- The old C code and the committed `gagent.exe` were removed (the C code only simulated the sandbox, the API
  and Drive; the exe had no source in the repository).

## Not verified (needs a real environment)

1. **Real Gemini API call with a valid key.** Only a mock server and an invalid-key request were used.
   Check: function-calling request shape (`parameters.type` is sent in upper case like the official SDKs),
   thought-signature round trip with Gemini 3 models, default model name `gemini-2.5-flash`.
2. **Real Google Drive.** Only the fake server was used. Needs an OAuth client of type "TVs and Limited Input
   devices"; confirm the `drive.file` scope works with the device flow for your client.
3. **Windows.** The code was written for Windows (no console window, `CREATE_NO_WINDOW` for Docker calls,
   `dunce` paths, system certificate store) but never run on Windows. Check Docker Desktop bind-mount paths and
   that the container user (1000:1000 on Windows/macOS) can write to the mounted folder.
   `cargo check --lib --target x86_64-pc-windows-gnu` (including the GUI) compiles cleanly.
4. **`Dockerfile`** (build the Windows exe with MinGW) and **`.github/workflows/ci.yml`** were not run.
5. macOS was not tested.

## Ideas / improvements

- Embed an icon in `gagent.exe` (needs a `build.rs` with `embed-resource` and an `.ico`); add a release
  workflow that attaches `gagent.exe` to GitHub releases.
- Publish the sandbox image to a registry so the first run does not build it.
- Stream model output (`streamGenerateContent`) for live text.
- Persist the Activity log with the session; show token/cost totals.
- Ask-before-run mode for sandbox commands; per-session network toggle.
- Support more than one project folder per session; a "continue after step limit" button.
- Syntax highlighting in code blocks and diffs; Markdown tables and links.
- A native-window UI test harness (egui_kittest) instead of the manual Xvfb/xdotool flow.
- Container cleanup after a hard crash relies on the instance lock in `engine::claim_instance`; containers
  also expire by themselves after 24 hours.
