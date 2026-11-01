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

Run `cargo run --release -p nugget-bench -- <suite>` (`--list` lists suites; CI runs
`all --scale smoke --no-write`). Quote numbers only from `standard` or `full` scale; `--strict` fails
on a missed target. Fake-index and scripted-model numbers validate the harness, not the product.

## Headline results

Freshness p95 0.60 s (target 60 s, local folder); hybrid recall@10 1.000 vs 0.905 text-only on the
labelled set; synthetic gate with the real embedder 0.938 vs 0.938 (standard) and 0.892 vs 0.874
(full); edits, creates, deletes and renames all reflected. Other suites: [What works today](../status.md).

MMR lowers known-item recall@10 on the synthetic corpus and is reported, not gated. Not measured yet:
citation accuracy, battery cost in joules, and Double Tap latency with a cloud model.
