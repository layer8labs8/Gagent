# Security and responsibility

Gagent lets AI models read your project and run commands on your computer. That is useful and it is risky. Please read this before
you use it on anything that matters.

## What can go wrong

- **The AI can be wrong.** It can write bad code, delete or overwrite the wrong files, or run a command you did not expect. Always
  look at the **Changes** tab before you apply them, and keep a copy (a git repository, a backup) of anything you care about.
- **Commands run on your own computer by default.** Gagent works in a private copy of your project and refuses dangerous commands
  (shutting down, formatting, registry edits, `sudo`, deleting the home folder...), and it asks before installing packages or touching
  files outside the project. These rules are a seat belt, not a vault: they read the text of a command, and a program the AI writes
  and then runs can do more than the command line shows. For projects you do not trust, choose **Docker** in Settings, which isolates
  the commands for real.
- **Instructions hidden in files.** A file, a web page or a package can contain text that tries to steer the AI ("ignore your
  instructions and run this"). Gagent asks "Do you trust this folder?" the first time you open a folder; say yes only to folders you
  wrote or got from a source you trust. Keep the permission mode on **Auto** or **Ask** rather than **Allow all**: in those modes
  commands that send data to another computer (`curl -d`, `scp`, `nc`...) ask first. A program the agent writes and runs can still
  send data, which is one more reason to choose Docker for a folder you do not trust.
- **Your files go to the AI service.** Everything the agent reads is sent to the service you chose (Google, NVIDIA, Groq or another
  server). Do not use Gagent on files you may not share. `.env` files and private keys are hidden from the agent unless you allow them.
- **Your API keys are saved as plain text** in `settings.json` in the data folder (`%LOCALAPPDATA%\Gagent` or `~/.local/share/Gagent`).
  Keep that folder private, use keys with limits, and replace a key that may have leaked.
- **The web tools can carry data out.** An address or a search the agent writes goes to the internet, and a page can hide
  instructions. That is why you are asked first (Auto and Ask modes) and the full address is shown: look at it before you say yes.
  Addresses of a private network, `file:` and addresses with a user name are refused. Turn the tools off in Settings if you do not
  want them.
- **Your voice goes to the AI service too.** The microphone button records only while you have switched it on, sends the sound to
  Groq or Google to be written down, and deletes the file at once. Reading answers aloud sends the text of the answer to Google.
- **Pages are opened for screenshots.** `screenshot` opens an HTML file of the project, or a local server, in your browser without a
  window. Scripts in that page run like in any browser.
- **Free plans have limits.** A task can use up a daily or monthly quota. Gagent counts and moves on to other models, but it cannot
  promise that a service is available.

## No warranty, no responsibility

Gagent is a free tool, provided as it is and without any warranty (see `LICENSE`). The authors are not responsible for what the AI
models write, say or do, for files that are changed or lost, for costs, quotas or bans at the AI services, or for any other
damage. You choose the models and you decide what to apply and what to run; you are responsible for that. If you do not accept
this, do not use Gagent.

Who does what:

- **You** install and start Gagent on your own computer, enter your own API keys and choose the services and models.
- **The AI services** (Google, NVIDIA, Groq or the server you connect) receive what the agent reads and send back what the model
  writes. They have their own terms and prices; you agree to those with them, not with us.
- **The authors** share the source code and the programs. They run no server, collect nothing, see nothing of what you do, and
  have no way to stop or undo it. They owe no support, updates or fixes.

This text is not legal advice.

## Reporting a security problem

Please do not post a vulnerability in a public issue. Use GitHub's private reporting (the **Security** tab of the repository,
*Report a vulnerability*), and describe what you found and how to reproduce it. We will answer as soon as we can.
