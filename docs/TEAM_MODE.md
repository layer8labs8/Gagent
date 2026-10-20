# Team projects (design draft)

Several AI agents, each on its own model, build one project together. This is a plan, not code yet.
The rule stays: the app must stay light. Team mode is off until you create a project.

## What the user sees

**Where.** In the sidebar, under *New session*, a second button: **New project**. Projects are listed in their own section.

**Create a project (one screen).**
1. Name, folder (or empty), and the goal in a few sentences.
2. **Team.** *Add agent* one by one. Each row has: provider and model, role (*Auto*, Planner, Coder, Tester, Reviewer, Docs, or a custom one),
   and limits: token budget, requests per minute (default from the provider, for example 40 for NVIDIA), and context size (read from the
   model list when known, otherwise typed). One agent is the **Lead** (star).
3. **Roles by the Lead.** A switch, on by default: the Lead decides who does what. It can also propose more agents from the models you
   have listed, and you accept or decline.
4. **Preview.** Total limits and an honest estimate: "up to about 300 requests, about 9 minutes at 40 per minute".
5. **Create plan.** Nothing runs yet.

**Plan review.** The Lead writes the plan; you see it as a board and can edit or approve it. Each task shows its owner, its files and what
"done" means. Approval can be switched off for trusted setups.

**Work view.** Left: the task board (todo, doing, done, blocked). Centre: the team feed, every message labelled with the agent that wrote it.
Right: changed files and diffs. Top: token meter and request meter per agent, Pause and Stop. Permission questions still appear, labelled with the agent.

## How it works

**The plan is data, not chat.** The Lead returns a plan as JSON that the program validates: tasks with id, title, description, files,
dependencies, owner, and an acceptance check (a command to run, or a short rule). Bad plans are sent back to the Lead, at most twice.
The plan lives in the project folder (`.gagent/project/plan.json`) together with a short `notes.md` (conventions, decisions).

**Who stands where.** The Lead sees a profile for every agent (model, context size, tool-calling quality, speed, request limit, token budget,
your tags such as "cheap" or "strong at code"). Rules the program enforces regardless of the Lead:
- A task is only given to an agent whose context can hold it: prompt budget is about 60 percent of the context.
- Big, risky work (planning, integration, review) goes to the strongest agents; small, well-bounded work (tests, docs, boilerplate) to cheap ones.
- Models without native function calling use the text fallback and get simpler tasks.
- If the Lead's assignment breaks a rule, the scheduler fixes it and says so in the feed.

**Talking to each other.** Not free chat (every message would cost every agent tokens). Agents write structured messages on the board:
*handoff* (what I did, which files, what the next agent must know), *question* (to the Lead), *blocked*. The Lead answers questions and can
add tasks. Round limits stop endless discussion.

**Reading the code.** Every agent has the same tools as today (list, search, read line ranges). On top, the program builds a **repo map**
(file list and the signatures of functions and types, made by code, not by an AI), cached and refreshed after each merge. Agents start from
the map instead of exploring, and the task text names the files to open.

**The scheduler** is plain code, not an AI. It starts ready tasks while respecting:
- one **shared rate limiter per API key** (for example 35 requests per minute for a 40 limit), with a fair share for each agent;
- per-agent and total token budgets;
- a parallelism cap (default 2) so a weak computer stays cool;
- file locks: tasks that touch the same files never run together; test commands run one at a time;
- retries and backoff on 429 (already in the program), and pause when a daily quota is empty, with resume later.

**How many agents.** As many as you add (ten or more). "Parallelism 2" only means how many work at the same moment; the others wait for their turn
or for the task they depend on. You can raise it. The real limit is the request rate shared by the key, not the computer.

**Example: a web page with five agents.** Lead (plan and contract), HTML, CSS, JS, tester. The first task is a **contract** written by the Lead
into `notes.md`: the element ids and classes, the function names, the file names. Without it the JS looks for ids the HTML never made.
Then HTML goes first; CSS and JS both depend on it and run side by side (two at once); the tester starts when both are done.
The tester also checks that every id and class used by the JS and CSS exists in the HTML.

**Reading the screen.** A role *Visual tester* for models that can see images (for example NVIDIA's vision models or Gemini). Two small tools:
- `screenshot`: renders a local HTML file with the browser that is already on the computer (Edge on Windows, Chrome or Chromium on Linux) in
  headless mode and saves a PNG into the workspace. No extra install. First version: the page as it loads. Later, clicks and typing through the
  browser's debugging connection.
- `view_image`: sends a PNG from the workspace to a vision model, with a limit on the number of images per task (an image costs a few thousand
  tokens). Models without vision never get this tool; they test with `node` and text checks.
The visual tester describes what is wrong ("the buttons overflow on a narrow window") and the Lead turns it into fix tasks.

**Finishing.** Last tasks are always: integrate, run the tests (Tester), review the diff (Reviewer). Failures become fix tasks, with a cap on
rounds. Then the usual **Changes** tab: review, apply, undo.

**Saved and resumable.** The plan, notes, step log and budgets are stored, so a closed app, a crash or an empty quota never loses the work.

## Limits that fit

| Thing | Rule |
|---|---|
| Context | per-model window; prompts are built to about 60 percent of it; old output is compacted as today |
| Requests | one limiter per key, shared by all agents using that key |
| Tokens | budget per agent and a total; the project stops cleanly when one is used up |
| CPU and heat | parallel agents max 2 by default; commands run at low priority, one test at a time |
| Cost preview | shown before the plan is created and updated as the plan grows |

## Safety

Same rules as one agent: private copy, permission questions (shown with the agent name), refused commands, no secrets in the environment.
An agent only gets the tools its role needs (a Reviewer cannot run installs).

## Status

Done: Part A (models and limits), Part B (eyes), Part C with the New project screen, the board and `gagent-cli team`. The engine: the lead's plan is
checked and repaired, tasks run in dependency order, each agent gets the contract and the notes of the tasks before it, and a task that
fails is taken over by another agent (twice at most). A real run with four NVIDIA models built a calculator page with passing tests in
three minutes (180 000 tokens); the tester even took a screenshot. Not yet: parallel work, the repo map, fix tasks from the tester and reviewer.

## Build order: three parts

**Part A: models and limits (for every session, not only projects).**
- One NVIDIA key works for every model on the free endpoint (the model name goes in the request), so switching models needs no new key.
  Gemini and other servers have their own keys.
- A model catalog made from the model names: chat or not, can it see images, code, reasoning, size. Utility models (speech, embeddings,
  safety, driving) are hidden from the picker unless "Show every model" is on.
- The picker next to the message box switches provider and model at any time, in any session, and shows what each model can do.
- One shared request limiter per server (35 a minute on NVIDIA's free endpoint, which allows about 40), so sessions and agents share one budget.

**Part B: eyes (for every session and project).**
- Images in the providers (Gemini and OpenAI-compatible), a `view_image` tool for models that can see, and a `screenshot` tool that renders
  a local page with the browser already on the computer. If the chosen model cannot see, the agent says so and offers a model that can.
- Image budget per task, because a picture costs a few thousand tokens.

**Part C: team projects.**
1. Agent profiles, the sub-agent runner with its own context, labels per agent. Plan schema, the Lead's planning step, plan review screen,
   sequential execution with handoffs, New project screen and work view.
2. Repo map, tester and reviewer loops with fix tasks, resume after quota or crash, parallel work (file locks, separate private copies and
   the three-way merge that *apply* already uses), Lead proposing more agents, agent files you can share.

## Open decisions

- Default Lead: the user's choice (star), or the strongest agent automatically?
- Plan approval: on by default?
- Default parallelism: 1 or 2?
- Start: build steps 1 and 2 first (sequential team), parallel later?
