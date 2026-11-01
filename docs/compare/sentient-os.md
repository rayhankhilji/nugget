# Nugget and Sentient OS: a sourced comparison

Checked October 2026 against Sentient OS v2.7 source and v2.7.1 release. Every Sentient claim cites
Nugget's clean-room notes: [teardown](../research/sentient-os-teardown.md) (§ numbers) or
[artifact](../research/shipped-artifact-findings.md). Only observed facts are used; later versions may
differ. Nugget claims point at code or the book. No Sentient code or prompts are quoted
([clean-room rules](../../CONTRIBUTING.md#clean-room)). No head-to-head benchmark exists.

| Area | Sentient OS (observed) | Nugget |
|---|---|---|
| Platforms | macOS 15.4+, Apple Silicon (§1, §2.1) | macOS, Windows, Linux; installers built but unsigned, no release yet |
| Licence | AGPL-3.0, CLA (§2.9) | Apache-2.0, DCO ([`LICENSE`](../../LICENSE)) |
| Indexing | Nightly, serial, root wake daemon; edits to files and Notes never re-read (§2.3, §2.4) | Continuous, versioned, reconciling ([`nugget-ingest`](../../crates/nugget-ingest/src/lib.rs)) |
| Memory | LLM Markdown, no provenance, index or validation; whole vault sent to Double Tap (§2.5) | Hybrid search with `[n]` citations; typed graph, every claim with a verbatim quote; vault with three-way merge ([`nugget-knowledge`](../../crates/nugget-knowledge/src/lib.rs), [`nugget-vault`](../../crates/nugget-vault/src/lib.rs)) |
| At rest | Not encrypted (§2.5); cards in UserDefaults (§2.8) | SQLCipher, key in the OS keychain ([`nugget-store`](../../crates/nugget-store/src/lib.rs)) |
| What leaves | Telemetry on by default; Double Tap via closed relay (§2.7, §2.9) | No telemetry; egress ledger; sensitive items never to cloud models ([privacy](../book/src/guide/privacy.md#the-egress-ledger)) |
| Connectors | Cloud CLI via vendor-hosted MCP, plan required (§2.4; artifact) | Direct APIs with your own OAuth (Google, Microsoft), IMAP, CalDAV, Slack, Notion, GitHub, Linear, iMessage, WhatsApp and Telegram imports |
| Sharing with AIs | Hosted mirror, password in the URL, server-side decryption (artifact; §2.5) | Local MCP (stdio, loopback HTTP), keyed-hash tokens, per-client scopes, audit ([`nugget-mcp`](../../crates/nugget-mcp/src/lib.rs)) |
| Double Tap | Screenshot plus whole vault, clipboard paste, unverified (§2.7) | Accessibility text first, rule-based field classification, budgeted cited context, read-back verified insert ([`nugget-draft`](../../crates/nugget-draft/src/lib.rs)) |
| Computer use | OpenAI's helper via `codex exec`; no step cap, allow-list, confirmations or kill switch; unverified "done" (§2.6; artifact) | Own agent and policy engine: risk classes, confirmations, budgets, kill switch, hash-chained log, verify-after-act ([`nugget-policy`](../../crates/nugget-policy/src/lib.rs), [`nugget-agent`](../../crates/nugget-agent/src/lib.rs)) |
| Proactive | LLM-ranked, up to 5 actions a day (§2.8) | Deterministic generators with evidence, explainable learned ranking, approval of a previewed diff ([`nugget-proactive`](../../crates/nugget-proactive/src/lib.rs)) |
| Plugins | None described; sources built into the app (§2.4) | Signed WASM components with approved, enforced capabilities ([`nugget-plugin`](../../crates/nugget-plugin/src/lib.rs)) |
| Updates | Forced and silent every 3 h, one signing key (§2.9; artifact) | Opt-in channel, signature-checked; no release to update to yet |
| Engineering | No unit-test target or build CI (§2.9) | CI on three OSes, tests, `cargo deny`, benchmarks ([`ci.yml`](../../.github/workflows/ci.yml)) |

## Where Sentient is ahead

- A packaged, signed macOS app with onboarding; Nugget has no release.
- Apple Notes and Apple Mail sources (§2.4); hosted connectors are easier to start with than Nugget's
  bring-your-own Google OAuth client (one-time Cloud setup, weekly re-sign-in in Testing mode).
- A frontier model writes its notes, which may read better than rendered pages; nobody has measured it.
- Nugget's own gaps are in [PLAN.md](../PLAN.md), including an open Double Tap prompt-injection
  Blocker and an unmeasured cloud latency.

Ideas Nugget took, in our own words (§2.1, §2.2, §2.6): a vision probe that reads back a random code;
no action in an app whose state was not read first; no replay of failed actions; a regex backstop
next to model triage.
