# Sentient OS v2.7: teardown

What was observed in [Sentient-OS-Labs/sentient-os](https://github.com/Sentient-OS-Labs/sentient-os)
(AGPL-3.0, v2.7 source, v2.7.1 release). The shipped DMG is covered in
[shipped-artifact-findings.md](shipped-artifact-findings.md).

> **Clean room.** Behaviour and architecture in our own words; no AGPL code or prompt text. Implementers
> may read this document but must not copy code, prompts or distinctive expression from upstream.

Labels: **O** observed in code, docs or artifact; **I** inferred; **U** unknown.

## 1. Summary

A macOS-only (Apple Silicon, 15.4+) SwiftUI menu-bar app of about 98k lines (O). Overnight, an
on-device Gemma model triages local data into short summaries; a frontier cloud model (via a bundled
OpenAI Codex CLI, the Claude Code CLI, or a custom endpoint) writes an 80–120 note Markdown vault and up
to 5 prepared actions a day; computer use ("Sidekick") runs on OpenAI's computer-use helper through
`codex exec`; Double Tap is one vision call (screenshot plus whole vault) via a closed relay, pasted with
the clipboard. Developed privately and mirrored to GitHub in bulk per release.

## 2. Findings

- **2.1 On-device model (O).** Gemma 4 E4B on LiteRT-LM (Metal), ~3 GB resident, used **only for
  per-item triage** (keep/junk/sensitive, title, summary) with a regex backstop for SSNs, card and
  passport numbers. Engine reloads every 40 items to dodge a GPU hang. Downloaded from a moving ref
  (`resolve/main`), never re-verified after install. No hardware-tier selection; Intel unsupported.
- **2.2 Providers (O).** Three backends dispatched by one switch, no provider abstraction. Codex is
  steered with undocumented config keys; a proxy silently falls back to a tool-less connection. The
  ChatGPT plan is read from an unverified JWT claim; model ids hard-coded. Custom endpoints must pass a
  vision probe (read back a random code from an image), a good idea. Third-party binaries are re-hosted
  (redistribution rights **U**).
- **2.3 Scheduling (O).** A **root LaunchDaemon** wakes the Mac at 3 AM and disables sleep; strictly
  serial, one item at a time; only the critical thermal state is honoured. A helper crash could leave
  sleep disabled (I).
- **2.4 Sources (O).** Files (keyed on date added: **edits never re-read**; no OCR), iMessage and
  WhatsApp (private SQLite, heuristic decoders), Apple Notes (schema-less protobuf; edits missed),
  Apple Mail (Envelope Index), Calendar (EventKit, no attendees). Gmail, Calendar, Slack, Outlook and
  Drive are read by the cloud CLI through vendor-hosted MCP; Gmail and Calendar need a ChatGPT or
  Claude plan. Writing style is raw sent-message samples, not a model.
- **2.5 Knowledge base (O).** Free-form LLM Markdown; constraints only in the prompt, nothing
  validates them. No entity store, provenance, embeddings, full-text index or graph DB. Links resolve
  by file-name stem, so same-name notes collide. Double Tap sends **the whole vault, up to 600 KB,
  silently truncated**. Nothing encrypted at rest; the mirror's key derives from a password in the
  share URL and the server decrypts per request.
- **2.6 Sidekick (O).** Codex with approvals and sandbox bypassed, proxying OpenAI's closed helper.
  Targets AX element ids. Good: mutations refused unless the app's state was read first; failed calls
  never replayed. Missing: step cap, deadline, app allow/deny list, confirmation for irreversible
  actions, a kill switch, and verified outcomes.
- **2.7 Double Tap (O).** Right Cmd twice; one screenshot (≤ 2000 px), whole vault and writing samples
  in one call through a closed `workers.dev` relay by default (model and retention **U**). The focused
  field is not read via accessibility. Clipboard paste; non-text clipboard content lost; delivery not
  verified.
- **2.8 Proactive cards (O).** A judge reads 7 days of summaries plus calendar; up to 5 prepared
  actions; ranking is the LLM's order. Cards stored as JSON in UserDefaults.
- **2.9 Trust and process (O).** Telemetry **on by default**, including an LLM-written free-text
  workflow phrase and install/uninstall pings after opt-out. Sparkle updates forced and silent every
  3 h, one signing key. No unit-test target or build CI; a CLA bot; one external PR merged. Users ask
  for Linux, localisation, an action log and default-deny data access.

## 3. Structural weaknesses

1. Core capabilities belong to others (OpenAI's helper, Codex, vendor MCP servers).
2. Undocumented ground: private formats, undocumented CLI keys, impersonated OAuth client.
3. Shallow memory: no provenance, search, entities, validation or incremental updates.
4. Nightly batch: edits and deletes missed; needs a root daemon and closed-lid wake.
5. Trust: telemetry and closed relay on by default, secret in a URL, forced updates, no encryption at
   rest, no action log.
6. macOS on Apple Silicon only, English only.
7. Unmeasured quality: no benchmarks, evals or CI.

See [../PLAN.md](../PLAN.md) for how Nugget answers these.
