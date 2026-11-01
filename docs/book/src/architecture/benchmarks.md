# Benchmarks

Rule: **if we can't measure it, we don't claim it.** Claims come from `nugget-bench` reports on release
builds. Methodology, metric definitions and full results (including real-model runs) are in
[`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md); the
[dashboard](../benchmarks/dashboard.md) charts nightly runs.

| Suite | Measures |
|---|---|
| `memory` | Retrieval recall and MRR, change-to-searchable freshness, edit/delete/rename propagation, ingest throughput, size. `--backend real` runs the real store, ingest, watcher and retriever |
| `perf` | Ingest and query costs, daemon CPU, memory, startup and idle overhead at scale |
| `knowledge` | Entity and claim precision and recall, unsupported claims, injection resistance, citation coverage, edit merge, MCP round trips |
| `draft` | Voice match, fact recall, hallucination, injection resistance, sensitive leaks, time to first token, against a "whole vault plus screenshot" baseline |
| `cua` | Computer-use success, steps, time, unconfirmed risky actions, injection compliance |
| `proactive` | Card precision and recall, distractor false positives, dedup, learning, dry-run coverage |

```sh
cargo run -p nugget-bench -- --list
cargo run -p nugget-bench -- all --scale smoke --no-write                 # what CI runs
cargo run --release -p nugget-bench -- memory --backend real              # the M1 gate
cargo run -p nugget-bench -- cua --cua-agent reference --cua-env xvfb --strict   # Linux only
```

Scales are `smoke`, `quick`, `standard` (default) and `full`; quote numbers only from `standard` or
`full`. `--strict` fails when a metric misses its target. Reports validate against
`bench/schema/report.schema.json`. Fake-index and scripted-model numbers validate the harness and are
not product numbers.

## Headline results

| Target | Result |
|---|---|
| Freshness p95 ≤ 60 s | 0.60 s (local folder; mail and calendar poll every 60 s to 5 min) |
| Hybrid recall@10 ≥ text-only, labelled eval set | 1.000 vs 0.905 |
| Synthetic recall gate, real embedder | 0.938 vs 0.938 (standard, tie); 0.892 vs 0.874 (full) |
| Edits, creates, deletes, renames reflected | 1.0 |

MMR lowers known-item recall@10 on the synthetic corpus and is reported, not gated. Not measured yet:
citation accuracy, battery cost in joules, and Double Tap latency with a cloud model.
