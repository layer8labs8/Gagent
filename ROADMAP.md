# Roadmap

Where Gagent is going. Nothing here is a promise or a date; it is the direction. The guiding rule stays the same for
every item: **the app must stay light** on memory, CPU and disk, so that it also runs well on a weak computer.

Items marked **help wanted** are good places to join in. Open an issue before you start, so that we do not do the same
work twice.

## Today

- Native desktop app and command line tool, one small program, no browser, no virtual machine.
- Gemini, NVIDIA-hosted models and any OpenAI-compatible server; text fallback for models without function calling.
- Private copy of the project, diff review, apply and undo.
- Commands run on your own computer (Git Bash on Windows) with guard rails; Docker and WSL are optional.
- Asks before installs and risky commands (modes: Ask, Auto, Allow all).
- Token savings: history compaction, short tool descriptions, cleaned output, token budget.

## Next

Small, concrete steps that make the daily use better.

- **First-run guide**: a short checklist in the window (API key, bash, Python) with a button that installs a missing tool
  after asking.
- **Plan mode**: the agent writes a plan, you edit or approve it, then it works through the steps.
- **Token and cost meter** in the window, per task and per day.
- **Streaming**: show the model's text while it is written, not after.
- **Better diffs and code blocks**: syntax highlighting, side-by-side view.
- **Project instructions**: read a short `GAGENT.md` in the project (coding style, test command) and keep it in every
  request.
- **Installers**: a winget package, a `.deb`, an AppImage, a signed Windows build. *(help wanted)*
- **macOS** testing and a notarized build. *(help wanted)*
- **Chat tools**: search and export sessions, rename, pin.
- **Translations** of the interface. *(help wanted)*

## Later: many agents working together

The big idea is not one assistant, but a small team that stays cheap.

- **Planner, workers and reviewer.** A planner splits a task, workers do the parts, a reviewer reads the result and runs
  the tests before anything reaches your project.
- **Parallel agents on separate private copies.** Each worker gets its own shadow copy of the project; the results are
  merged with the same three-way merge that apply already uses, and conflicts are shown to you.
- **Model routing.** A strong model plans and reviews, a cheap or local model does the simple edits. Every agent has its
  own token budget, and the total stays under the limit you set.
- **Specialists.** Test writer, refactoring agent, documentation agent, dependency updater, security reviewer.
- **Shared notes.** A small scratch file the agents use to hand work to each other instead of repeating the whole history.
- **Delegation tool.** The main agent can start a sub-agent for a narrow job (search the code base, run the test suite and
  summarize) and get back only a short result, which keeps the main conversation small.
- **Permissions per agent.** Each agent gets only the tools and folders its job needs.

## Later: the ecosystem

- **MCP support**: use tools from Model Context Protocol servers (browser, databases, issue trackers).
- **Local models**: tuned prompts and tool formats for small models run with Ollama or LM Studio.
- **Benchmarks**: `gagent-cli bench` with a set of small repair tasks, to measure quality and token use of a model or a
  change in the prompt. *(help wanted)*
- **CI mode**: run Gagent in a pipeline, for example to propose a fix when a test fails.
- **Plugins**: add your own tools in a small, safe way.
- **Remote and team use**: a headless server mode, shared sessions, audit log of what the agent ran.
- **Lighter sandboxes**: a tiny isolated runner that needs no Docker and no virtual machine.

## How to take part

1. Pick an item, or propose your own, in an issue.
2. Keep it small and light: no heavy dependencies, nothing that starts in the background without being asked.
3. Add tests (`cargo test`), keep the README and this file in step with what you change.

Have an idea that is not here? Open an issue and tell us what problem it solves.
