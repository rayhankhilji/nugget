# Wave X: release readiness

Starts from 11bb209, the first commit with CI green on macOS, Windows and Linux. Five cloud sessions,
each a swarm lead; the orchestrator merges the `x/*` branches into `claude/inspiring-wozniak-z3gg3n`.

| WP | Branch | Package |
|---|---|---|
| X1 | `x/safety-tuning` | Fewer needless confirmations (0.29 → ≤ 0.10), real injection detection |
| X2 | `x/knowledge-ux` | Graph view labels and polish, entity page, W7 follow-ups |
| X3 | `x/release` | Unsigned installer dry run on all three OSes, AppImage smoke test, release checklist |
| X4 | `x/wiring` | Reranker, conversational search, chat citations, W1/W4 gaps |
| X5 | `x/perf` | Daemon CPU/RSS/startup at 100k items, W9/W12 follow-ups |
