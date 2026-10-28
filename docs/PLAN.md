# Nugget: build plan

**Nugget** (working name) is an Apache-2.0, local-first, cross-platform personal AI. It learns your work and life
continuously, drafts in your voice, prepares work for your approval, and operates your computer, with every step
inspectable and owned by you.

Grounding: [Sentient OS teardown](research/sentient-os-teardown.md) and [shipped-artifact findings](research/shipped-artifact-findings.md).

Status: **draft, pending the decisions in §9.**

---

## 1. Decisions so far
| Decision | Choice | Source |
|---|---|---|
| License | **Apache-2.0**, no CLA (DCO sign-off instead) | User |
| Areas to beat Sentient on | **All of them**: real-time memory, open connectors and plugins, reliable computer use, privacy and speed | User |
| v1 scope | **Everything**: memory, Double Tap, proactive cards, computer use, MCP | User |
| Clean room | No AGPL code, prompts or distinctive expression copied. The teardown docs are the only bridge. | Required by Apache-2.0 |
| Platform and stack | **Rust core plus Tauri 2 shell** (recommended; pending, see §9) | — |

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

## 9. Open questions (need answers before M0)
1. **Platform and stack.** Confirm Rust core plus Tauri 2, shipping Mac, Windows and Linux together, or choose Mac-first.
2. **Default model setup.** Fully local by default (slower, private), or ask during onboarding, with local recommended?
3. **Hosted services.** Should we run *any* first-party cloud (an optional free drafting relay, the E2E sync relay), or ship purely BYO keys at v1?
4. **Team and timeline.** How many engineers, and is there a target date? This decides how milestones run in parallel.
5. **Name and brand.** Keep "Nugget"?
6. **Business model.** Apache core plus a paid hosted team tier (shared knowledge, admin policy, SSO)?
