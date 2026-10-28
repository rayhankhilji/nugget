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

## Local agents (this container)
- **M2-F:** `bench` knowledge suite.
- **M2-G1:** daemon wiring for knowledge, vault and MCP, the `nuggetd mcp` bridge and the install helper.
- **M2-G2:** graph contract upgrades (update_claim, Rejected, batch, provenance, split) and their adoption.

## Waiting
- **M4 computer-use driver** (`nugget-cua`, on top of `nugget-a11y`): starts once `m3/a11y` merges.
- **Integration waves for M3–M6:** daemon IPC wiring and desktop surfaces, after their branches merge.
