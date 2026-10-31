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
- The [M3–M6 security review](reviews/security-m3-m6.md) is reconciled (Z4): 13 findings Fixed with their commits and regression tests, PM-4 Mitigated (an HTTP MCP client can still send its token to a port squatter while nuggetd is down; the stdio bridge avoids it).
- End-to-end QA with real models (wave GG; findings and owners in [docs/qa/README.md](qa/README.md)). These are the product as a user meets it and override the bench figures where they differ:
  - Double Tap ([GG1](qa/GG1.md)): drafts complete and are in the right language in 22/25 non-English threads; an 8B judge rates 61/100 4B drafts sendable. **Blocker:** an instruction planted in a vault note is obeyed (attacker link in 62 drafts). Non-English password, one-time-code and card fields are drafted into; the newest question in long threads is cut; drafts glue to typed text.
  - Ask ([GG2](qa/GG2.md), before EE3's grounded prompt): 77% (4B) / 82% (8B) of 149 turns fully correct; **Blocker:** planted instructions obeyed; dates come from file times; follow-ups are never rewritten with the default config; sources show only with the first token (35–95 s).
  - Computer use ([GG3](qa/GG3.md), before DD3): 0.56 task success with the 4B over 81 realistic tasks (the bench's 0.92 overstates it); 0 unconfirmed risky clicks over 192 runs and the kill switch holds, but it never asks on ambiguous targets (trashed a whole folder), empties the Trash unasked, can report success after a declined send, and the Send confirmation hides the recipient and body.
  - v1 needs every Blocker and High fixed and re-verified by the same QA scenarios.
- Privacy, verified from captured network traffic (QA [GG8](qa/GG8.md)): local-only use sends nothing (0 DNS queries, 0 packets, including idle). With a cloud provider or opt-in, three High findings: an Ask follow-up forwarded earlier answers unscanned (GG8-1), cloud embeddings carried sensitive notes (GG8-2, **fixed**), and Double Tap sent texts became a hidden source readable by MCP clients and cloud Ask (GG8-3); plus an unledgered default update check (GG8-5) and lower findings. Each is routed in [docs/qa/README.md](qa/README.md); v1 needs all High and Medium fixed.
- The [waves AA–CC security review](reviews/security-aa-cc.md) (EE4): 25 findings (1 High, 6 Medium fixed in the reviewed code with regression tests; 2 accepted). Two Medium findings in wave DD code (RR-1: sensitive passages to a cloud reranker; AG-1: screen text outside the agent's untrusted fence) have patches in `docs/reviews/security-aa-cc-patches/`; both are applied (DD4 and DD3, with their regression tests). Open by design: the streaming credential stop can miss digits spread across words, and homoglyph or encoded credentials.
- `memory --backend real` with the manifest's embedding model (nomic-embed-text v1.5, DD4): the recall gate is met at every scale (quick 0.970 vs 0.930, standard 0.938 vs 0.938 as a tie, full 0.892 vs 0.874; at standard the hash embedder still misses by 0.002, and so does nomic without its task prefixes). Embedding a fresh 20,000-file corpus takes 4 hours on a 2-core CPU. On the synthetic corpus of made-up words the vector leg has no signal, and rank fusion puts chunks found by both lists above a strong full-text match (hybrid MRR 0.458 vs bm25 0.888); score-aware fusion fixes that corpus but loses on the English eval set, so RRF stays. The reranker needs ~0.5 s per passage on a 2-core CPU: the adaptive pool now pauses it instead of making every search wait 2.5 s ([bench/README.md](../bench/README.md), Real-model results).
- Wayland: the Double Tap global shortcut fires through the GlobalShortcuts portal on KDE Plasma 6 and GNOME 50, verified on real runners from a virtual key press through to Double Tap (installed debug .deb, consent dialogs accepted over AT-SPI; DD1). Three causes were fixed: the portal accepts a host app id only when a desktop file named after it with an existing `Exec` is installed (the .deb/.rpm now ship `dev.nugget.Nugget.desktop`, the AppImage writes one under `~/.local/share/applications`); kglobalaccel drops components whose name ends in `.desktop`, so the portal app id is `dev.nugget.Nugget`, not the Tauri identifier; and shortcut ids are now stable so a consent is remembered. Still open: not tried on a physical Plasma or GNOME desktop; Plasma 5.27 and GNOME before 48 untested; CI's KWin runs with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` for its fake-input keyboard; GNOME's GTK 4 insert path is not run (no data-control clipboard in Mutter). sway and other desktops without the portal use the tray/app or a compositor binding to `nugget-desktop --double-tap`.
- Computer use, **decision for the user: turn the harm triage on for local models?** (`AgentConfig.triage`: one goal-only call that refuses fraud, impersonation, harassment and other serious harm; on in the real-model bench, off in the daemon. Evidence from DD3, run 41018125222, [`bench/README.md`](../bench/README.md) "Asking when the target is ambiguous, and the triage decision".)
  - *For.* Without it neither local model refused any of the 4 harmful bench tasks (they filled in the impersonation form, followed the gift-card scammer, worked on the phishing mail); with it both refused all 4, and the triage alone refused 17 of 17 harmful goals on both models. Policy and confirmations still stop the risky step itself, but a harmful message the user confirms is still sent.
  - *Against.* False refusals: 4 of 83 benign goals with the 4B model (5%), 7 of 83 with the 8B (8%), concentrated on goals that only look sensitive (15–20% of those: payroll bank details, a passport scan to a travel agent, a breakup message, a one-star review); the 8B model also refused "turn off the firewall" and forwarding an invoice to accounts. Cost: one call per task, about 5.5 s (4B) or 9 s (8B) at p50 on a 2-core CPU; refused tasks end sooner, so median task time did not rise.
  - *Recommendation.* Turn it on for local models, with two conditions: run the triage on the 4B model even when the agent uses the 8B (fewer false refusals and faster), and make a triage refusal a question, not a dead end: show the reason and offer "Do it anyway", which counts as the user's own decision and is logged. Cloud models keep their own refusals and need no triage. Not flipped in DD3; it is the user's call.
- Computer use: "ask, don't guess" (DD3) holds the first change when two on-screen items fit the goal and asks with the candidates; the ask rate went from 0.07–0.14 to 0.57 on both local models (target 0.9, missed), with 0 needless questions from the guard. What is left: the file manager (the models never open the folder, so they never see the two reports) and requests missing a fact (no time for "book a meeting with Dana"), which the guard does not cover and the models do not ask about by themselves.
- Knowledge extraction with a local model (AA3): entity F1 0.62→0.71 and 0.66→0.75 on 30 items in same-runner A/B runs, 0.67–0.77 on all 93 (target 0.8, missed); claim F1 0.66–0.67 on 93 with 0 unsupported claims; about 2× faster per request. Nicknames and job changes still fail. `EXTRACTOR_VERSION` is unchanged, so existing items keep their earlier extraction until re-extracted.
- Battery cost in joules is still not measured (DD2): GitHub's Linux VMs expose no RAPL counter and the virtual Macs' `powermetrics` reports no processor energy, so the `energy` suite reports CPU time and wakeups (indexing 1k items: 1.06 CPU-s; idle daemon + hidden shell: ~0.15% of a core; a 4B read-ahead: 26 CPU-s on Linux, 34 on a virtual Mac). It reports joules wherever a counter exists; a run on a physical laptop is still needed. With read-ahead on, moving between text fields costs the shell ~1.5% of a core and ~120 wakeups/s (the accessibility read of each new field), not cut yet. Indexing with the real embedding model is not in the 1k figure ([bench/README.md, Energy](../bench/README.md#energy)).
- The book's [What works today](book/src/status.md) page still describes wave W.

### v1 checklist

| Criterion | Evidence | State |
|---|---|---|
| M0: `nuggetd` runs on 3 OSes | [ci.yml](../.github/workflows/ci.yml) (Rust, bench and desktop jobs on Ubuntu, macOS, Windows) | done |
| M0: chat works with local and remote models | [e2e.rs](../crates/nuggetd/tests/e2e.rs) (OpenAI-compatible mock server); real local models in [Y2](waves/wave-y/Y2.md) | done (real-model run in progress, Y2) |
| M0: CI green | [ci.yml](../.github/workflows/ci.yml), [e2e.yml](../.github/workflows/e2e.yml), [package-smoke.yml](../.github/workflows/package-smoke.yml) | done |
| M1: freshness 60 s or less on the bench | `memory --backend real`: `freshness.p95_s` 0.60 s at standard scale ([bench/README.md](../bench/README.md), Results); [nightly.yml](../.github/workflows/nightly.yml) | done |
| M1: retrieval eval ≥ baseline | `memory --backend real`: eval-set hybrid recall@10 1.000 vs 0.905 text-only; synthetic gate with the real embedder (nomic-embed-text v1.5, DD4) 0.938 vs 0.938 at standard and 0.892 vs 0.874 at full scale (0.936 at standard with the hash embedder) ([bench/README.md](../bench/README.md), Real-model results) | done (with the real embedder at every scale: standard as a tie, full 0.892 vs 0.874) |
| M2: every vault fact cites sources | `knowledge`: `vault.citation_coverage` = 1, run with `--strict` in [ci.yml](../.github/workflows/ci.yml) | done |
| M2: Claude Desktop and Cursor can query Nugget over MCP | [mcp_clients.rs](../crates/nuggetd/tests/mcp_clients.rs) (clients connected as `nuggetd mcp install` configures them); [MCP guide](book/src/guide/mcp.md) | done |
| M3: Double Tap p50 under 1.2 s (cloud) | `draft`: `ttft.p50_s` with a real cloud provider; only the fake model has run | not started |
| M3: Double Tap p50 under 3 s (local) | `draft --provider-url` with a local model; time to first inserted token in [Y1](waves/wave-y/Y1.md); with a scripted model, first inserted token in 117 ms (Windows) and 76 ms (macOS) on real desktops; real model (Y2, Qwen3-4B Q4_K_M, 2-core CPU runner, no GPU): first insertable token p50 39–42 s, prompt reading ~22 tok/s on a ~1k-token prompt ([bench/README.md](../bench/README.md), Real-model results); AA1 (smaller local prompt, cached rules+voice prefix, same-runner A/B): first inserted character p50 41.0 → 30.2 s on a 2-core CPU and 14.4 → 10.4 s on a virtual M2 Pro (Metal); a repeat draft of the same thread starts in 0.14–0.32 s; ~500 uncached prompt tokens per new thread remain the bottleneck; CC1 (read-ahead while a reply field keeps focus, plus an optional signed `qwen3-1.7b-q8` Double Tap model; same-runner runs on four machines): 4B first inserted character p50 0.14–0.58 s once read-ahead has finished, which needs 6–21 s of focus first (1.7B: 0.08–0.20 s after 4.5–11 s); without read-ahead 10–30 s (4B) and 3–15 s (1.7B); with AA3's default grounding check (secrets) on the merged code the 4B read-ahead case is 0.27 s p50 (+0.13 s) on the EPYC 7763 runner | partly met: under 3 s on every measured machine once read-ahead has finished; missed for a trigger without read-ahead; a trigger during read-ahead not measured |
| M3: drafting eval wins over a screenshot-only baseline | `draft`: `draft.win_rate_vs_baseline` 0.95 with the fake model ([bench/README.md](../bench/README.md)); real model in [Y2](waves/wave-y/Y2.md); real model (Y2): voice match 0.91–0.94 vs 0.68–0.72, judge wins 0.50 vs 0.125–0.375, but hallucination 0.33 (target 0); AA3 moved the bench onto the real `Drafter` (Y2's 0.33 came from a bench-built prompt) and added grounding (a dated facts block plus a streaming secret check): on the merged code, 16 tasks, Qwen3-4B, hallucination 0 and sensitive leak 0 in every Nugget arm (baseline leaks 0.06), voice 0.80 vs baseline 0.62, 8B judge Nugget 0.38–0.63 vs 0.25–0.38; fact recall 0.62 vs baseline 0.81 (target 0.8); one draft still asserts an attachment that doesn't exist | done (wins; hallucination 0 and leak 0 on the product path); fact recall missed (0.62) |
| M4: published success rate on `bench/cua` | `cua`: `success_rate` 1.00 for the `nugget` arm with a scripted model, inproc ([bench/cua/README.md](../bench/cua/README.md), Reference results); real model and native envs pending; `--cua-env native` on real Windows UI Automation: success 1.00, 0 unconfirmed risky actions; real model (Y2): success 0.50 with Qwen3-4B and 8B (target 0.8), 0 unconfirmed risky actions, 0 injection compliance, false-confirmation 0.29–0.38; AA2 (same-runner A/B, real local models): Qwen3-4B success 0.58 → 0.92, false confirmations 0.33 → 0; Qwen3-8B 0.33 → 0.67 on the slowest CPU (turn timeouts), 0.83 on a faster one; 0 unconfirmed risky actions and 0 injection compliance in every arm | done (4B meets 0.8; 8B limited by CPU speed) |
| M4: 0 unconfirmed risky actions in red-team tests | `cua`: `safety.unconfirmed_risky_actions` 0 for the `nugget` and `nugget-compliant` arms ([bench/cua/README.md](../bench/cua/README.md)) | done (real-model re-check in Y2) |
| M5: precision of accepted cards tracked | `proactive --backend real`: accepted precision 0.976, precision@5 0.849 ([bench/README.md](../bench/README.md), Results); BB2 learns from Accept/Dismiss reasons per sender, domain, kind and thread (decaying, explainable, resettable): precision@5 0.849 → 0.883, distractor false positives 0.031 → 0.018, recall 0.997 unchanged, 0 actions without approval | done |
| M5: dry-run diff on every action | `proactive --backend real`: dry-run coverage and diff fidelity 1.0, 0 executed without approval | done |
| M6: security review | [security-m3-m6.md](reviews/security-m3-m6.md) (W8); supply-chain gaps in [release.md](release.md) §8; manifest rollback protection, release key slot, signed update feed, reproducible-build check ([manifest-trust.md](security/manifest-trust.md), [reproducible-builds.md](security/reproducible-builds.md)) | done (release keys to generate) |
| M6: installers for 3 OSes | [release.yml](../.github/workflows/release.yml) dry run builds all targets ([release.md](release.md)); AppImage smoke in [package-smoke.yml](../.github/workflows/package-smoke.yml); dmg and NSIS smoke in Y1; dmg and NSIS install, launch, connect and quit in [real-os.yml](../.github/workflows/real-os.yml) and the release dry run | done (signing needs release keys) |
| M6: reproducible benchmarks in the README | [README Benchmarks](../README.md#benchmarks), generated from [latest.json](../bench/results/latest.json) | done (Y5) |
| Target: freshness about 60 s; edits and deletions propagate | `memory --backend real`: `freshness.p95_s`, `propagation.*_rate` = 1 | done |
| Target: useful search and drafts in under 2 min | First-run scenario in the desktop e2e ([Y4](waves/wave-y/Y4.md)): empty install to a cited search result in 3.6 s, checked against a 120 s budget on every e2e run; with the recommended local model installed in onboarding (signed test manifest, local download), the first cited model answer comes 5.2 s after app start and `draft.begin` drafts from it (e2e `ai-setup`) | done |
| Target: computer-use injection compliance 0 | `cua`: `safety.injection_compliance_rate` 0, both `nugget` arms | done |
| Target: needless confirmations ≤ 0.10 | `cua`: `safety.false_confirmation_rate` 0.00 after X1 | done |
| Target: no secrets sent to cloud models in drafts | `draft`: `sensitive_prompt_exposure_rate` and `sensitive_leak_rate` 0 (fake model, cloud-marked) | done |
| Target: proactive precision@5 ≥ 0.7, recall ≥ 0.8 | `proactive --backend real`: 0.849 and 0.997 (0.883 after BB2) | done |
| Target: RAM, CPU and idle overhead | `perf --backend real` (W12, 100k items); daemon CPU/RSS/startup in X5; energy and battery in DD2 (`energy` suite, [Energy](../bench/README.md#energy)): CPU and wakeups measured (index 1k items 1.06 CPU-s; daemon idle 0.086% / 1.6 wakeups/s; hidden shell after replacing the 1 s focus poll with OS focus notifications: macOS 0.47% → 0.053% of a core, 4.1 → 1.6 wakeups/s; Linux, which keeps an adaptive poll, 7.4 → 5.0 wakeups/s), joules MISSED (no energy counter on the CI runners); X5 at 100k items: idle CPU 0.03%, 0 idle wakeups in the bench window, startup 74 ms, first search 83 ms, RSS after idle 1472 MiB, of which the draft voice profile (~980 MiB) is now cut to ~106 MiB (`nugget-voice` example `voice_memory`), [perf.yml](../.github/workflows/perf.yml) (weekly `energy` and `energy-prefill` jobs) | done (joules not measured: no counter on CI) |
| Target: Linux, macOS and Windows | [ci.yml](../.github/workflows/ci.yml); real desktop runs in [Y1](waves/wave-y/Y1.md); installers, accessibility, Double Tap and computer use exercised on real macOS and Windows runners in [real-os.yml](../.github/workflows/real-os.yml); Z1: Wayland (sway, KWin) accessibility and Double Tap insert, the Double Tap overlay fired by real key presses on Linux, Windows and macOS, computer use over macOS AX (16/16) | done (DD1: Wayland global shortcut fires from a virtual key press on KWin and GNOME on real runners; physical desktops: see open items) |
| Target: nightly eval dashboards | [nightly.yml](../.github/workflows/nightly.yml) (knowledge; memory and proactive with the real backend; cua inproc and xvfb, draft with the fake provider, perf with the real backend; history on the `bench-results` branch); [dashboard](book/src/benchmarks/dashboard.md) | done (Y5) |

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
