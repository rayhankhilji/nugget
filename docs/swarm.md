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
| X5 | `x/perf` | session_01CTjRSsrJLKVRYFyVjzqDnH | daemon CPU/RSS/startup at 100k items, W9/W12 follow-ups |

## Wave Y (v1 evidence): cloud sessions from 308f775
Briefs in `docs/waves/wave-y/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| Y1 | `y/real-os` | session_01CTjRSrWcPgzD1WCLwMPhhi | installer smoke, accessibility, Double Tap and computer use on real macOS/Windows runners (merged) |
| Y2 | `y/real-models` | session_01CTjRSswimZrnz747s9AdLp | benches with real local models; latency, drafting, computer use, extraction |
| Y3 | `y/trust` | session_01CTjRSrvKuQh87zTMFfWoXZ | manifest rollback protection, release key, signed update feed, reproducible builds (merged) |
| Y4 | `y/first-run` | session_01CTjRSr8rCA1mYQCUBBFN8z | measured time to first value, empty/error states, X4 gaps, screenshots (merged) |
| Y5 | `y/dashboards` | session_01CTjRSuGsEKLz91CWSGSx6H | nightly benches, dashboard, generated README numbers, PLAN status (merged) |

## Wave Z (last untested paths, translations, AI setup, hardening): cloud sessions from 6f54817
Briefs in `docs/waves/wave-z/`.

| WP | Branch | Session | Scope |
|---|---|---|---|
| Z1 | `z/desktop-coverage` | session_01CTjRSpodmRHxnXjWF2CDXH | Wayland, the Double Tap overlay on three OSes, computer use on macOS |
| Z2 | `z/i18n` | session_01CTjRSspNzvkrF5GkeK22rE | complete all catalogs, Arabic and RTL, overflow checks |
| Z3 | `z/ai-setup` | session_01CTjRSrXicS94EepP69WwKH | onboarding model install to cited answer, key validation, ask fallback |
| Z4 | `z/hardening` | session_01CTjRStVNmwX45S5GjXvhm3 | open security-review rows, fuzzing, context gaps, strict reproducibility |
