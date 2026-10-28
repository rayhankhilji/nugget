# Nugget and Sentient OS

Nugget started as an answer to [Sentient OS](https://github.com/Sentient-OS-Labs/sentient-os), an
AGPL-licensed, macOS-only personal AI. This page compares the two **factually**. Everything said about
Sentient OS comes from Nugget's clean-room research notes,
[`docs/research/sentient-os-teardown.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/research/sentient-os-teardown.md)
(v2.7 source, v2.7.1 release) and
[`docs/research/shipped-artifact-findings.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/research/shipped-artifact-findings.md)
(the shipped v2.7.1 app), and may be out of date for later versions. Everything said about Nugget is
either measured (with a link to the numbers) or labelled as not done yet.

The full, sourced comparison, with the research section behind every claim about Sentient and the
code path behind every claim about Nugget, is
[`docs/compare/sentient-os.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/compare/sentient-os.md)
(checked October 2026). This page is the short version.

**The short version:** Nugget's memory layer (indexing, freshness, search, the knowledge graph,
privacy accounting) and its local MCP server run in the daemon today, and differ from Sentient's by
design. Sentient ships features Nugget is still wiring in: drafting in any text field (Double Tap),
computer use (Sidekick), proactive cards, and packaged macOS releases. Nugget has no installer at all
yet.

> **Status:** Double Tap, the agent and its safety policy, proactive cards, plugins and the Microsoft
> 365 and SaaS connectors exist as tested crates and are being wired into the daemon and app now
> (wave W). Until that lands, treat them as not available.

## Where Nugget stands today

| Area | Sentient OS (as observed) | Nugget (today) |
|---|---|---|
| Platforms | macOS 15.4+, Apple Silicon only | Builds and tests on macOS, Windows and Linux in CI. **No installers yet**; real-machine testing on macOS and Windows is still pending |
| Licence | AGPL-3.0, with a CLA bot | Apache-2.0, DCO, no CLA |
| How data gets indexed | Nightly batch from a root LaunchDaemon that wakes the Mac at 3 AM; one item at a time | Continuous, event-driven, per-user (no root). **Measured:** a local file change is searchable in 0.60 s at p95 (5 000-file corpus, test embedder) |
| Edits and deletes | Files and Notes keyed on date added or created, so **edits are never re-read** | Versioned items; edits, creates, deletes and renames all propagate. **Measured:** propagation rate 1.0 for every kind |
| Search | No full-text index, no embeddings; Double Tap sends the **whole vault, up to 600 KB, silently truncated** | Hybrid full-text and vector search with rank fusion and MMR; context packed under an explicit token budget with `[n]` citations. **Measured:** recall@10 1.000 hybrid vs 0.905 full-text on the labelled eval set; the synthetic-corpus gate is **missed by 0.2 points** at standard scale ([details](architecture/benchmarks.md)) |
| Knowledge structure | Free-form Markdown written by an LLM; constraints only in the prompt; no provenance, no validation; links resolve by file-name stem | Typed entity graph; every fact carries evidence with a verbatim quote. Rendered as a Markdown vault with stable-id links and three-way merge of your edits. Runs in the daemon |
| Mail and calendar | Gmail and Calendar read by a cloud CLI through vendor-hosted MCP connectors; needs a ChatGPT or Claude plan | Direct APIs with **your own** Google OAuth client (read-only scopes), plus IMAP and CalDAV for any provider. No plan or intermediary required. Cost: a one-time Google Cloud setup and, in Testing mode, a weekly re-sign-in |
| Models | On-device Gemma for triage only; frontier work through bundled OpenAI Codex CLI or Claude Code CLI; model ids hard-coded | One provider layer (Anthropic, OpenAI, any OpenAI-compatible server) routed by tier; managed local `llama-server` with a signed, pinned manifest; capabilities proven by probe. Local models can do everything, including Ask |
| Model downloads | Fetched from a moving ref (`resolve/main`); never re-verified after install | Signed manifest pinning exact revision, size and SHA-256; verified before install; re-verifiable. Trusts a **development** signing key only, for now |
| Encryption at rest | None locally | SQLCipher (AES-256) store, key in the OS keychain |
| What leaves the device | Telemetry **on by default** (including an LLM-written free-text workflow phrase); install and uninstall pings even after opt-out; Double Tap through a closed relay by default | No telemetry, no first-party servers. An **egress ledger** records every non-local request with host, feature, bytes and items. Sensitive items are withheld from cloud models |
| Sharing knowledge with other AIs | "MCP mirror" on their server, **password in the URL path**, decrypted server-side | Local-only MCP server (stdio and loopback HTTP), per-client scoped tokens stored as keyed hashes, never in URLs; every call audited. Runs in the daemon |
| Engineering | No unit-test target, no build CI; released by bulk mirror | CI on three OSes, unit and integration tests, recorded connector fixtures, `cargo deny`, a public benchmark harness |

## What Sentient has and Nugget does not (yet)

| Feature | Sentient OS | Nugget |
|---|---|---|
| Drafting in any app (Double Tap) | Ships: right-⌘ double tap, a screenshot plus the whole vault, clipboard paste without delivery check | **Being wired in (wave W).** Engine built and tested: accessibility text first, rule-based field classification, retrieval-sized cited context, verified insertion |
| Computer use (Sidekick) | Ships, built on OpenAI's closed computer-use helper driven through `codex exec`; no step cap, allow-list, confirmations or kill switch | **Being wired in (wave W).** Own agent loop and policy engine built and tested: risk classes, confirmations, budgets, a kill switch and a hash-chained action log. Accessibility-driven tools still in progress |
| Proactive cards and morning brief | Ships: up to 5 prepared actions a day, ranked by LLM order | **Being wired in (wave W).** Deterministic generators with evidence, explainable scoring, actions only after approval and the policy gate |
| Local Apple sources (Notes, Messages, Apple Mail, WhatsApp) | Ships, by reading private databases | **Not built** |
| Slack, Notion, Drive, Outlook | Through hosted MCP connectors | **Being wired in (wave W):** Outlook, Microsoft calendar and OneDrive over Microsoft Graph; Slack, Notion, GitHub and Linear over their own APIs. No Google Drive connector |
| Packaged, signed, auto-updating app | Ships (forced, silent updates every 3 h, one signing key) | **Not built** (M6; planned with user-chosen channels and opt-in updates) |

## Ideas Nugget took from Sentient

The research also recorded things Sentient does well, which Nugget matches: proving a model's vision
support by having it read back a random code from an image; refusing to act in an app whose state was
not read first; never replaying a failed action; a deterministic regex backstop for card and ID numbers
alongside model triage.

## Numbers, and what they are not

Every Nugget number above is from `nugget-bench memory --backend real` on a 4-vCPU cloud VM with a
hashing test embedder, on synthetic and hand-labelled corpora. They show the pipeline is fast and
correct; they say nothing about answer quality with a real model. Nugget does **not** yet have
measurements for drafting latency or quality, computer-use success, time to first value, or battery
cost, so this page makes no claims about them. Sentient publishes no comparable benchmarks, so there
is no head-to-head performance comparison here.
