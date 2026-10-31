You are the **swarm lead** for one work package of Nugget, an open-source, local-first personal AI OS (Rust workspace + Tauri 2 desktop, Apache-2.0) in repo rayhankhilji/nugget. The orchestrator is session session_01CTjRSq7tP1Q1H28H8EZ9EV; it merges every branch.

## How to work
- Read "How to work" in `docs/waves/wave-cc/CC1.md` (ground rules, daemon features seam, desktop conventions, build hygiene, CI, commits with `git commit -s` and the trailer, no model names elsewhere) and follow it, with the differences below.
- Your branch is `ii/platform-integration`, created from the checked-out integration branch `claude/inspiring-wozniak-z3gg3n` (740d754 or later). Never push to the integration branch; open no PR. If you need the latest integration branch later, merge it in (never rebase or force-push).
- **Two layers, small:** at most 2 subagents at a time (the usage budget is shared by about 20 sessions). A build subagent implements a piece; a fresh review subagent that did not write it reviews the diff for correctness, security, privacy and test gaps; you fix what holds up.
- **Toolchain:** CI runs Rust 1.99.1 — run `rustup update stable` first; clippy `-D warnings` must be clean on it. Run the full workspace tests and `pnpm run e2e` before your final push (visual screenshots may differ in this container; run with `E2E_VISUAL_TOLERANCE=1` and say which screens your change affects so the orchestrator refreshes their baselines from CI).
- **In flight elsewhere (stay out, or additive changes only, listed in your report):** nugget-draft (EE2), nugget-knowledge/graph extraction (EE1), nugget-vault + knowledge worker (HH3), nuggetd ask.rs + Chat.tsx (EE3), nugget-retrieve + nugget-infer (DD4), nugget-agent (DD3), nugget-computer/browser/policy + agent UI (HH1), nugget-ingest triage (HH2), nugget-proactive + .ics extraction (BB2), release/updater (EE5).
- **Quality bar:** every feature works end to end in the real app (daemon IPC + desktop UI + docs), strings in all 9 catalogs plus review lists, accessible (keyboard, screen reader, RTL), with unit, integration and e2e coverage. Privacy: nothing leaves the device unless the user chose a cloud provider; every network call is in the egress ledger; sensitive items are never shown or sent where they shouldn't be. If time runs short, finish fewer pieces completely rather than many partially — the pieces below are in priority order.
- **Report** when done: one send_message to the orchestrator (branch and head, what was built with evidence, tests, edits outside your ownership, contract changes, known gaps).

## Your package
**II4: Nugget as a well-behaved desktop app on every OS.** Build the platform integration a daily-use app needs:
1. **Tray / menu bar**: status (indexing, model ready, agent running), quick actions (Ask, Double Tap, pause indexing, stop all agents), open app; native on macOS (template icon, menu bar), Windows (notification area) and Linux (StatusNotifierItem/AppIndicator with fallbacks).
2. **Launch at login** (opt-in in onboarding and Settings; LaunchAgent / Registry Run key / XDG autostart) and **start hidden**.
3. **Native notifications** for the morning brief and agent confirmations needing attention (opt-in, quiet hours, never including sensitive text, clickable to the right page).
4. **Deep links** `nugget://` (open an item, a search, a page; validated; no actions without confirmation) and **file associations** for `.nuggetplugin` (opens the install/approve flow).
5. **Global quick-ask window** (a small Spotlight-style ask box on a shortcut, separate from Double Tap) with cited answers.
6. Single-instance handling, graceful quit (daemon stays or stops per setting), and correct behaviour after sleep/resume.
Verified on real runners (real-os.yml jobs for macOS, Windows, Linux X11/Wayland) where automatable; document what is manual. Strings in all catalogs; accessible.
You own: apps/desktop/src-tauri/src/{main.rs tray/setup parts, new tray.rs, autostart.rs, notify.rs, deeplink.rs, quickask.rs}, tauri config (deep link / file association sections; EE5 owns signing/updater sections), Settings UI for these, docs/book/src/guide/desktop.md.
