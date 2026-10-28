# Wave W: wiring M3–M6 into the product

Twelve parallel work packages, one cloud session each. Each session is a swarm lead that splits
its package across its own subagents; the orchestrator merges the `w/*` branches into
`claude/inspiring-wozniak-z3gg3n`, regenerates `Cargo.lock` and fixes seams between them.

| WP | Branch | Package |
|---|---|---|
| W1 | `w/doubletap` | Double Tap end to end: daemon `draft.*`/`voice.status`, hotkey, overlay, insertion |
| W2 | `w/computer-use` | Computer use on accessibility: `nugget-a11y` actions, `nugget-computer` tools, bench arm |
| W3 | `w/agent` | Agent, safety policy and browser bridge in the daemon; Agent and Safety pages |
| W4 | `w/proactive` | Proactive cards and the morning brief: daemon, executors, Today page, real bench |
| W5 | `w/connectors` | Microsoft 365 and SaaS connectors registered and in the Sources wizard; power policy |
| W6 | `w/plugins` | Plugins in the daemon and app; trust, approvals, examples, CI |
| W7 | `w/knowledge` | Knowledge quality: bench targets pass, `rejected` status, MCP provenance |
| W8 | `w/security` | Adversarial security review of M3–M6 with fixes |
| W9 | `w/platform` | Cross-OS CI and M0 hardening follow-ups |
| W10 | `w/docs` | Docs for the wired product, Sentient comparison, architecture diagram |
| W11 | `w/desktop` | Desktop shell quality and UI tests |
| W12 | `w/retrieval` | Retrieval quality and store performance at scale |

Daemon features plug in through `crates/nuggetd/src/features/` (one module each). Each brief
(`W<n>.md`) lists what the package owns.
