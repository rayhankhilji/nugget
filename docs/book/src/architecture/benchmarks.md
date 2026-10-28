# Benchmarks

Nugget's rule, from the plan: **if we can't measure it, we don't claim it.** Every performance or
quality claim comes from a report produced by `nugget-bench` on a release build. This page summarises
the current results. The full methodology, metric definitions and raw tables are in
[`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md).

## Suites

Six suites are registered (`bench/src/suites/`). `cargo run -p nugget-bench -- --list` prints them.

| Suite | State | Measures |
|---|---|---|
| `memory` | Implemented: a fake in-crate index (default) or the **real stack** (`--backend real`: encrypted store, ingest pipeline, files watcher and retriever in process) | Retrieval recall and MRR, change-to-searchable freshness, edit, insert, delete and rename propagation, ingest throughput, database size, memory |
| `perf` | Implemented against the fake index only | Ingest, dedup, update, delete and query micro-benchmarks |
| `knowledge` | Implemented: the real M2 pipeline (`nugget-knowledge`, `nugget-vault`, `nugget-mcp`) on a labelled fixture corpus, with a scripted fake model | Entity and claim precision and recall, resolution, unsupported claims, injection resistance, citation coverage, user-edit merge fidelity, retraction, MCP `get_context` round trips |
| `draft` | Implemented: Nugget's drafting context against a Sentient-style "whole vault plus screenshot" baseline, with a deterministic fake model by default or a real one (`--provider-url`) | Voice match (stylometric), fact recall, hallucination rate, injection resistance, sensitive-item leaks, prompt size, time to first token and total latency |
| `cua` | Implemented: six deterministic test apps, run in process, over a loopback socket, or on an Xvfb desktop with AT-SPI (`--cua-env`); agents `reference`, `null` and `naive` (`--cua-agent`) | Computer-use task success, steps, time, unconfirmed risky actions and injected instructions followed |
| `proactive` | Implemented against an in-crate reference engine over a 30-day fixture world; `--backend real` (the `nugget-proactive` crate) is not implemented yet | Card precision and recall, distractor false positives, dedup, learning curve, dry-run coverage and the approval gate, against a recency baseline |

The `cua` agents validate the harness: no Nugget agent arm exists yet. Computer use and the real
proactive engine are being wired into the daemon in wave W, and their bench arms with them.

```sh
cargo run -p nugget-bench -- --list                                     # registered suites
cargo run -p nugget-bench -- all --scale smoke --no-write              # what CI runs (it writes reports)
cargo run --release -p nugget-bench -- memory --backend real            # the M1 gate
cargo run --release -p nugget-bench -- memory --backend real --scale quick
cargo run -p nugget-bench -- knowledge --strict
cargo run -p nugget-bench -- cua --cua-agent reference --cua-env xvfb --strict   # Linux only
```

Scales are `smoke`, `quick`, `standard` (the default) and `full`; quote numbers only from `standard`
or `full` on a release build. Reports are JSON files in `bench/results/` (`--out-dir` changes that,
`--no-write` skips them), checked in CI against `bench/schema/report.schema.json`. `--strict` exits
non-zero when a suite is not implemented or a metric misses its target.

Fake-index numbers validate the harness and serve as a regression baseline; they are **not** product
numbers and are not quoted here.

## Results: `memory --backend real` (M1, 2026-10-28)

**Setup.** A cloud VM with 4 vCPUs (Intel Xeon, 2.80 GHz) and 15 GiB RAM, Ubuntu 24.04, ext4. Release
build. Seed 42, default settings (300 ms watcher debounce, default chunking and retrieval). The real
encrypted store, ingest pipeline, files connector and retriever run in process. **Embeddings come from
a deterministic hashing test embedder (256 dimensions), not a real model.** That keeps runs
reproducible but means the vector and hybrid numbers say nothing about semantic quality.

### Targets

| Target | quick (2 000 docs) | standard (5 000 docs) |
|---|---|---|
| Freshness p95 at most 60 s | 0.45 s: **pass** | 0.60 s: **pass** |
| Hybrid recall@10 at least the fake index's, same corpus and queries | 0.955 vs 0.930: **pass** | 0.936 vs 0.938: **missed** by 0.2 points (1 query in 500) |
| Every edit, create, delete and rename fully reflected | 1.0: **pass** | 1.0: **pass** |
| Hybrid recall@10 at least text-only on the labelled eval set | 1.000 vs 0.905: **pass** | same set: **pass** |

### Freshness

From the file-system call returning to `search` reflecting the change completely:

| | quick | standard |
|---|---|---|
| Operations | 100 | 300 |
| p50 / p95 / max | 0.39 / 0.45 / 0.53 s | 0.48 / 0.60 / 0.72 s |
| Timeouts (120 s) | 0 | 0 |

Most of the half second is the watcher's 300 ms debounce. Against the 60 s target this is a margin of
about 100 times, **on a local folder**. Mail and calendar sources poll (every 60 s for Gmail, 5 min for
Calendar and CalDAV) and are not covered by this benchmark.

### Retrieval and latency

| | quick | standard |
|---|---|---|
| Full-text recall@10 | 0.980 | 0.966 |
| Hybrid recall@10 (no MMR) | 0.955 | 0.936 |
| Default search (hybrid + MMR) recall@10 | 0.890 | 0.842 |
| Default search latency p50 / p95 | 45 / 67 ms | 108 / 157 ms |

At standard scale, the hybrid recall@10 (0.936) is from the re-run after the chunker fix described
below; the other standard-scale figures are from the run before it (hybrid recall@10 was 0.922 there),
because the re-run published only the gate metrics, freshness and propagation.

On the hand-labelled eval set (57 documents, 42 queries), hybrid search reaches recall@10 1.000 and
nDCG@10 0.913, against 0.905 and 0.810 for full-text alone.

### Ingest and resources

| | quick | standard |
|---|---|---|
| Initial ingest | 2 000 files in 7.9 s (255 files/s) | 5 000 files in 25.5 s (196 files/s) |
| Database on disk after ingest, per 1 000 items (file, WAL and SHM) | 7.5 MB | 6.2 MB |
| Peak memory of the whole benchmark process | 37 MiB | 57 MiB |

## Reading these honestly

- **The recall gate is missed at standard scale**, by one query in 500. The first cause found, a
  chunker that repeated each Markdown title, was fixed in `nugget-extract` and recovered most of the
  gap. Ranking was **not** tuned further against this synthetic corpus, because its hash embedder has
  no semantic signal and tuning to it would overfit. The gap is tracked for a re-run with a real
  embedding model.
- **The default search path costs recall.** MMR (diversity) lowers known-item recall@10 to 0.842 at
  standard scale. Known-item queries are where diversity helps least; on the eval set MMR loses nothing.
  It is reported, not gated.
- **Query latency grows with the corpus**: vector search is a full scan, then MMR. Fine at 5 000
  items; it needs watching at 50 000.
- **Two retrieval fixes came from this benchmark**: fusion depth now follows the requested result count
  (deep fusion cost about 8 points of recall@10), and the store gained a weighted title index.
- **Ingest throughput here excludes a real model.** With a real embedding model, ingest will be bound
  by the model.

## Not measured yet

- Time to first useful result on a fresh install (target: under 2 minutes).
- Retrieval quality with a real embedding model (`--embed-url` exists; no published run yet).
- Mail and calendar connectors end to end, through the daemon and IPC.
- Citation accuracy (does each `[n]` support its sentence?).
- CPU, battery and idle cost per 1 000 items, and daemon idle overhead.
- Published `knowledge`, `draft`, `cua` and `proactive` results: the suites run in CI at smoke
  scale, but no release-build run is published here yet, and `knowledge` and `draft` use fake models
  by default.
- Nugget's own computer-use agent and the real proactive engine on their suites (being wired in
  wave W).
