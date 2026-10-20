# Changelog

All notable changes to Gagent. The newest version is first.

## 0.2.1

### New

- **One window for everything.** There is no separate "project" any more. Write what you want in a session: a small job goes to one
  agent, a big one (an app, a site, a long list of wishes) is planned by a lead, the AIs that fit are chosen automatically and the team
  starts after your yes. The plan and the tasks sit above the message box. `/team` and `/solo` in front of a message decide for
  you, *Big jobs* in Settings sets the rule, and a setting starts plans without asking. `gagent-cli team` still lets you choose every
  agent by hand.
- **Gagent finds out which AIs work.** When it starts, and before every team, the best models are asked a tiny question: which answer,
  which are blocked in your account, which are out of quota. The ones that answered lately are preferred, a slow model counts against
  itself, and a model that fails is left out at once (up to ten are tried for one request). If the lead of a team cannot make the
  plan, the other agents try in turn instead of stopping.
- **One place for the services and models.** Settings → *AI services* has the keys of all services together (no switching) and a
  button *Check keys and models*. *Models* lists every model with a dot (works and how fast, slow, out of quota, blocked, not tested)
  and a tick to leave one out. Models the service does not have are not shown. Any service that speaks the OpenAI API (OpenRouter,
  DeepSeek, Mistral, Ollama...) can be added by hand.
- **Voice.** The microphone button writes what you say into the message box (Groq's Whisper when your Groq project allows it,
  otherwise a Gemini model; the sound is deleted at once). Settings can read the answers aloud with a Gemini voice. Recording uses
  PowerShell on Windows and `arecord` on Linux, so nothing heavy is added.
- **Web search and reading pages.** Two new tools, `web_search` (DuckDuckGo, no key needed) and `web_fetch` (a page or text file as
  plain text, in parts when it is long). You are asked before every search and before every new host in the modes Ask and Auto; the
  question shows the full address. Your own server on `localhost` is read without a question. Addresses on a private network, `file:`
  and addresses with a user name are never read, a redirect to another host is asked about again, and what comes from the internet is
  marked as data, not as instructions. Switch it off in Settings.
- **Git checkpoint, diff and commit.** When the project is a git repository, Gagent saves a checkpoint of it right before the agent's
  changes reach it (a commit object under `refs/gagent/`: your branch, index and files are not touched; the newest 20 are kept). The
  Changes panel has a Git box with the branch, **Show git diff** for the files the agent changed, and **Commit...**, which commits
  those files only. The command line prints the checkpoint too. Turn it off in Settings.
- **Pictures in the message.** A picture attached to a message (the + button, or drop it on the window) is sent along with it; with
  Auto, a model that can see is chosen for that message.
- **Tokens per company.** The header of a session shows how many tokens it used and how they split between Gemini, NVIDIA, Groq and
  any other server you added.
- **Notes for the agent.** A `GAGENT.md` (or `AGENTS.md`) in the project root is read at the start of every task: how to build and
  test, conventions, things to avoid.
- **Auto is the default.** New installs choose the model by themselves for every message (turn it off in Settings).

### Safer

- **Do you trust this folder?** Gagent asks once for every folder before the agent reads files or runs commands in it (window and
  command line; scripts pass `--trust`). The answer is remembered; Settings can forget all trusted folders. Folders you opened with an
  earlier version are asked about once more.
- **Sending data out is asked about.** In Ask and Auto modes, commands that send data to another computer (`curl -d/-F/-T`,
  `wget --post-file`, PowerShell `-Method Post`, `scp`, `sftp`, `nc`, `rclone`, `aws s3 cp`...) ask first. Testing your own server on
  `localhost` and plain downloads do not.
- A plain **Disclaimer** section in the README and a sixth license term: Gagent is a free tool that runs on your own computer with
  your own keys; the authors run no server, collect nothing and owe no support.

### Changed

- **A simpler window.** The message box has only +, the microphone, the permission mode and send. The bottom of the sidebar is just
  *Settings* (a line appears only when something is wrong). The text of your own message starts at the left of its bubble, not at
  the right. The Changes panel and the first page say what is true about applying changes.
- **Groq models updated** to the ones the service lists today (`openai/gpt-oss-120b`, `openai/gpt-oss-20b`, `qwen/qwen3.8-27b`). A
  model that is blocked in your Groq project (console.groq.com/settings/project/limits) is shown as "blocked in your account" and
  skipped, without spoiling the other models of the service.
- Gemini model names that no longer exist for new users are replaced by current ones (`gemini-flash-latest`, `gemini-3.6-flash`).
- A settings file that starts with a byte order mark (saved by Notepad) is no longer treated as damaged.
- The agent is told to write the user's language with its own letters (for Azerbaijani: ə ı ö ü ğ ş ç).
- A small line after the first answer says a star on GitHub helps; one click hides it for good.

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
