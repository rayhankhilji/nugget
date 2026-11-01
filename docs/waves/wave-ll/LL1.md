You are the **swarm lead** for one work package of Nugget, an open-source, local-first personal AI OS (Rust workspace + Tauri 2 desktop, Apache-2.0) in repo rayhankhilji/nugget. The orchestrator is session session_01CTjRSq7tP1Q1H28H8EZ9EV; it merges every branch.

## How to work
- Read "How to work" in `docs/waves/wave-cc/CC1.md` (ground rules, daemon features seam, desktop conventions, build hygiene, CI, commits with `git commit -s` and the trailer, no model names elsewhere) and follow it, with the differences below.
- Your branch is `ll/scale-2`, created from the checked-out integration branch `claude/inspiring-wozniak-z3gg3n` (the head named in the session's first message, or later). Never push to the integration branch; open no PR. If you need the latest integration branch later, merge it in (never rebase or force-push).
- **Two layers, small:** at most 2 subagents at a time (the usage budget is shared by about 20 sessions). A build subagent implements a piece; a fresh review subagent that did not write it reviews the diff for correctness, security, privacy and test gaps; you fix what holds up.
- **Toolchain:** CI runs Rust 1.99.1 — run `rustup update stable` first; clippy `-D warnings` must be clean on it. Run the full workspace tests and `pnpm run e2e` before your final push (visual screenshots may differ in this container; run with `E2E_VISUAL_TOLERANCE=1` and say which screens your change affects so the orchestrator refreshes their baselines from CI).
- **In flight elsewhere (stay out, or additive changes only, listed in your report):** nugget-draft (EE2, paused), nugget-proactive (BB2, paused), nugget-knowledge extraction (EE1), nugget-ingest triage and sensitivity (HH2), retrieval and embeddings (DD4), release/updater/app lifecycle (EE5).
- **Quality bar:** every feature works end to end in the real app (daemon IPC + desktop UI + docs), strings in all 9 catalogs plus review lists, accessible (keyboard, screen reader, RTL), with unit, integration and e2e coverage. Privacy: nothing leaves the device unless the user chose a cloud provider; every network call is in the egress ledger; sensitive items are never shown or sent where they shouldn't be. If time runs short, finish fewer pieces completely rather than many partially — the pieces below are in priority order.
- **Report** when done: one send_message to the orchestrator (branch and head, what was built with evidence, tests, edits outside your ownership, contract changes, known gaps).

- **GitHub Actions is unavailable** (every job fails at start since 2026-11-01 07:58 UTC, an account-level problem). Measure locally in this container; state the machine and its core count with every number. If Actions comes back, use it as before.

## Your package
**LL1: fast at any size, part two (II5's open items).** Read bench/README.md "Scale" and II5's notes in PLAN first. In priority order, each with before/after numbers from `bench perf --backend real` (local, same container, same item count):
1. **Ingest writes in batches:** the pipeline commits one item per transaction; use `Store::upsert_items_with_chunks` (II5) for runs of changes, keeping per-item error isolation and the change feed. Target: seeding/first sync throughput ×3 at 100k items without raising freshness p50.
2. **Search tail at 500k:** p95 327 ms → under 150 ms (FTS query plan, candidate caps, vector search limits); no recall loss on the memory eval set.
3. **Paging where the UI stops at a limit:** `search` and `egress.recent` gain a cursor (additive IPC), the Search page and the Privacy ledger page through results with II5's VirtualList.
4. **Graph overview top-N:** a GraphStore method for the overview instead of scanning entities.
5. **Proactive stops rescanning the whole store each run** (incremental over the change feed) — additive, coordinate in your report since BB2 owns nugget-proactive and is paused.
You own: nugget-store performance paths, nugget-ingest's write path (not triage), the search/egress IPC paging and their UI lists, bench perf.
