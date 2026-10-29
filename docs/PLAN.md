# Nugget: build plan

**Nugget** (working name) is an Apache-2.0, local-first, cross-platform personal AI. It learns your work and life
continuously, drafts in your voice, prepares work for your approval, and operates your computer, with every step
inspectable and owned by you.

Grounding: [Sentient OS teardown](research/sentient-os-teardown.md) and [shipped-artifact findings](research/shipped-artifact-findings.md).

Status (2026-10-29):
- **M0 (Foundations): done.** Workspace, CI on three OSes, Tauri shell, `nugget-store`, providers, signed model manifest, bench harness; `nuggetd` runs them.
- **M1 (Memory): done.** Ingest runtime, files, Obsidian, Gmail, Google Calendar, IMAP and CalDAV connectors, hybrid cited retrieval and the local runtime are wired into `nuggetd`.
- **M2 (Knowledge graph): done.** Extraction, the vault and the local MCP server (`nuggetd mcp`, `nuggetd mcp install`) are wired into `nuggetd` and the app.
- **M3 (Double Tap), M4 (Agent and computer use), M5 (Proactive): wired into the daemon and app.** All twelve [wave W](waves/wave-w/README.md) packages are merged, plus a real-app end-to-end test in CI ([e2e.yml](../.github/workflows/e2e.yml)).
- **[Wave X](waves/wave-x/README.md) (release readiness): X1–X4 merged** (safety tuning, knowledge UX, release dry run, reranker and chat citations). **X5 (daemon CPU/RSS/startup at 100k items) is still running** ([swarm.md](swarm.md)).
- **M6 (Hardening and v1.0): mostly built, not released.** Packaging, the release workflow with a dry run that builds unsigned installers on all three OSes, the AppImage smoke test, the opt-in signed updater, i18n, onboarding and the user guide have landed. The Microsoft 365 and SaaS connectors and the WASM plugin host are wired into the daemon and app (W5, W6). No release has been cut; the signing paths are untested without real certificates ([release.md](release.md) §8). The E2E relay (optional) is not started.
- There is no `nugget-privacy` crate: egress accounting lives in `nugget-core` and `nugget-store`, and the PII scan in `nugget-ingest`.

What remains for v1 is mostly [wave Y](waves/wave-y/README.md), which produces the evidence the exit criteria ask for:
- **Y1:** real macOS and Windows runs (installer smoke, accessibility, Double Tap insertion, native `cua` env). Only Linux has run the platform code so far.
- **Y2:** real-model evaluations. Every published draft, cua and knowledge number so far comes from scripted models.
- **Y3:** trust and supply chain: model-manifest rollback protection, a release key, a signed update feed, reproducible builds, X1 agent-safety follow-ups.
- **Y4:** a measured first run (target under 2 min) and empty, loading and error states.
- **Y5:** reproducible benchmarks in the README and nightly dashboards.

Also open, outside wave Y:
- Double Tap p50 against a cloud model (target under 1.2 s) has never been measured; it needs a provider key.
- The [M3–M6 security review](reviews/security-m3-m6.md) still lists 14 findings as Open. Code has regression tests for PA-1, PA-2, PA-3, PA-4, PA-6 and BR-9; PA-5, PA-7 and PM-1 to PM-6 show no fix yet. The table needs reconciling.
- The `memory --backend real` recall gate misses at standard scale by 0.002 (1 query of 500) with the hash embedder; it waits on a re-run with a real embedding model.
- Battery cost per 1k items and idle overhead are not measured yet (`bench/perf` plan).
- The book's [What works today](book/src/status.md) page still describes wave W.

### v1 checklist

| Criterion | Evidence | State |
|---|---|---|
| M0: `nuggetd` runs on 3 OSes | [ci.yml](../.github/workflows/ci.yml) (Rust, bench and desktop jobs on Ubuntu, macOS, Windows) | done |
| M0: chat works with local and remote models | [e2e.rs](../crates/nuggetd/tests/e2e.rs) (OpenAI-compatible mock server); real local models in [Y2](waves/wave-y/Y2.md) | done (real-model run in progress, Y2) |
| M0: CI green | [ci.yml](../.github/workflows/ci.yml), [e2e.yml](../.github/workflows/e2e.yml), [package-smoke.yml](../.github/workflows/package-smoke.yml) | done |
| M1: freshness 60 s or less on the bench | `memory --backend real`: `freshness.p95_s` 0.60 s at standard scale ([bench/README.md](../bench/README.md), Results); [nightly.yml](../.github/workflows/nightly.yml) | done |
| M1: retrieval eval ≥ baseline | `memory --backend real`: eval-set hybrid recall@10 1.000 vs 0.905 text-only; synthetic standard-scale gate 0.936 vs 0.938 ([bench/README.md](../bench/README.md)) | done (synthetic gate missed by 0.002) |
| M2: every vault fact cites sources | `knowledge`: `vault.citation_coverage` = 1, run with `--strict` in [ci.yml](../.github/workflows/ci.yml) | done |
| M2: Claude Desktop and Cursor can query Nugget over MCP | [mcp_clients.rs](../crates/nuggetd/tests/mcp_clients.rs) (clients connected as `nuggetd mcp install` configures them); [MCP guide](book/src/guide/mcp.md) | done |
| M3: Double Tap p50 under 1.2 s (cloud) | `draft`: `ttft.p50_s` with a real cloud provider; only the fake model has run | not started |
| M3: Double Tap p50 under 3 s (local) | `draft --provider-url` with a local model; time to first inserted token in [Y1](waves/wave-y/Y1.md) | in progress (Y1, Y2) |
| M3: drafting eval wins over a screenshot-only baseline | `draft`: `draft.win_rate_vs_baseline` 0.95 with the fake model ([bench/README.md](../bench/README.md)); real model in [Y2](waves/wave-y/Y2.md) | in progress (Y2) |
| M4: published success rate on `bench/cua` | `cua`: `success_rate` 1.00 for the `nugget` arm with a scripted model, inproc ([bench/cua/README.md](../bench/cua/README.md), Reference results); real model and native envs pending | in progress (Y1, Y2) |
| M4: 0 unconfirmed risky actions in red-team tests | `cua`: `safety.unconfirmed_risky_actions` 0 for the `nugget` and `nugget-compliant` arms ([bench/cua/README.md](../bench/cua/README.md)) | done (real-model re-check in Y2) |
| M5: precision of accepted cards tracked | `proactive --backend real`: accepted precision 0.976, precision@5 0.849 ([bench/README.md](../bench/README.md), Results) | done |
| M5: dry-run diff on every action | `proactive --backend real`: dry-run coverage and diff fidelity 1.0, 0 executed without approval | done |
| M6: security review | [security-m3-m6.md](reviews/security-m3-m6.md) (W8); supply-chain gaps in [release.md](release.md) §8 | in progress (Y3) |
| M6: installers for 3 OSes | [release.yml](../.github/workflows/release.yml) dry run builds all targets ([release.md](release.md)); AppImage smoke in [package-smoke.yml](../.github/workflows/package-smoke.yml); dmg and NSIS smoke in Y1 | in progress (Y1, Y3) |
| M6: reproducible benchmarks in the README | [README Benchmarks](../README.md#benchmarks), generated from [latest.json](../bench/results/latest.json) | in progress (Y5) |
| Target: freshness about 60 s; edits and deletions propagate | `memory --backend real`: `freshness.p95_s`, `propagation.*_rate` = 1 | done |
| Target: useful search and drafts in under 2 min | First-run scenario in the desktop e2e ([Y4](waves/wave-y/Y4.md)) | in progress (Y4) |
| Target: computer-use injection compliance 0 | `cua`: `safety.injection_compliance_rate` 0, both `nugget` arms | done |
| Target: needless confirmations ≤ 0.10 | `cua`: `safety.false_confirmation_rate` 0.00 after X1 | done |
| Target: no secrets sent to cloud models in drafts | `draft`: `sensitive_prompt_exposure_rate` and `sensitive_leak_rate` 0 (fake model, cloud-marked) | done |
| Target: proactive precision@5 ≥ 0.7, recall ≥ 0.8 | `proactive --backend real`: 0.849 and 0.997 | done |
| Target: RAM, CPU and idle overhead | `perf --backend real` (W12, 100k items); daemon CPU/RSS/startup in X5; battery not measured | in progress (X5) |
| Target: Linux, macOS and Windows | [ci.yml](../.github/workflows/ci.yml); real desktop runs in [Y1](waves/wave-y/Y1.md) | in progress (Y1) |
| Target: nightly eval dashboards | [nightly.yml](../.github/workflows/nightly.yml) (knowledge; memory and proactive with the real backend; cua inproc and xvfb, draft with the fake provider, perf with the real backend; history on the `bench-results` branch); [dashboard](book/src/benchmarks/dashboard.md) | in progress (Y5) |

---

## 1. Decisions so far
| Decision | Choice | Source |
|---|---|---|
| License | **Apache-2.0**, no CLA (DCO sign-off instead) | User |
| Areas to beat Sentient on | **All of them**: real-time memory, open connectors and plugins, reliable computer use, privacy and speed | User |
| v1 scope | **Everything**: memory, Double Tap, proactive cards, computer use, MCP | User |
| Clean room | No AGPL code, prompts or distinctive expression copied. The teardown docs are the only bridge. | Required by Apache-2.0 |
| Platform and stack | **Rust core plus Tauri 2 shell; macOS, Windows and Linux ship together** | User |
| Default AI | **Asked during onboarding, local recommended.** Small local models always do triage and embeddings | User |
| Hosted services | **None at v1.** No first-party servers in the data path; a self-hostable E2E relay comes later | User |
| Team | **One or two humans directing Claude agents.** Milestones run in order, with parallel work done by agents | User |

## 2. What "10x better" means, measured
Every claim below gets a benchmark in `bench/` and a number in the README. If we can't measure it, we don't claim it.

| Axis | Sentient (observed) | Nugget v1 target |
|---|---|---|
| Platforms | macOS 15.4+, Apple Silicon only | macOS (Apple Silicon and Intel), Windows 10/11, Linux (X11 and Wayland) |
| Knowledge freshness | Nightly batch; edits and deletions never re-read | **About 60 s** from change to searchable; edits and deletions propagate |
| Time to first value | "First run takes ~15 min, keep lid up"; hours for the full overnight run | Useful search and drafts in **under 2 min**; backfill continues in the background, resumable |
| Retrieval | Dumps the whole vault (600 KB cap, silent truncation) | Hybrid BM25, vector and graph search with reranking; **cited** context of 8k tokens or less |
| Knowledge structure | Free-text LLM Markdown, no provenance, no validation | Typed entity graph, with Markdown *rendered* from it; every fact cites its source items |
| Double Tap latency (p50, cloud) | ~2 s claimed; screenshot plus whole vault | **Under 1.2 s to first inserted token**: accessibility text first, retrieval-sized prompt, prewarmed connection |
| Double Tap latency (local) | Not a primary path | Under 3 s on a 16 GB machine with a 4B-class model |
| Computer use | OpenAI's closed helper; no step cap, no confirmations, no kill switch, outcome unverified | Our own driver on every OS; policy engine; **verified outcomes**; published task success rate on `bench/cua` |
| Connectors | Vendor-hosted MCP through a ChatGPT or Claude plan; reverse-engineered SQLite | Direct APIs with our own OAuth (Gmail, Graph, CalDAV, IMAP, Slack, Notion), a WASM plugin SDK, and MCP in and out |
| Privacy | Telemetry on by default, a closed relay on by default, nothing encrypted at rest | Telemetry **off** by default; **no** first-party cloud needed; SQLCipher at rest; an **egress ledger** of every byte sent |
| Updates | Forced, silent, one signing key | User-chosen channel, opt-in auto-update, signing in CI with threshold keys |
| Engineering | No CI, no unit tests, released by bulk mirror | Built in public; CI on all three OSes; unit and integration tests; nightly eval dashboards |

## 3. Architecture

```
┌──────────────────────────── Tauri 2 shell (TS/React) ───────────────────────────┐
│  tray/menu bar · command bar · Double Tap overlay · cards inbox · knowledge UI   │
│  graph view · settings · permissions wizard · egress ledger · action audit log   │
└──────────────────────────────────────┬────────────────────────────────────────────┘
                                       │ typed IPC (tauri commands + event stream)
┌──────────────────────────── nuggetd (Rust core daemon) ───────────────────────────┐
│ ingest ─▶ store ─▶ knowledge ─▶ retrieve ─▶ { draft, proactive, agent } ─▶ policy │
│    ▲        │                                        │                     │      │
│ connectors  └── mcp-server (local) ◀── other AIs     └── infer (providers) │      │
│ (native + WASM plugins + MCP clients)                       a11y drivers ◀─┘      │
└────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.1 Crates (Cargo workspace)
| Crate | Responsibility | Key choices |
|---|---|---|
| `nugget-store` | Items, chunks, entities, relations, provenance, cursors, cards, audit log, egress ledger | SQLite with **SQLCipher**, FTS5 and `sqlite-vec`; key wrapped by the OS keychain; WAL; migrations |
| `nugget-ingest` | Connector runtime, change detection, scheduler | Event-driven: FSEvents, inotify/fanotify, ReadDirectoryChangesW, and polling `data_version` on SQLite sources. Typed per-source cursors plus content hashes, so edits and deletes propagate. Work runs in idle or charging slices, with **no root daemon** |
| `nugget-connectors-*` | First-party sources | See §4 |
| `nugget-plugin` | Third-party connectors and tools | **WASM components** (wasmtime, WIT interface) with declared capabilities (network hosts, scopes), plus MCP-client adapters |
| `nugget-infer` | One `Provider` trait covering chat, tools, vision, streaming, JSON schema and embeddings | Local: **llama.cpp** (GGUF; Metal, CUDA, Vulkan, CPU), with MLX optional on Mac. Remote: native **Anthropic Messages** and **OpenAI Responses**, any OpenAI-compatible server, Ollama, LM Studio. A **capability probe** (vision, tool calls, JSON schema) runs before a model can be selected. Routing is by **capability tier** (triage, light, synthesis, agentic), never by hard-coded model ids. Quantization is picked from detected RAM and VRAM |
| `nugget-models` | Model manifest and download | Signed manifest: model, **pinned revision**, size, hash, minimum app version. Content-addressed storage, lazy re-verification, two slots with rollback, and a fail-closed disk check |
| `nugget-knowledge` | Understanding pipeline | Triage, then chunk, then extract typed entities and relations (person, org, project, commitment, preference, event), with alias and contact resolution. The **Markdown vault is rendered** from the graph: stable ids, YAML frontmatter, citations. User edits are three-way merged back. Validators check that links resolve and the schema holds |
| `nugget-retrieve` | Query-time context | Hybrid BM25 and vector search, graph neighbourhood expansion, cross-encoder rerank, and token-budgeted packing with citations |
| `nugget-draft` | Double Tap | Global hotkey (configurable, works without a notch or Apple keyboard). Context comes from the **accessibility tree first** (thread and field text, app, recipient), with a screenshot only as a fallback. Retrieval-sized prompt, prompt caching, prewarmed HTTP/2. Insertion through accessibility insert-at-selection, verified by reading the field back, with a clipboard fallback that preserves every pasteboard type |
| `nugget-proactive` | Morning brief and cards | Deterministic candidate store (commitments, unanswered threads, renewals, meetings) with dedup and due dates, and a feedback-learned score. An LLM judge enriches candidates. **Typed actions with dry-run diffs**, executed only on approval |
| `nugget-agent` | Our own agent loop | Tool registry, plan/act/verify loop, **step and time budgets**, idempotent action ids, structured **verify-after-act** (accessibility state diff, or an API check such as "message exists in Sent") |
| `nugget-a11y` | Cross-platform computer-use driver | One trait over **macOS AX plus CGEvent**, **Windows UIA**, and **Linux AT-SPI plus libei/portals**. Element-id snapshots, background-safe actions where the OS allows, and an overlay "agent cursor" that never moves the user's pointer. Browsers: a companion extension or WebDriver BiDi on an opt-in profile, falling back to accessibility |
| `nugget-policy` | Safety | Per-app allow/deny lists, a risk classifier for actions (send, pay, delete, share, install) with **inline confirmation**, a global **kill-switch hotkey**, and a tamper-evident audit log with before/after evidence |
| `nugget-privacy` | Trust | Per-source consent and egress scope. Redaction (NER plus pattern packs) before any cloud call. The **egress ledger** records which bytes went to which host, for which feature, and when |
| `nugget-mcp` | Share knowledge with other AIs | A **local** MCP server (stdio and localhost HTTP) with per-client scoped tokens and revocation. Optional remote access through an **end-to-end-encrypted, self-hostable** relay. No secrets in URLs |
| `nuggetd` | Daemon | Runs the pipeline and IPC; the OS service manager starts it as a user-level service (launchd agent, Task Scheduler, systemd user unit) |

### 3.2 App shell
Tauri 2 with TypeScript and React for the tray or menu-bar app, plus overlay windows (command bar, Double Tap indicator, agent cursor). The Mac
notch integration is a nice-to-have skin, **not** a requirement.

### 3.3 Data flow
1. **Change detected.** A connector emits `ItemChanged`, `ItemDeleted` or `ItemUpserted` with a content hash.
2. **Triage.** The local small model decides keep, junk or sensitive and writes a summary. Deterministic PII scanning follows.
3. **Chunk and embed.** Done locally. The result is searchable within 60 s of the change.
4. **Extract.** Entities and relations are pulled out, locally or by a frontier model depending on per-source policy, and merged into the graph with provenance.
5. **Render.** Only the vault pages for affected entities are re-rendered, with a three-way merge against user edits.
6. **Consume.** Proactive candidates, retrieval for drafts and the agent, and MCP exposure all read from here.

## 4. Connectors in v1
**Local and native:**
- Files (all OSes, with OCR and real chunking)
- Obsidian, Logseq, Markdown folders
- Apple Notes, iMessage, Apple Mail, Calendar, Contacts (macOS)
- WhatsApp Desktop and Signal Desktop (where readable)
- Thunderbird and Maildir
- Outlook (Windows, via Graph)

**Direct APIs with our own OAuth:**
- Gmail (`historyId` deltas)
- Google Calendar and Drive
- Microsoft Graph (mail, calendar, OneDrive delta links)
- IMAP/SMTP and CalDAV/CardDAV
- Slack, Notion, GitHub, Linear

**Extension points:**
- Any MCP server as a source
- WASM plugins from a community registry

## 5. Delivery (v1 = everything, built in milestones)
v1.0 ships all pillars. Milestones are internal checkpoints, each ending in a usable nightly build and green benchmarks.

| Milestone | Contents | Exit criteria |
|---|---|---|
| **M0: Foundations** | Repo, CI (macOS, Windows, Linux), Cargo workspace, Tauri shell, `nugget-store`, provider trait with 3 backends, model manifest, bench harness skeleton | `nuggetd` runs on 3 OSes; chat works with local and remote models; CI green |
| **M1: Memory** | Ingest runtime, files, Obsidian, Gmail, GCal, IMAP connectors, triage, chunking, embedding, hybrid search, a basic knowledge UI | Freshness 60 s or less on the bench; retrieval eval ≥ baseline |
| **M2: Knowledge graph** | Entity and relation extraction, provenance, vault renderer with three-way merge, graph view, local MCP server | Every vault fact cites sources; Claude Desktop and Cursor can query Nugget over MCP |
| **M3: Double Tap** | Hotkey, accessibility context, retrieval-sized prompts, verified insertion, voice profile | p50 under 1.2 s cloud and under 3 s local; drafting eval wins over a screenshot-only baseline |
| **M4: Agent and computer use** | `nugget-a11y` (Mac, Windows, Linux), agent loop, policy engine, kill switch, audit log, browser extension | Published success rate on `bench/cua`; 0 unconfirmed risky actions in red-team tests |
| **M5: Proactive** | Candidate store, judge, typed actions, cards inbox, morning brief | Precision of accepted cards tracked; dry-run diff on every action |
| **M6: Hardening and v1.0** | Remaining native connectors, WASM plugin SDK, E2E relay (optional), signed updates, i18n, docs, onboarding | Security review; installers for 3 OSes; reproducible benchmarks in the README |

## 6. Evaluation (`bench/`)
- **`bench/cua`.** Scripted computer-use tasks on all three OSes in VMs (form fill, email triage, file organising, calendar booking, web checkout up to the confirmation step). Metrics: verified success, steps, wall time and safety violations. Where licences allow, include an OSWorld-style subset.
- **`bench/draft`.** Reply drafting against held-out real threads (with consent) or synthetic personas. Scored by pairwise LLM judging plus human spot checks on voice match, factuality and latency.
- **`bench/memory`.** Freshness (change to searchable), retrieval recall@k, citation accuracy, and edit and delete propagation.
- **`bench/perf`.** RAM, CPU and battery cost per 1k items, and idle overhead.

## 7. Engineering practices
- Built in public on GitHub. DCO instead of a CLA. Conventional commits. `cargo fmt`, `clippy -D warnings`, `cargo deny`.
- Unit tests per crate, integration tests with recorded fixtures for every connector, and UI tests through Playwright on the Tauri webview.
- Security: threat model doc, `cargo audit`, fuzzing for parsers (iMessage typedstream, Notes protobuf, MIME), and per-plugin capability sandboxing.
- Telemetry: opt-in OpenTelemetry only, with no free text, a local preview of every payload, and the option to self-host.

## 8. Risks
| Risk | Mitigation |
|---|---|
| Scope ("everything in v1") is very large | Milestones ship nightlies; v1.0 gates on benchmarks, not dates; cut connectors before cutting quality |
| Cross-platform accessibility APIs differ widely, especially on Wayland | One trait plus per-OS capability flags; fall back to screenshots and coordinates where accessibility is missing |
| Private Apple database formats change | Schema-backed parsers, fuzzing, fixture tests per OS version, graceful degradation with clear UI |
| Local models are too weak for agentic work | Tiered routing; local by default for triage, embedding and drafting; BYO frontier key for the agent; publish per-tier numbers |
| OAuth app verification (Google restricted scopes) | Start the verification process early; allow users to bring their own OAuth client in the meantime |

## 9. Open questions
1. **Name and brand.** Keep "Nugget"?
2. **Business model.** Apache core plus a paid hosted team tier later (shared knowledge, admin policy, SSO)?

Resolved: platform (all three OSes), default AI (ask, local recommended), hosted services (none at v1), team (humans plus agents).
