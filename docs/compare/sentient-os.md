# Nugget and Sentient OS: a sourced comparison

Checked: **October 2026** (Nugget at wave W; Sentient OS v2.7 source and v2.7.1 release).

Nugget started as an answer to [Sentient OS](https://github.com/Sentient-OS-Labs/sentient-os), an
AGPL-3.0, macOS-only personal AI. This page compares the two area by area. It tries to be fair: where
Sentient is ahead, it says so, and where a Nugget feature is still being wired in, it says that too.

## How we checked

- **Everything about Sentient OS** comes from Nugget's two clean-room research notes, and each claim
  below names the section it comes from:
  - [`docs/research/sentient-os-teardown.md`](../research/sentient-os-teardown.md) ("teardown"): a
    reading of the v2.7 source, labelled O (observed), I (inferred) and U (unknown). File references
    in it are relative to `Sentient OS macOS/` in the upstream repository.
  - [`docs/research/shipped-artifact-findings.md`](../research/shipped-artifact-findings.md)
    ("artifact"): a read-only inspection of the shipped `SentientOS-2.7.1.dmg` (bundle contents,
    `strings`, plists). Nothing from it was executed.

  Only claims labelled observed are used as facts here; inferences are marked as such. Later Sentient
  versions may differ.
- **Everything about Nugget** points at the code or the book, with paths in this repository. Features
  that exist as tested crates but are not yet running in the daemon are labelled **being wired in
  (wave W)**; see [`docs/waves/wave-w/README.md`](../waves/wave-w/README.md).
- **Clean room.** This page describes behaviour in our own words. It quotes no Sentient code or
  prompts. See [the clean-room rules](../../CONTRIBUTING.md#clean-room).
- No head-to-head benchmark exists: Sentient publishes none, and Nugget has not run one against it.

## Summary

| Area | Sentient OS (observed) | Nugget |
|---|---|---|
| Platforms | macOS 15.4+, Apple Silicon only (teardown §1, §2.1) | macOS, Windows, Linux in CI; built from source, **no installers yet** |
| Licence | AGPL-3.0, CLA bot (teardown header, §2.9) | Apache-2.0, DCO, no CLA ([`LICENSE`](../../LICENSE), [`CONTRIBUTING.md`](../../CONTRIBUTING.md)) |
| Data at rest | Not encrypted locally (teardown §2.5) | SQLCipher store, key in the OS keychain |
| What leaves the device | Telemetry on by default; Double Tap via a closed relay by default (teardown §2.7, §2.9) | No telemetry; every off-device request in an egress ledger |
| Memory | LLM-written Markdown, no provenance or search index (teardown §2.5) | Hybrid search plus a typed graph where every claim has an evidence quote |
| Sharing with other AI apps | Hosted "MCP mirror" with a password in the URL (artifact, "Security and trust notes") | Local MCP server, per-client scoped tokens |
| Drafting anywhere (Double Tap) | **Ships** (teardown §2.7) | Being wired in (wave W) |
| Computer use | **Ships**, on OpenAI's helper via `codex exec` (teardown §2.6; artifact) | Own agent and policy engine, being wired in (wave W) |
| Proactive cards | **Ships** (teardown §2.8) | Being wired in (wave W) |
| Packaged, auto-updating app | **Ships** (artifact) | Not yet (M6) |

## Privacy architecture

**Sentient OS.** Per-item triage runs on an on-device model, with a regex backstop for SSNs, card
numbers and passport numbers (teardown §2.1). The knowledge base, proactive cards and computer use
then go to a frontier cloud model through a bundled CLI (teardown §1, §2.2). Gmail, Calendar, Slack,
Outlook and Drive are read and summarised by that cloud CLI through vendor-hosted MCP connectors
(teardown §2.4; artifact, "Network endpoints"). Nothing is encrypted at rest locally (teardown §2.5).
Telemetry is on by default, with an always-on tier that sends an LLM-written free-text workflow
phrase, and install and uninstall pings even after opt-out (teardown §2.9). Proactive cards are kept
as JSON in UserDefaults (teardown §2.8). Updates are mandatory and silent every 3 hours (teardown §2.9;
artifact, bundle table).

**Nugget.**

- **Local-first.** One per-user daemon, no root, no first-party servers
  ([`crates/nuggetd/src/lib.rs`](../../crates/nuggetd/src/lib.rs),
  [security model](../book/src/architecture/security.md#egress-accounting)). Local models come from a
  signed, pinned manifest and run in a supervised `llama-server` on loopback
  ([`crates/nugget-models/src/lib.rs`](../../crates/nugget-models/src/lib.rs),
  [`crates/nugget-runtime/src/lib.rs`](../../crates/nugget-runtime/src/lib.rs)). Cloud providers are
  optional and use your own key ([`crates/nugget-infer/src/lib.rs`](../../crates/nugget-infer/src/lib.rs)).
- **Encrypted store.** One SQLCipher database holds items, chunks, the FTS5 and `sqlite-vec` indexes,
  the egress ledger, a hash-chained audit log and the graph; the key is a random 256-bit key in the OS
  keychain, or a `0600` key file when no keychain is usable
  ([`crates/nugget-store/src/lib.rs`](../../crates/nugget-store/src/lib.rs)).
- **Egress ledger.** Every request to a non-local destination records host, feature, bytes and the
  items whose content was included, from both model providers and connectors
  (`EgressSink` in [`crates/nugget-infer/src/http.rs`](../../crates/nugget-infer/src/http.rs);
  `MeteredHttp` in the connectors). The desktop Privacy page shows it
  ([privacy guide](../book/src/guide/privacy.md#the-egress-ledger)).
- **Sensitivity gating.** A deterministic scan marks items with card numbers, SSNs, IBANs or secrets
  as sensitive at ingest. Sensitive items never go to a cloud model for triage, embedding, extraction
  or Ask, and MCP clients do not see them unless granted `include_sensitive`
  ([`crates/nugget-retrieve/src/lib.rs`](../../crates/nugget-retrieve/src/lib.rs),
  [`crates/nugget-core/src/ipc.rs`](../../crates/nugget-core/src/ipc.rs) `McpScopes`,
  [privacy guide](../book/src/guide/privacy.md#sensitive-items)).
- **No telemetry** ([security model](../book/src/architecture/security.md#egress-accounting)).

**Honest gaps.** The ledger is written when an exchange ends, so a crash mid-request can leave no row,
and a local server that forwards to the cloud looks on-device unless you mark it (same section). The
service install and keychain backends are not yet verified on real macOS and Windows machines outside
CI ([install guide](../book/src/guide/install.md)).

## Memory with citations

**Sentient OS.** A frontier model writes 80–120 free-form Markdown notes; the structure lives only in
the prompt and nothing validates it. There is no entity store, no provenance (summaries are deleted
after each cycle), no embeddings, no full-text index and no graph database. Links resolve by file-name
stem, so notes with the same name collide (teardown §2.5). Double Tap sends the whole vault, up to
600 KB, silently truncated (teardown §2.5, §2.7). Files and Notes are keyed on date added or created,
so edits are never re-read (teardown §2.4). Indexing is a nightly batch, one item at a time, driven by
a root LaunchDaemon that wakes the Mac (teardown §2.3; artifact, bundle table).

**Nugget.**

- **Continuous ingest** with versioned items, so edits, deletes and renames propagate; a reconciling
  sync catches deletes made while Nugget was off
  ([`crates/nugget-ingest/src/lib.rs`](../../crates/nugget-ingest/src/lib.rs),
  [data flow](../book/src/architecture/data-flow.md)).
- **Hybrid retrieval** (BM25 plus vectors, rank fusion, MMR), packed under an explicit token budget
  with `[n]` citations ([`crates/nugget-retrieve/src/lib.rs`](../../crates/nugget-retrieve/src/lib.rs)).
  Measured numbers, and the gate it still misses, are in the
  [benchmarks page](../book/src/architecture/benchmarks.md).
- **Knowledge graph, running in the daemon.** Deterministic extractors always run; an optional LLM
  extractor (local providers by default) must give, for every claim, a quote found verbatim in the
  source. Every claim carries evidence `{item, chunk, quote}`; facts you reject stay rejected
  ([`crates/nugget-knowledge/src/lib.rs`](../../crates/nugget-knowledge/src/lib.rs),
  [`crates/nuggetd/src/knowledge.rs`](../../crates/nuggetd/src/knowledge.rs)).
- **Vault.** The graph is rendered as Markdown pages with stable-id links; your edits are merged back
  three-way into claims ([`crates/nugget-vault/src/lib.rs`](../../crates/nugget-vault/src/lib.rs)).

**Where Sentient is ahead.** It reads more local Apple sources today: Messages, WhatsApp, Apple Notes
and Apple Mail, by parsing their private databases (teardown §2.4). Nugget has no connectors for them.
Its notes are written by a strong cloud model, which may read better than Nugget's rendered pages;
nobody has measured that.

## MCP: sharing knowledge with other AI apps

**Sentient OS.** An "MCP mirror" of your knowledge on Sentient's server, reached at a URL that carries
a user id and a password in the path (artifact, "Network endpoints" and "Security and trust notes",
citing `MirrorClient.swift:10`). The vault zip is encrypted, but the key comes from that password and
the server decrypts on every request (teardown §2.5). The server source is not public (artifact, "What
is not in the public repo").

**Nugget.** A local MCP server in the daemon, read-only tools and resources only
([`crates/nugget-mcp/src/lib.rs`](../../crates/nugget-mcp/src/lib.rs),
[`crates/nuggetd/src/mcp.rs`](../../crates/nuggetd/src/mcp.rs)):

- stdio through `nuggetd mcp --client <id>` and streamable HTTP bound to 127.0.0.1; no remote transport;
- one token per client (`ngt_…`), stored only as a keyed hash, sent in an environment variable or a
  bearer header and never in a URL; revocable, with `last_used`;
- scopes per client: a sources allow-list, a tools allow-list and `include_sensitive`, applied down to
  individual evidence and passages;
- Host and Origin checks, per-client rate limits, an audit entry per call (no content);
- source content returned inside `<untrusted-content>` blocks.

Clients are managed with `mcp.clients.create`, `mcp.clients.list` and `mcp.clients.revoke`, from the
desktop app's Connected AI apps page or `nuggetd mcp install`
([MCP guide](../book/src/guide/mcp.md)).

## Double Tap (drafting in any app)

**Sentient OS (ships).** Right Cmd twice; one screenshot of the display (up to 2000 px) plus the whole
vault and writing samples go in one vision call, by default through a closed `workers.dev` relay whose
model and retention are unknown. The focused field is not read through accessibility. The draft is
pasted with the clipboard and a synthetic Cmd-V; non-text clipboard content is lost and delivery is not
verified (teardown §2.7). Writing style is raw samples, not a model (teardown §2.4).

**Nugget (being wired in, wave W).** The crates exist and are tested; the daemon methods
(`draft.begin`, `draft.cancel`, `draft.feedback`, `voice.status`) and the overlay are being wired in
now ([`docs/waves/wave-w/W1.md`](../waves/wave-w/W1.md)). Design, from the code:

- read the focused field and thread through accessibility (AX, UI Automation, AT-SPI) and insert at the
  caret with read-back verification ([`crates/nugget-a11y/src/lib.rs`](../../crates/nugget-a11y/src/lib.rs));
- field kind decided by rules, not the model; secure fields and excluded apps refused first;
- retrieval-sized context (1,500 tokens by default) with sensitive items withheld from cloud providers
  and cited items in the egress ledger; no whole-vault dump
  ([`crates/nugget-draft/src/lib.rs`](../../crates/nugget-draft/src/lib.rs));
- a style profile and exemplars per channel and recipient
  ([`crates/nugget-voice/src/lib.rs`](../../crates/nugget-voice/src/lib.rs)).

No drafting latency or quality numbers exist yet.

## Computer use and its safety

**Sentient OS (ships).** Every backend runs the Codex CLI with approvals and sandbox bypassed, driving
OpenAI's closed computer-use helper (teardown §2.6). The helper and CLI are pinned downloads of about
170 MB or more (artifact, "The computer-use stack is mostly OpenAI's"). Missing safeguards, as observed:
no step cap or deadline, no app allow or deny list, no confirmation for irreversible actions, no global
kill switch (the hotkey does not stop a task), and "done" means the agent said so (teardown §2.6). There
is no action log (teardown §3, point 5; users ask for one, §2.9). Good ideas it has: mutations
refused unless the app's state was read first, failed calls never replayed (teardown §2.6).

**Nugget (being wired in, wave W).** Its own agent loop and policy engine, tested as crates; the daemon
methods and the Agent and Safety pages are being wired in now
([`docs/waves/wave-w/W3.md`](../waves/wave-w/W3.md)):

- **Policy engine** ([`crates/nugget-policy/src/lib.rs`](../../crates/nugget-policy/src/lib.rs)): six
  risk classes (Observe, Edit, Send, Destroy, Spend, Security); rules per app and site where deny always
  wins; a deterministic classifier that can only raise the declared risk; Send and above need
  confirmation unless a rule says otherwise, and an allow rule must name those classes explicitly; rate
  limits per risk class.
- **Confirmations**: a timeout or the kill switch counts as deny.
- **Kill switch**: a process-wide halt checked before every step and action, designed to hang off the
  global hotkey.
- **Action log**: append-only, SHA-256 hash-chained, exportable as JSONL.
- **Agent** ([`crates/nugget-agent/src/lib.rs`](../../crates/nugget-agent/src/lib.rs)): budgets on
  steps, wall clock, tokens and per-tool rate; mutations verified after acting, and success reported
  only when every mutation verified; untrusted tool output fenced as data.
- **Browser** through an extension and a native-messaging host, per-site consent, no localhost ports
  ([`crates/nugget-browser/src/lib.rs`](../../crates/nugget-browser/src/lib.rs),
  [`extensions/browser/README.md`](../../extensions/browser/README.md)).

**Where Sentient is ahead.** It ships a working computer-use agent today, on a mature third-party
runtime. Nugget's accessibility-driven computer-use tools are still being built (wave W2), and Nugget
has no computer-use success numbers yet.

## Proactive cards

**Sentient OS (ships).** A daily judge reads 7 days of summaries plus calendar; research turns up to 8
items into at most 5 prepared actions. Ranking is the model's ordering (teardown §2.8).

**Nugget (being wired in, wave W).** Deterministic generators with evidence, an explainable scoring
model that learns from accept, dismiss and snooze, an optional judge that can only nudge the rank, and
typed actions that run only after approval of a previewed diff and the policy gate
([`crates/nugget-proactive/src/lib.rs`](../../crates/nugget-proactive/src/lib.rs),
[`docs/waves/wave-w/W4.md`](../waves/wave-w/W4.md)).

## Connectors

**Sentient OS.** Local Apple sources through private formats; Gmail, Calendar, Drive, Slack and Outlook
through vendor-hosted MCP servers read by the cloud CLI, which needs a ChatGPT or Claude plan for Gmail
and Calendar; Notion and Granola with their own OAuth (teardown §2.4; artifact, "Network endpoints").

**Nugget.** In the daemon today: folders, Obsidian, Gmail and Google Calendar with your own OAuth
client (read-only scopes), IMAP and CalDAV for any provider; incremental, deterministic, no AI plan
needed ([connector guides](../book/src/guide/sources.md)). Being wired in (wave W): Outlook mail,
calendar and OneDrive over Microsoft Graph with your own app registration
([`crates/nugget-connector-microsoft/src/lib.rs`](../../crates/nugget-connector-microsoft/src/lib.rs)),
and Slack, Notion, GitHub and Linear over their official APIs
([`crates/nugget-connector-saas/src/lib.rs`](../../crates/nugget-connector-saas/src/lib.rs),
[`docs/waves/wave-w/W5.md`](../waves/wave-w/W5.md)).

**Trade-off.** Nugget's Google setup needs a one-time Google Cloud project and, in Testing mode, a
weekly re-sign-in ([Google guide](../book/src/guide/sources-google.md)). Sentient's hosted connectors
are easier to start with.

## Plugins

**Sentient OS.** The research describes no third-party plugin system. New sources arrive as MCP
connectors built into the app (teardown §2.4); community contributions such as the SiYuan source go
into the app itself (teardown §2.4, table).

**Nugget (being wired in, wave W).** WebAssembly components with capabilities declared in
`nugget-plugin.toml`, approved at install and enforced on every host call (deny by default; no
filesystem, sockets or environment); fuel, deadline and memory limits; packages signed with ed25519
and verified against trusted keys ([`crates/nugget-plugin/src/lib.rs`](../../crates/nugget-plugin/src/lib.rs),
[`crates/nugget-plugin/src/package.rs`](../../crates/nugget-plugin/src/package.rs),
[`docs/waves/wave-w/W6.md`](../waves/wave-w/W6.md)).

## Licence and project

| | Sentient OS | Nugget |
|---|---|---|
| Licence | AGPL-3.0 (teardown header) | Apache-2.0 |
| Contributions | CLA bot; 1 external PR merged in visible history (teardown §2.9) | DCO sign-off, no CLA |
| Development | Private tree mirrored to GitHub per release (teardown §1) | In the open |
| Tests and CI | No unit-test target, no build CI (teardown §2.9) | CI on three OSes, `cargo deny`, benchmark harness ([`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)) |
| Closed parts | Double Tap relay, MCP mirror server (artifact, "What is not in the public repo") | None; no first-party servers |

AGPL-3.0 requires network-service operators to publish modifications; Apache-2.0 does not, and grants
an explicit patent licence. Which suits you depends on how you plan to use the code.

## Where Sentient is ahead, in one list

- It is a packaged, signed macOS app with onboarding; Nugget is built from source.
- Double Tap, computer use and proactive cards ship; in Nugget they are being wired in.
- More local Apple sources (Messages, WhatsApp, Notes, Apple Mail) and hosted connectors.
- A frontier model writes the knowledge base, which may read well; no one has compared quality.

## Ideas Nugget took from Sentient

From the research (teardown §2.1, §2.2, §2.6), described in our own words: prove a model's vision
support by having it read back a random code from an image; refuse to act in an app whose state was
not read first; never replay a failed action; keep a deterministic regex backstop next to model triage.
