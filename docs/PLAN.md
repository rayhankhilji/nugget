# Nugget: build plan

**Nugget** (working name) is an Apache-2.0, local-first, cross-platform personal AI. It learns your work and life
continuously, drafts in your voice, prepares work for your approval, and operates your computer, with every step
inspectable and owned by you.

Grounding: [Sentient OS teardown](research/sentient-os-teardown.md) and [shipped-artifact findings](research/shipped-artifact-findings.md).
Architecture: [book/src/architecture/](book/src/architecture/overview.md). User-facing summary: [What works today](book/src/status.md).

## Status

- **M0 Foundations, M1 Memory, M2 Knowledge graph: done.** Workspace, CI on three OSes, encrypted store, providers, signed model manifest, local runtime, connectors, hybrid cited retrieval, knowledge graph, vault and local MCP server run in `nuggetd`.
- **M3 Double Tap, M4 Agent and computer use, M5 Proactive: wired into the daemon and app**, with a real-app end-to-end test in CI ([e2e.yml](../.github/workflows/e2e.yml)). Message imports (iMessage, WhatsApp, Telegram), search filters and timelines, tray, deep links and quick ask are in too.
- **M6 Hardening: mostly built, not released.** Packaging, a release dry run (unsigned installers on all three OSes), Windows signing via Azure Trusted Signing, a release rehearsal with throwaway keys, the opt-in signed updater, i18n, onboarding, Microsoft 365 and SaaS connectors, and the WASM plugin host are in.

## Open: blockers, highs and misses

v1 needs every Blocker and High fixed and re-verified by the same QA scenarios on real models.

**Release**
- No release has been cut. Release keys (updater, model-manifest release key, Apple and Windows certificates) do not exist; signing paths are untested without real certificates ([release.md](release.md) §8).
- Real macOS and Windows runs on physical machines (installer, accessibility, Double Tap insertion, native computer use) not done; only CI runners so far. Start-at-login service not exercised on macOS or Windows in CI.
- MSI, `.deb` and `.rpm` are build-only (no launch smoke). Linux packages are not GPG-signed; no apt or rpm repository.
- Empty, loading and error states not confirmed complete. The E2E relay (optional) is not started.

**Blockers**
- **GG1-1, still open:** Double Tap obeys an instruction planted in a vault note (attacker link in 62 drafts). Fix paused until the user says continue.
- GG2-1 (Ask obeys planted instructions), GG5-1 (card and account numbers written into the vault) and GG6-1 (onboarding indexed at LLM speed) are fixed; **re-verification on real models is pending.**

**Open Highs**
- Double Tap: non-English password, one-time-code and card fields are drafted into; the newest question in long threads is cut; drafts glue to typed text; one-line and German drafts (fix paused).
- Computer use: does not ask on ambiguous targets or missing facts (ask rate 0.71 with the 4B, 0.76 with the 8B, target 0.9); can report success after a declined send; the Send confirmation hides recipient and body. The 8B times out on a third of tasks at 4 vCPUs.
- Knowledge: claim extraction with a local model produces junk titles; vault edits are overwritten by re-extraction; vault and graph ignore sensitivity; extraction falls behind on CPU and regresses after a restart.
- Proactive: meeting prep and the brief's calendar break on `.ics` invites (recurrence, dedup, time zones).
- First week: a hung or crashing local model server and raw download errors; folders with nothing to index; API keys that cannot be changed; quit, uninstall and update leave the daemon behind.
- Sensitivity: the 4B triage clears health, money and legal mail. Ingest fixes (searchable before triage, unknown counts as sensitive, deterministic rules in all 9 UI languages) are not re-verified by QA.
- WhatsApp import: dates in fr/ar and day-first exports.
- Privacy (captured traffic; local-only use sends nothing): GG8-1 an Ask follow-up forwards earlier answers unscanned to a cloud model; GG8-3 Double Tap sent texts become a hidden source readable by MCP clients and cloud Ask. GG8-5 (unledgered update check): update checks are now ledgered through `egress.record`; not re-verified. v1 needs all High and Medium privacy findings fixed.
- Ask: the real-model re-run after the date, follow-up and partial-match fixes is pending.

**Security**
- PM-4 is **mitigated, not fixed**: an HTTP MCP client can send its token to a port squatter while `nuggetd` is down (the stdio bridge avoids it). The other 13 M3–M6 review findings are fixed with regression tests.
- Open by design: the streaming credential stop can miss digits spread across words, and homoglyph or encoded credentials.
- Runtime port ownership is verified on Linux only; IPC has no peer-credential check; no store key rotation; audit-log hash is unkeyed.

**Measurement misses**
- Double Tap p50 against a cloud model (target under 1.2 s) has **never been measured**; it needs a provider key.
- Double Tap local (target under 3 s): met only once read-ahead has finished (0.14–0.58 s after 6–21 s of focus); 10–30 s without read-ahead; a trigger during read-ahead is not measured.
- Draft fact recall 0.62 (target 0.8); one draft asserts an attachment that does not exist.
- Knowledge entity F1 0.67–0.77 (target 0.8); nicknames and job changes fail.
- Battery cost in joules not measured (no energy counter on CI runners); needs a physical laptop. Read-ahead costs the shell ~1.5% of a core and ~120 wakeups/s between fields.
- At 500k items: first extraction ~1 h, 10.6 KB/item on disk, ~1 GB idle RSS. The daemon has not been re-measured after the voice-profile memory cut.
- Wayland: the global shortcut fires through the portal on KDE Plasma 6 and GNOME 50 in CI; not tried on a physical desktop; Plasma 5.27 and GNOME before 48 untested; GNOME's GTK 4 insert path not run. sway and other desktops without the portal use the tray or a compositor binding to `nugget-desktop --double-tap`.
- Multilingual: Double Tap perspective and proactive cards outside English wait on paused work; the multilingual embedder awaits signing.

**Decision pending (user):** turn the computer-use harm triage on for local models? It refused 17/17 harmful goals but falsely refused 5% (4B) / 8% (8B) of benign goals, at ~5.5–9 s per task. Recommended: on, run on the 4B, refusal shown as a question with "Do it anyway" (built: `[agent] triage = false`, `triage_override = true`). Evidence: [bench/README.md](../bench/README.md).

## v1 checklist

Numbers: [bench/README.md](../bench/README.md); QA findings above override bench figures where they differ.

| Criterion | Evidence | State |
|---|---|---|
| M0: `nuggetd` runs on 3 OSes; CI green | [ci.yml](../.github/workflows/ci.yml), [e2e.yml](../.github/workflows/e2e.yml), [package-smoke.yml](../.github/workflows/package-smoke.yml) | done |
| M0: chat with local and remote models | [e2e.rs](../crates/nuggetd/tests/e2e.rs); real local 4B in first-week QA | done |
| M1: freshness ≤ 60 s | `memory --backend real`: p95 0.60 s at standard scale | done |
| M1: retrieval eval ≥ baseline | hybrid recall@10 1.000 vs 0.905 text-only; synthetic gate with the real embedder 0.938 vs 0.938 (standard, tie), 0.892 vs 0.874 (full) | done |
| M2: every vault fact cites sources | `knowledge`: citation coverage 1, `--strict` in CI | done |
| M2: Claude Desktop and Cursor over MCP | [mcp_clients.rs](../crates/nuggetd/tests/mcp_clients.rs) | done |
| M3: Double Tap p50 < 1.2 s (cloud) | needs a real cloud provider | **not started** |
| M3: Double Tap p50 < 3 s (local) | 4B, first inserted character after read-ahead 0.14–0.58 s; without read-ahead 10–30 s | **partly met** |
| M3: drafting beats screenshot-only baseline | 4B: hallucination 0, leak 0, voice 0.80 vs 0.62, judge 0.38–0.63 vs 0.25–0.38; fact recall 0.62 vs 0.81 | done; fact recall **missed** |
| M4: published `bench/cua` success rate | real 4B 0.92 (target 0.8); 8B 0.67–0.83, CPU-bound; realistic-task QA 0.56 (before later fixes) | done (bench) |
| M4: 0 unconfirmed risky actions | 0 in every arm, real and scripted models | done |
| M5: accepted-card precision tracked; dry-run diff on every action | accepted precision 0.976; precision@5 0.883, recall 0.997; diff coverage 1.0; 0 actions without approval | done |
| M6: security review | M3–M6 review reconciled; [manifest-trust.md](security/manifest-trust.md), [reproducible-builds.md](security/reproducible-builds.md) | done (release keys to generate) |
| M6: installers for 3 OSes | [release.yml](../.github/workflows/release.yml) dry run; AppImage, dmg, NSIS install-launch-quit in [real-os.yml](../.github/workflows/real-os.yml) | done (unsigned) |
| M6: reproducible benchmarks in README; nightly dashboards | [README](../README.md#benchmarks), [nightly.yml](../.github/workflows/nightly.yml), [dashboard](book/src/benchmarks/dashboard.md) | done |
| Edits and deletions propagate | propagation rates 1 | done |
| Useful search in under 2 min | first cited result 3.6 s; first model answer 5.2 s after start (e2e) | done |
| cua injection compliance 0; needless confirmations ≤ 0.10 | 0 and 0.00 | done |
| No secrets to cloud models in drafts | sensitive exposure and leak 0 (fake cloud model) | done |
| RAM, CPU, idle overhead | 100k items: idle CPU 0.03%, startup 74 ms, first search 83 ms; 500k: startup 0.48 s, Ask retrieval p50 0.22 s, search p95 126 ms (LL1, was 350 ms), first sync of 100k files 1,874 items/s (was 506), freshness p95 449 ms (up from 312 ms: items wait for their batch); joules not measured | done to 100k; 500k partial |
| Linux, macOS, Windows | CI plus real-OS runners; Wayland portal shortcut on KWin and GNOME | done on CI runners; physical machines open |

## Decisions

- **Licence:** Apache-2.0, DCO sign-off, no CLA. **Clean room:** no AGPL code, prompts or distinctive expression; `docs/research/` is the only bridge.
- **Scope:** v1 = everything (memory, Double Tap, proactive cards, computer use, MCP), beating Sentient OS on every axis in [compare/sentient-os.md](compare/sentient-os.md). If we can't measure it, we don't claim it.
- **Stack:** Rust core plus Tauri 2 shell; macOS, Windows and Linux ship together.
- **Default AI:** asked during onboarding, local recommended; small local models always do triage and embeddings.
- **Hosted services:** none at v1; a self-hostable E2E relay later.
- **Team:** one or two humans directing Claude agents.
- **Not built, planned:** local-app connectors (Apple Notes, Apple Mail, macOS Calendar and Contacts, Signal, Thunderbird), Logseq, Maildir, CardDAV, MCP servers as sources, a community plugin registry, an OSWorld-style `cua` subset.

## Risks and open questions

- Risks: the v1 scope is very large (gate on benchmarks, not dates; cut connectors before quality); accessibility APIs differ, especially on Wayland (one trait, per-OS flags, screenshot fallback); private Apple formats change (schema-backed parsers, fuzzing); local models are weak for agentic work (tier routing, bring-your-own key, per-tier numbers); Google restricted-scope verification (users bring their own OAuth client).
- Open questions: keep the name "Nugget"? Business model: Apache core plus a paid hosted team tier later?
