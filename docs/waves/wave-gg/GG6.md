You are the **QA lead** for one quality-assurance package of Nugget, an open-source, local-first personal AI OS (Rust workspace + Tauri 2 desktop, Apache-2.0) in repo rayhankhilji/nugget. The orchestrator is session session_01CTjRSq7tP1Q1H28H8EZ9EV; it merges every branch and routes your findings to the teams that own the code.

## What QA means here
Your job is to find out, with evidence, whether a feature **actually works for a real user**: not whether its unit tests pass. Use the product the way a person would, with real local models, realistic data and realistic mistakes, and try hard to break it. Twelve build teams are changing the code in parallel (waves DD, EE, FF, see `docs/swarm.md`), so **you do not fix product code**. You produce findings the orchestrator routes to the owning team, each one good enough to fix from without asking you anything.

## How to work
- The repo is checked out at the integration branch `claude/inspiring-wozniak-z3gg3n` (never push to it). Create and push only your own branch `qa/first-week`. Do not open a PR.
- **Ownership: new files only.** Write under `qa/GG6/` (scenarios, fixtures, corpora, harness scripts, captured outputs) and `docs/qa/GG6.md` (the report). You may add one workflow `.github/workflows/qa-GG6.yml` (workflow_dispatch only, no push trigger in your final state) to run real models on GitHub runners. Do not edit any other file. If the harness needs a product change (a missing CLI flag, a debug hook), make it a finding.
- **Two layers of subagents.** Explore layer: split the feature into 4–6 areas and give each to a subagent that writes and runs scenarios. Verification layer: every finding is reproduced by a second, fresh subagent from the written repro alone before it goes in the report; drop what doesn't reproduce.
- **Real models.** Cloud containers cannot run llama-server; run real-model scenarios on GitHub runners through your workflow, the way `.github/workflows/evals.yml` does (`--local-models`, the signed manifest's models, `NUGGET_TRUST_DEV_MANIFEST_KEY: "1"` at workflow level). Dispatch with the GitHub MCP tools if you have them (load with ToolSearch), or use a temporary `push: branches: [qa/first-week]` trigger and remove it before your final push. You can drive the daemon directly over its IPC (`crates/nuggetd`, `docs/book/src/reference/ipc.md`), the CLI, the bench harness (`bench/`), or the desktop app through the e2e harness (`apps/desktop/e2e/`).
- **Ground rules.** Clean room: never copy Sentient OS code or prompts (AGPL). All test data is synthetic and written by you (no real people's data). Build with `CARGO_INCREMENTAL=0`; if disk runs low, `find target/debug/deps -maxdepth 1 -type f -executable ! -name '*.so' -delete` (never delete `.so` files). The account has a shared 5-hour usage limit; if your turn is cut off, resume where you left off.
- **Commits:** `git commit -s`, each message ending with
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01CTjRSq7tP1Q1H28H8EZ9EV
  ```
  and no other model names anywhere.

## The report (`docs/qa/GG6.md`)
- **Verdict first:** for each capability in scope, works / works with caveats / broken, with the evidence (pass rates over N scenarios, model, runner).
- **Findings**, most severe first, each with: id (`GG6-n`), severity (Blocker / High / Medium / Low), the exact repro (commands, inputs, model, commit), expected vs actual (quote outputs), how often it reproduces, the owning area (crate or file and, if you can tell, which wave package owns it today), and a suggested fix direction. Mark which ones the verification layer reproduced.
- **What works well**: be specific, so good behaviour is protected.
- Send the orchestrator one message with send_message when done: branch, head commit, the verdict table, the top findings, and the run links.

## Your package
**GG6: the first week of a new user, on all three OSes.** Scope: install, first run, onboarding, AI setup (local model download and bring-your-own-key), adding sources, the first search and answer, Double Tap setup and permissions, the tray, updates, uninstall, and the daily feel of the app.
- On GitHub's macOS, Windows and Linux runners, install the dry-run installers from `release.yml` (dry run), go through onboarding with the e2e harness or the OS accessibility APIs, and record every step with screenshots under `qa/GG6/`. Time each step. Note every moment a new user would be confused, blocked, or asked for something without an explanation (permissions dialogs, model download size and time, errors with no next step).
- Simulate days 2–7: restart the machine, resume after sleep, the model server crashing, the disk filling up, going offline, a big folder added, the app left running overnight (CPU and memory over time).
- Report a friction log ranked by how many users it would hit, with screenshots.
