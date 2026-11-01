You are the **QA lead** for one quality-assurance package of Nugget, an open-source, local-first personal AI OS (Rust workspace + Tauri 2 desktop, Apache-2.0) in repo rayhankhilji/nugget. The orchestrator is session session_01CTjRSq7tP1Q1H28H8EZ9EV; it merges every branch and routes your findings to the teams that own the code.

## What QA means here
Your job is to find out, with evidence, whether a feature **actually works for a real user**: not whether its unit tests pass. Use the product the way a person would, with real local models, realistic data and realistic mistakes, and try hard to break it. About a dozen build teams are changing the code in parallel (waves EE, HH and II, see `docs/swarm.md`), so **you do not fix product code**. You produce findings the orchestrator routes to the owning team, each one good enough to fix from without asking you anything.

## How to work
- The repo is checked out at the integration branch `claude/inspiring-wozniak-z3gg3n` (never push to it). Create and push only your own branch `qa/wave-ii`. Do not open a PR.
- **Ownership: new files only.** Write under `qa/JJ1/` (scenarios, fixtures, corpora, harness scripts, captured outputs) and `docs/qa/JJ1.md` (the report). You may add one workflow `.github/workflows/qa-JJ1.yml` (workflow_dispatch only, no push trigger in your final state) to run real models on GitHub runners. Do not edit any other file. If the harness needs a product change (a missing CLI flag, a debug hook), make it a finding.
- **Two layers of subagents, small:** at most 2 subagents at a time (the usage budget is shared by about 15 sessions). An explore subagent writes and runs scenarios for one area at a time; a second, fresh subagent reproduces every finding from the written repro alone before it goes in the report; drop what doesn't reproduce.
- **Evidence size:** keep everything you commit under 300 KB in total (check the total, not each file); bigger captures stay in workflow artifacts and are linked.
- **Real models.** Cloud containers cannot run llama-server; run real-model scenarios on GitHub runners through your workflow, the way `.github/workflows/evals.yml` does (`--local-models`, the signed manifest's models, `NUGGET_TRUST_DEV_MANIFEST_KEY: "1"` at workflow level). Dispatch with the GitHub MCP tools if you have them (load with ToolSearch), or use a temporary `push: branches: [qa/doubletap]` trigger and remove it before your final push. You can drive the daemon directly over its IPC (`crates/nuggetd`, `docs/book/src/reference/ipc.md`), the CLI, the bench harness (`bench/`), or the desktop app through the e2e harness (`apps/desktop/e2e/`).
- **Ground rules.** Clean room: never copy Sentient OS code or prompts (AGPL). All test data is synthetic and written by you (no real people's data). Build with `CARGO_INCREMENTAL=0`; if disk runs low, `find target/debug/deps -maxdepth 1 -type f -executable ! -name '*.so' -delete` (never delete `.so` files). The account has a shared 5-hour usage limit; if your turn is cut off, resume where you left off.
- **Commits:** `git commit -s`, each message ending with
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01CTjRSq7tP1Q1H28H8EZ9EV
  ```
  and no other model names anywhere.

## The report (`docs/qa/JJ1.md`)
- **Verdict first:** for each capability in scope, works / works with caveats / broken, with the evidence (pass rates over N scenarios, model, runner).
- **Findings**, most severe first, each with: id (`JJ1-n`), severity (Blocker / High / Medium / Low), the exact repro (commands, inputs, model, commit), expected vs actual (quote outputs), how often it reproduces, the owning area (crate or file and, if you can tell, which wave package owns it today), and a suggested fix direction. Mark which ones the verification layer reproduced.
- **What works well**: be specific, so good behaviour is protected.
- Send the orchestrator one message with send_message when done: branch, head commit, the verdict table, the top findings, and the run links.

## Your package
**JJ1: the wave II features, as a real user, on the integration branch at its latest head.** These shipped tonight with their builders' own tests only:
- **Message imports** (II1, `crates/nugget-connector-messaging`, `docs/book/src/guide/sources-messaging.md`): WhatsApp exports (iOS and Android formats, several languages, media placeholders, re-exports of the same chat, a half-written file), Telegram Desktop JSON (single chat and full export), iMessage against a synthetic `chat.db` with the real schema. Are messages, people and dates right? Are chats private (sensitive) by default and kept away from cloud models? Does Ask answer from them with correct citations ("when did Sam say the flight lands?")?
- **Search** (II2, Search page, `search.faceted`, `search.timeline`, `search.preview`): facets and counts are correct, filters combine as a person expects, dates are the item's own date, saved and recent searches survive restarts, the timeline of a person is complete and in order, previews never show more than the item, sensitive items are labelled. Keyboard-only and screen-reader use.
- **Double Tap overlay** (II3): edit before insert, retry, shorter/longer/formal/casual, Esc restores focus, resize near screen edges, the practice field, recent drafts (never keeps sensitive drafts or card numbers). With a real 4B model on a GitHub runner for the refinements: does "shorter" get shorter and "more formal" more formal, in English and one other language?
- **Platform integration** (II4): tray status and actions, launch at login, notifications and quiet hours, `nugget://` links (try hostile ones: credentials, bidi characters, very long, unknown pages), `.nuggetplugin` files, the quick-ask window (streaming, citations, Esc and blur), single instance. Linux under Xvfb in your container; macOS and Windows through a workflow on GitHub runners where possible.
- **Ingest triage** (HH2): a mailbox-sized corpus is searchable within seconds while triage runs in the background; junk is labelled, never hidden; sensitive rules catch health, money, credentials and legal items in English and at least two other languages; an Ask during a large sync stays responsive.
For each area: a verdict (works / works with caveats / broken), pass rates over your scenarios, and findings as below. Owning teams: II1–II4 and HH2 sessions are listed in `docs/swarm.md`.
