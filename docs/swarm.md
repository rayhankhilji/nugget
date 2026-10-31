# Swarm roster

Parallel work packages. Each package owns its crate or directory and works on its own branch.
The orchestrator merges each branch into `claude/inspiring-wozniak-z3gg3n`, regenerates
`Cargo.lock` and verifies the whole workspace.

## Cloud sessions (separate containers)
| WP | Branch | Session | Scope |
|---|---|---|---|
| M3-A | `m3/a11y` | session_01CTjRSsgGbRB9EmsVkUwEvs | `nugget-a11y`: AX / UIA / AT-SPI read and insert |
| M3-B | `m3/hotkey` | session_01CTjRSr7cY6jSeFDAygP2Mn | `nugget-hotkey`: global trigger, kill switch |
| M3-C | `m3/draft` | session_01CTjRSri5Cy24683KhNyJhL | `nugget-draft`: drafting engine |
| M3-D | `m3/voice` | session_01CTjRSrfkob6kp1V4f3bU1k | `nugget-voice`: voice profile |
| M3-E | `m3/latency` | session_01CTjRSqp94W9osALWkJ3AqD | `nugget-infer`: caching, prewarm, timings |
| M3-F | `m3/bench-draft` | session_01CTjRSsU6rHqAzyXxBsiJUp | `bench`: draft suite |
| M4-A | `m4/agent` | session_01CTjRSrpmHLcfMhhxjhNFHA | `nugget-agent`: agent loop |
| M4-B | `m4/policy` | session_01CTjRSrwSdUnGD47VKReTKe | `nugget-policy`: safety engine, red-team suite |
| M4-C | `m4/browser` | session_01CTjRSsibjKq18UjXgD4qQs | `extensions/browser` + `nugget-browser` native host |
| M4-D | `m4/bench-cua` | session_01CTjRSuNL9CvQu4cMuZomrg | `bench`: cua suite with Linux task environment |
| M5-A | `m5/proactive` | session_01CTjRSqe3LKQj5cFvhGkuEC | `nugget-proactive`: cards, morning brief |
| M5-B | `m5/bench-proactive` | session_01CTjRSt8F3TyEtXpA44BGEz | `bench`: proactive suite |
| M6-A | `m6/plugin` | session_01CTjRSs7Pv9q63tANHv67P9 | `nugget-plugin`: WASM plugin runtime and SDK |
| M6-B | `m6/packaging` | session_01CTjRSqBSh4sJy8x38q5uJh | bundling, sidecar, release workflow, signed opt-in updates |
| M6-C | `m6/onboarding-i18n` | session_01CTjRSqqCZDXUpK6dWeNNBx | desktop onboarding, 8 locales, a11y checks |
| M6-D | `m6/docs` | session_01CTjRSqpkGF6ggUXaNzJ5uK | mdBook user and developer docs with a drift check |
| M6-E | `m6/microsoft` | session_01CTjRSqh5PtecX1ahjUxQV7 | Outlook mail and calendar, OneDrive |
| M6-F | `m6/saas` | session_01CTjRSrQY2KnVUFW2SW5ex3 | Slack, Notion, GitHub, Linear |

## Wave W (wiring M3–M6 into the product): cloud sessions, each a swarm lead with subagents — all merged
Briefs: `docs/waves/wave-w/`. Daemon features plug in through `crates/nuggetd/src/features/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| W1 | `w/doubletap` | session_01CTjRStnoX652MRxbNwSpAM | Double Tap end to end |
| W2 | `w/computer-use` | session_01CTjRSsopXCg3H46VhuHkzE | computer use on accessibility (`nugget-computer`) |
| W3 | `w/agent` | session_01CTjRStKEKwwk7JPoKfPkBa | agent, safety policy, browser bridge wired in |
| W4 | `w/proactive` | session_01CTjRSsZfijoUUui2srYGCa | proactive cards, morning brief, Today page |
| W5 | `w/connectors` | session_01CTjRSqdohXyVJ3dgv6FZVf | Microsoft 365 and SaaS connectors in the app; power policy |
| W6 | `w/plugins` | session_01CTjRStDrNuY9AEiEMfZrMQ | plugins in the daemon and app |
| W7 | `w/knowledge` | session_01CTjRSuCgDAG5bV1tVw22qL | knowledge quality, M2 exit criteria |
| W8 | `w/security` | session_01CTjRStu5JF2bP3MNj6hBmi | security review of M3–M6 |
| W9 | `w/platform` | session_01CTjRSs7rCLYgT4e1XXg9cJ | cross-OS CI, M0 hardening follow-ups |
| W10 | `w/docs` | session_01CTjRSpisXZEZ8cUcvXkasf | docs, Sentient comparison |
| W11 | `w/desktop` | session_01CTjRStmMDYih1pk2k7ko9x | desktop shell quality, UI tests |
| W12 | `w/retrieval` | session_01CTjRSqr4eEo2eWw8xKSmak | retrieval quality, store performance |

## Merged
All M0–M6 wave-1 branches above, and the local M2-F (knowledge bench), M2-G1 (daemon wiring for
knowledge, vault and MCP) and M2-G2 (graph contract upgrades) agents.
All twelve wave W packages, plus the local X4 real-app end-to-end test (`apps/desktop/e2e`, run in
CI by `.github/workflows/e2e.yml`).

## Wave X (release readiness): cloud sessions from the first all-green commit (11bb209)
Briefs in `docs/waves/wave-x/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| X1 | `x/safety-tuning` | session_01CTjRSrsidrNJCePPcDqeNS | fewer needless confirmations, injection detection (merged) |
| X2 | `x/knowledge-ux` | session_01CTjRSuTbenvZTQvGqobCoo | graph view and entity page polish, W7 follow-ups (merged) |
| X3 | `x/release` | session_01CTjRSuHipddEqZu3cBA7Wr | unsigned installer dry run on three OSes, AppImage smoke (merged) |
| X4 | `x/wiring` | session_01CTjRSqHjCrA9Qxn9PCyqHN | reranker, conversational search, chat citations, W1/W4 gaps (merged) |
| X5 | `x/perf` | session_01CTjRSsrJLKVRYFyVjzqDnH | daemon CPU/RSS/startup at 100k items, W9/W12 follow-ups (merged) |

## Wave Y (v1 evidence): cloud sessions from 308f775
Briefs in `docs/waves/wave-y/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| Y1 | `y/real-os` | session_01CTjRSrWcPgzD1WCLwMPhhi | installer smoke, accessibility, Double Tap and computer use on real macOS/Windows runners (merged) |
| Y2 | `y/real-models` | session_01CTjRSswimZrnz747s9AdLp | benches with real local models; latency, drafting, computer use, extraction (merged) |
| Y3 | `y/trust` | session_01CTjRSrvKuQh87zTMFfWoXZ | manifest rollback protection, release key, signed update feed, reproducible builds (merged) |
| Y4 | `y/first-run` | session_01CTjRSr8rCA1mYQCUBBFN8z | measured time to first value, empty/error states, X4 gaps, screenshots (merged) |
| Y5 | `y/dashboards` | session_01CTjRSuGsEKLz91CWSGSx6H | nightly benches, dashboard, generated README numbers, PLAN status (merged) |

## Wave Z (last untested paths, translations, AI setup, hardening): cloud sessions from 6f54817
Briefs in `docs/waves/wave-z/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| Z1 | `z/desktop-coverage` | session_01CTjRSpodmRHxnXjWF2CDXH | Wayland, the Double Tap overlay on three OSes, computer use on macOS (merged) |
| Z2 | `z/i18n` | session_01CTjRSspNzvkrF5GkeK22rE | complete all catalogs, Arabic and RTL, overflow checks (merged) |
| Z3 | `z/ai-setup` | session_01CTjRSrXicS94EepP69WwKH | onboarding model install to cited answer, key validation, ask fallback (merged) |
| Z4 | `z/hardening` | session_01CTjRStVNmwX45S5GjXvhm3 | open security-review rows, fuzzing, context gaps, strict reproducibility (merged) |

## Wave AA (real-model quality): cloud sessions from 31ec850
Briefs in `docs/waves/wave-aa/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| AA1 | `aa/doubletap-latency` | session_01CTjRSuDx768RtwbJTaiHvE | local Double Tap under 3 s: prompt budget, prefix cache, Apple Silicon numbers (merged) |
| AA2 | `aa/agent-quality` | session_01CTjRSpmAvmjUGCE3Js3Ytu | computer use with small local models: success ≥ 0.8, false confirmations ≤ 0.10 (merged) |
| AA3 | `aa/grounding` | session_01CTjRSseja8BUetk34ePA1G | drafts without hallucinations, extraction F1 ≥ 0.8 with real models (merged) |

## Wave BB (depth alongside Z1 and AA): cloud sessions from 3924cf1
Briefs in `docs/waves/wave-bb/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| BB1 | `bb/google-drive` | session_01CTjRStcm2Tq4cK6HdhtSor | Google Drive, Docs, Sheets and Slides connector (merged) |
| BB2 | `bb/proactive-learning` | session_01CTjRSt2zXMhX5puD2vRYrP | cards learn from Accept/Dismiss feedback, explainable and resettable (merged) |
| BB3 | `bb/a11y-audit` | session_01CTjRSuD9VRtjRwVksuYKcZ | axe audit on every page and dialog, keyboard-only core flows, screen readers (merged) |

## Wave CC (local Double Tap under 3 s): cloud session from b5cc85a
Brief in `docs/waves/wave-cc/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| CC1 | `cc/doubletap-fast` | session_01CTjRSs68FrQ6bjtvDEG7tf | small draft model for Double Tap, read-ahead on focus (local only), four-way latency measurement (merged) |

## Wave DD (independent of AA3): cloud sessions from 40e1994
Briefs in `docs/waves/wave-dd/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| DD1 | `dd/wayland-shortcut` | session_01CTjRSrbhjVwZjhmA3QQoAb | Double Tap global shortcut on Wayland (portal app id, packaging, KWin/GNOME CI) (merged) |
| DD2 | `dd/energy` | session_01CTjRSt524Nw1erCRtS19P5 | energy and battery cost of daemon, shell, indexing and read-ahead; cut waste (merged) |
| DD3 | `dd/agent-judgement` | session_01CTjRSsAKmoyDDN46QUNq67 | computer-use agent asks when targets are ambiguous; triage on/off evidence (merged) |
| DD4 | `dd/recall-real-embedder` | session_01CTjRSrbHfZnGNeRxohghVF | memory recall gates with the real embedding model and reranker (merged) |

## Wave EE (after AA3, alongside DD): cloud sessions from the EE briefs commit
Briefs in `docs/waves/wave-ee/`. Each lead runs a build layer and an independent review layer of subagents.

| WP | Branch | Session | Scope |
|---|---|---|---|
| EE1 | `ee/entity-resolution` | session_01CTjRSqYumRoCNPsSqWU2cj | knowledge entity resolution (nicknames, job changes) to entity F1 ≥ 0.8 |
| EE2 | `ee/draft-recall` | session_01CTjRSt5sQsEtmsjpgvdjDp | draft fact recall ≥ 0.8, no invented attachments, read-ahead latency kept |
| EE3 | `ee/ask-quality` | session_01CTjRSuP6na1rnf1rpacnuU | Ask answer-quality suite with real models (correctness, citations, abstention) and fixes |
| EE4 | `ee/security-review-2` | session_01CTjRSrVUg4uRkQ2yhjxPSi | security review of AA, BB, CC and manifest v4; fixes with regression tests (merged) |
| EE5 | `ee/release-readiness` | session_01CTjRSqf3cixr49jrhBuKmL | Windows cloud signing path, throwaway-key release rehearsal, release checklist, status page |
| EE6 | `ee/ui-polish` | session_01CTjRSuMKsfCZJQtWc4LnsJ | page-by-page UI review in light/dark/RTL/de, fixes, visual regression check (merged) |

## Wave FF (alongside DD and EE): cloud sessions from b0ea28f
Briefs in `docs/waves/wave-ff/`. Each lead runs a build layer and an independent review layer of subagents.

| WP | Branch | Session | Scope |
|---|---|---|---|
| FF1 | `ff/data-safety` | session_01CTjRSquqrkkGYfv4TUQEat | crash-safety harness, encrypted backup/restore, open-format export, corruption recovery |
| FF2 | `ff/connector-hardening` | session_01CTjRSsffsNYDoWtbuPyEx5 | connector conformance suite (pagination, 429s, token refresh, deltas, crashes), fixes, sync status (merged) |
| FF3 | `ff/a11y-apps` | session_01CTjRSrGRrLv1FvtvRdVkMp | Double Tap read/insert matrix across native editors, browsers and rich-editor frameworks on 3 OSes |
| FF4 | `ff/mcp-plugins` | session_01CTjRSrAf5oakXNcKynQoQK | MCP compatibility with official SDK clients, plugin SDK and three first-party plugins |

## Wave GG (independent QA, new files only): cloud sessions from 5f1998e
Briefs in `docs/waves/wave-gg/`. QA leads explore with subagents and have every finding reproduced by a second subagent; findings go to the owning build team through the orchestrator.

| WP | Branch | Session | Scope |
|---|---|---|---|
| GG1 | `qa/doubletap` | session_01CTjRStX6Dx5a8tNZozvNrv | Double Tap end to end, 60+ scenarios, real models |
| GG2 | `qa/ask` | session_01CTjRSsqzVXFPgqGksu9bV6 | Ask and Chat over a realistic persona corpus, 100+ questions |
| GG3 | `qa/agent` | session_01CTjRStQCE4cZD93QkhUFM6 | computer-use agent, 50+ tasks incl. injection, kill switch, confirmations |
| GG4 | `qa/proactive` | session_01CTjRSs4jvVBSuALYkXyaPB | proactive cards and morning brief over two simulated weeks |
| GG5 | `qa/knowledge` | session_01CTjRStixqfmN8YjP6FXqRq | knowledge graph and vault on hard real-life cases |
| GG6 | `qa/first-week` | session_01CTjRSsqNHZBx8FKRogmMJe | a new user's first week on macOS, Windows and Linux |
| GG7 | `qa/multilingual` | session_01CTjRSuVMK4RoFUCfw3YT7S | AI features in all 9 UI languages |
| GG8 | `qa/privacy` | session_01CTjRSt3BBwCh15spCoLbbe | privacy and egress verified from captured network traffic |
