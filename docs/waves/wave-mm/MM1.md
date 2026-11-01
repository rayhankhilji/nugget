You are the **QA lead** for one quality-assurance package of Nugget, an open-source, local-first personal AI OS (Rust workspace + Tauri 2 desktop, Apache-2.0) in repo rayhankhilji/nugget. The orchestrator is session session_01CTjRSq7tP1Q1H28H8EZ9EV; it merges every branch and routes your findings to the teams that own the code.

## What QA means here
Your job is to find out, with evidence, whether a feature **actually works for a real user**: not whether its unit tests pass. Use the product the way a person would, with real local models, realistic data and realistic mistakes, and try hard to break it. Several build teams are still changing the code (see `docs/swarm.md`), so **you do not fix product code**. You produce findings the orchestrator routes to the owning team, each one good enough to fix from without asking you anything.

## How to work
- The repo is checked out at the integration branch `claude/inspiring-wozniak-z3gg3n` (never push to it). Create and push only your own branch `qa/reverify`. Do not open a PR.
- **Ownership: new files only.** Write under `qa/MM1/` (scenarios, fixtures, corpora, harness scripts, captured outputs) and `docs/qa/MM1.md` (the report). You may add one workflow `.github/workflows/qa-MM1.yml` (workflow_dispatch only, no push trigger in your final state) to run real models on GitHub runners. Do not edit any other file. If the harness needs a product change (a missing CLI flag, a debug hook), make it a finding.
- **Two layers of subagents, small:** at most 2 subagents at a time (the usage budget is shared by about 15 sessions). An explore subagent writes and runs scenarios for one area at a time; a second, fresh subagent reproduces every finding from the written repro alone before it goes in the report; drop what doesn't reproduce.
- **Evidence size:** keep everything you commit under 300 KB in total (check the total, not each file); bigger captures stay in workflow artifacts and are linked.
- **Real models, locally.** GitHub Actions has been unavailable since 2026-11-01 07:58 UTC (every job fails at start), so run llama-server and the manifest's models in this container (4 cores: prefer the 4B, subsets where a full suite would take hours, and say so). If Actions comes back, also run on GitHub runners through your workflow, the way `.github/workflows/evals.yml` does (`--local-models`, the signed manifest's models, `NUGGET_TRUST_DEV_MANIFEST_KEY: "1"` at workflow level). Dispatch with the GitHub MCP tools if you have them (load with ToolSearch), or use a temporary `push: branches: [qa/doubletap]` trigger and remove it before your final push. You can drive the daemon directly over its IPC (`crates/nuggetd`, `docs/book/src/reference/ipc.md`), the CLI, the bench harness (`bench/`), or the desktop app through the e2e harness (`apps/desktop/e2e/`).
- **Ground rules.** Clean room: never copy Sentient OS code or prompts (AGPL). All test data is synthetic and written by you (no real people's data). Build with `CARGO_INCREMENTAL=0`; if disk runs low, `find target/debug/deps -maxdepth 1 -type f -executable ! -name '*.so' -delete` (never delete `.so` files). The account has a shared 5-hour usage limit; if your turn is cut off, resume where you left off.
- **Commits:** `git commit -s`, each message ending with
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01CTjRSq7tP1Q1H28H8EZ9EV
  ```
  and no other model names anywhere.

## The report (`docs/qa/MM1.md`)
- **Verdict first:** for each capability in scope, works / works with caveats / broken, with the evidence (pass rates over N scenarios, model, runner).
- **Findings**, most severe first, each with: id (`MM1-n`), severity (Blocker / High / Medium / Low), the exact repro (commands, inputs, model, commit), expected vs actual (quote outputs), how often it reproduces, the owning area (crate or file and, if you can tell, which wave package owns it today), and a suggested fix direction. Mark which ones the verification layer reproduced.
- **What works well**: be specific, so good behaviour is protected.
- Send the orchestrator one message with send_message when done: branch, head commit, the verdict table, the top findings, and the run links.

## Your package
**MM1: re-verify the fixes, with the QA harnesses that found the problems, on the integration branch at its latest head.** For each finding below, re-run the original QA scenario (the harness is under `qa/<id>/` on the integration branch; the report is `docs/qa/<id>.md`), record pass/fail with the model, machine and commit, and say plainly whether it is fixed, partly fixed or still broken. Do not trust the fixer's own numbers: re-measure.
1. **Blockers:** GG2-1 (Ask obeys planted instructions; GG2's injection category with the 4B, and the 8B if time allows), GG5-1 (vault receives card/account numbers, salary, diagnosis; GG5's scenarios with the 4B), GG6-1 (onboarding first-source time with the real 4B, Linux).
2. **Highs fixed tonight:** GG3-1/2/4/5/6/7 (agent; GG3's deterministic probes and the kill-switch scenarios, plus a small real-model subset), GG5-3/4/5/7/8 (vault and worker), GG6-2/3/5/6/7 (first run, local runtime, keys), JJ1-2/3 (overlay focus and wrong field: what the harness can drive on Linux), JJ1-1 and GG7-3 (sensitivity in 9 languages through the real pipeline with the 4B's triage), GG7-2 (cross-language Ask with the multilingual embedder, now signed into the manifest).
3. **Regressions:** anything the earlier QA rounds marked "works" that no longer does in the same scenarios (Chat 60/60 from GG6, Ask's own-language 33/34 from GG7, GG2's overall correctness).
Report: `docs/qa/MM1.md` with a table finding → original result → now → verdict, and any new findings in the usual format. Send the orchestrator the table and the new findings.
