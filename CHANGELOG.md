# Changelog

All notable changes to Gagent. The newest version is first.

## Unreleased

- **Notes for the agent.** A `GAGENT.md` (or `AGENTS.md`) in the project root is read at the start of every task: how to build and
  test, conventions, things to avoid.

- **Do you trust this folder?** Gagent asks once for every folder before the agent reads files or runs commands in it (window and
  command line; scripts pass `--trust`). The answer is remembered; Settings can forget all trusted folders. Folders you opened
  with an earlier version are asked about once more.
- **Sending data out is asked about.** In Ask and Auto modes, commands that send data to another computer (`curl -d/-F/-T`,
  `wget --post-file`, PowerShell `-Method Post`, `scp`, `sftp`, `nc`, `rclone`, `aws s3 cp`...) ask first. Testing your own server on
  `localhost` and plain downloads do not.

- A plain **Disclaimer** section in the README and a sixth license term: Gagent is a free tool that runs on your own computer with
  your own keys; the authors run no server, collect nothing and owe no support.

## 0.2.0

### New

- **Team projects.** *New project* in the sidebar: add AI agents one by one (provider, model, role, token limit). The lead writes a
  plan with a contract between the parts, you approve it, and each agent does its tasks on its own model, leaving a note for the next
  one. A board shows every task and its owner; a task that fails is taken over by another agent. Also from the terminal:
  `gagent-cli team`.
- **Groq** as a third service next to Gemini and NVIDIA (key from console.groq.com/keys, or `GROQ_API_KEY`).
- **Automatic choice of the model.** Every free service has its own limits. The **Auto** button next to the message box picks the best
  available model for every message (what it can do, how fast it answered before, what is left of its quota). When a model runs out of
  quota, does not exist or fails, the next one takes over and the chat says so; a model that is slow to answer is raced against the
  next one. **Auto team** builds a team the same way. Terminal: `--auto`, `--auto-team`.
- **Pictures.** Models that can see get two tools: `screenshot` (renders a page with the browser that is already installed) and
  `view_image`, so the agent can check how something looks. Pictures are dropped from old messages and never saved.
- **Model menu** with tags (`[images]`, `[code]`, `[reasoning]`), measured reply times and quota marks; utility models (speech,
  embeddings, safety) are hidden unless asked for. Provider and model can be switched in any session.
- **Request limit** shared by everything that uses the same key (35 a minute on NVIDIA's free endpoint, 25 on Groq).
- `GAGENT_DEBUG=1` prints what is sent to the AI service and how long it took (never the key).

### Security and responsibility

- New `SECURITY.md`: what can go wrong (the AI can be wrong, commands run on your own computer, instructions hidden in files, files go
  to the AI service, keys are saved as plain text), how to reduce the risk, and how to report a problem.
- The license has a fifth term: you are responsible for checking what the AI models write and do; the authors are not.
- A short reminder in the window: AI can make mistakes, check the changes, keep a backup.

### Changed

- **Fewer broken tool calls.** Some models send tool calls that almost work: a made-up tool name (`bash`, `functions.read_file`,
  `run_command<|channel|>commentary`), JSON with raw line breaks, a list instead of a command line, JSON instead of file text.
  Gagent now reads all of these. A call that cannot be read gets a clear error with an example of the right call (before, it ran with
  empty arguments and the model saw "missing argument"), and a command that fails because of quoting (`python -c "..."`), a missing
  program or a wrong path gets a short tip. After three failed calls in a row the model is told to stop and read the error; a model
  that sends eight broken calls in a row is stopped.
- Models that often send broken tool calls are marked `error-prone` in the model menu and chosen less often by **Auto**.
- Tool calls that fail because of a used-up quota are remembered for hours instead of being retried.
- Agents no longer leave `HANDOFF` note files in the project.

## 0.1.1

- License: free to use, not for sale (MIT plus the Commons Clause and four additional terms). See `LICENSE`.
- Each release lists the libraries built into Gagent and their licenses (`THIRD-PARTY-LICENSES.txt`).

## 0.1.0

- Commands run on the user's own computer by default (Git Bash on Windows), at low priority, without secrets in their environment;
  Docker and WSL stay optional.
- Questions before installs and other risky commands (modes Ask, Auto, Allow all); dangerous commands are always refused.
- NVIDIA and any OpenAI-compatible server next to Gemini; a text format for models without function calling.
- Token savings: old file contents leave the conversation, short tool descriptions, shortened command output, a token budget.
- New blue theme and logo; Google Drive removed.
