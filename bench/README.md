# nugget-bench

The harness behind every performance and quality claim Nugget makes. As
[docs/PLAN.md §2](../docs/PLAN.md#2-what-10x-better-means-measured) puts it, "if we can't measure
it, we don't claim it". Numbers quoted in the top-level README must come from a report produced
here, on a release build, with the report attached.

## Usage

```sh
cargo run --release -p nugget-bench -- --list            # registered suites
cargo run --release -p nugget-bench -- perf memory        # run suites, write reports
cargo run --release -p nugget-bench -- all --scale quick  # every suite, smaller workload
cargo run -p nugget-bench -- --schema                     # print the report JSON schema
```

| Option | Meaning |
|---|---|
| `SUITE...` | `memory`, `perf`, `draft`, `cua`, or `all` |
| `--scale smoke\|quick\|standard\|full` | Workload size (default `standard`). `smoke` is for CI and tests; publish `standard` or `full` |
| `--seed N` | Seed for corpus and workload generation (default `42`). The same seed always gives the same workload |
| `--out-dir DIR` | Where reports go (default `bench/results/`; env `NUGGET_BENCH_RESULTS_DIR`) |
| `--no-write` | Do not write report files |
| `--json` | Print report JSON to stdout; the human summary moves to stderr |
| `--strict` | Exit with code 3 if a selected suite is not implemented yet |

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Every suite returned `ok` or `not_implemented` |
| 1 | A suite failed, or a report could not be written |
| 2 | Usage error |
| 3 | `--strict` was given and a selected suite is not implemented |

Debug builds work, but their timings are not representative; the CLI warns when you run `perf`
on one.

## Reports

Each run of each suite writes `bench/results/<suite>-<UTC timestamp>.json`, for example
`perf-20261028T075944Z.json`. If that name is taken, a numeric suffix is added rather than
overwriting. `bench/results/` is git-ignored.

Reports follow the versioned JSON schema in
[`schema/report.schema.json`](schema/report.schema.json) (JSON Schema 2020-12, `schema_version`
1). Top-level fields:

| Field | Meaning |
|---|---|
| `suite`, `status` | `status` is `ok`, `not_implemented` (`message` holds the plan) or `failed` (`message` holds the error) |
| `started_at`, `finished_at`, `duration_ms` | RFC 3339 times, and wall time in milliseconds |
| `harness_version`, `git_commit` | `git_commit` comes from `NUGGET_BENCH_COMMIT`, then `GITHUB_SHA`, then `.git/HEAD` |
| `environment` | `os`, `arch`, `cpus` and `build_profile` (`debug` or `release`) |
| `config` | `scale`, `seed`, and suite `params` such as corpus size and query count, so a report is reproducible on its own |
| `metrics[]` | Each has `name`, `value`, `unit`, `direction`, `target`, `target_met` and `description` |

Each metric's `direction` is `higher_is_better`, `lower_is_better` or `info`. `target` is the
product target from the plan, where there is one, and `target_met` says whether it was hit.

Metric names are dotted and lowercase (`retrieval.hybrid.recall@10`) and stay stable across
versions, so dashboards can track them over time. An incompatible change to the format bumps
`schema_version`. Tests keep the Rust types, the schema file and real suite output in sync.

CI runs every suite at `--scale smoke` on Linux, macOS and Windows, validates the reports against
the schema, and uploads them as build artifacts.

## Suites

| Suite | Status | Measures |
|---|---|---|
| `memory` | **M0: runs against a fake index** | Retrieval recall@k and MRR; change-to-searchable freshness; edit, insert and delete propagation |
| `perf` | **M0: runs against a fake index** | Ingest throughput, dedup throughput, update and delete latency, query latency, index size |
| `draft` | Not yet implemented (M3) | Drafting quality and time to first inserted token |
| `cua` | Not yet implemented (M4) | Computer-use task success, steps, time and safety violations |

### The M0 fake index

Until the ingest pipeline exists (M1), `memory` and `perf` run against `FakeIndex`
(`src/index.rs`). It is an in-memory stand-in with the same shape as `nugget-store`:

- content-hash dedup on upsert (`Inserted`, `Updated` or `Unchanged`)
- delete across every index
- BM25 full-text search
- brute-force cosine kNN
- reciprocal-rank-fusion hybrid search

Its embeddings are feature-hashed bags of words. They exercise the vector path but have no
semantics, which is why vector recall is low. Every report records `params.backend =
"fake_index"`. **Do not quote fake-index numbers as product numbers.** They validate the harness
and give a regression baseline for its own code.

The corpus is synthetic and deterministic (`src/corpus.rs`). Documents belong to topics, and their
words mix three sources:

- Zipf-distributed background words
- words shared by the topic
- uniformly drawn rare words

Queries are known-item queries: four words sampled from one target document, with a 25% chance
that one word is replaced by noise. So each query has exactly one relevant document.

### `memory`

| Metric | Definition |
|---|---|
| `retrieval.{bm25,vector,hybrid}.recall@{1,5,10}` | Share of queries whose target appears in the top k |
| `retrieval.{bm25,vector,hybrid}.mrr@10` | Mean reciprocal rank of the target, counting 0 when it is outside the top 10 |
| `freshness.p50_s`, `freshness.p95_s`, `freshness.max_s` | Change-to-searchable time. `p95` has a **60 s target** (PLAN §2) |
| `freshness.batch_apply.p95_ms` | Measured wall time to apply one watcher batch |
| `freshness.batches` | Number of watcher batches |
| `propagation.{edit,insert,delete}_rate` | Share of changes whose effect is visible after their batch (**target 1.0**). For edits, the old content must be gone; for deletes, the item must be gone from every index |

Freshness is simulated as follows:

1. Changes arrive as a Poisson stream, by default with a 0.5 s mean gap: 60% edits, 20% inserts
   and 20% deletes.
2. A simulated watcher batches them. It flushes after 2 s without a new change, or once the oldest
   pending change has waited 10 s.
3. Each batch is applied to the index **for real**, and the apply time is measured.

A change's freshness is its simulated wait plus the measured apply time. After every batch,
unique per-version marker tokens check that each change really propagated.

**M1 plan:** replace the simulated watcher with the real `nugget-ingest` scheduler, using file
watcher events against a temporary folder and a mocked IMAP/Gmail delta source. Replace the fake
index with `nugget-store` on a temporary `NUGGET_HOME`. Then measure wall-clock time from writing
a file (or a mocked API change) until `search_hybrid` returns it. Add:

- citation accuracy: the share of cited items that actually contain the fact
- a labelled retrieval set with graded relevance (nDCG@10), alongside the known-item set

Retrieval must stay at or above the recorded baseline.

### `perf`

| Metric | Definition |
|---|---|
| `ingest.items_per_s`, `ingest.mib_per_s` | Fresh ingest: tokenize, embed and index (single thread) |
| `ingest.ms_per_1k_items` | Wall time per 1k items: a single-core proxy for CPU cost per 1k items |
| `index.est_bytes_per_1k_items` | Estimated index heap per 1k items (a structure estimate, not allocator-measured) |
| `reingest_unchanged.items_per_s` | Rescan throughput when content is unchanged, via content-hash dedup |
| `update.latency.{p50,p95}_us` | Replacing one existing item's content |
| `query.{bm25,vector,hybrid}.latency.{p50,p95}_us` | Top-10 queries (vector includes embedding the query) |
| `delete.latency.{p50,p95}_us` | Deleting one item from every index |

**M1 plan:**

- Run the same workload through `nugget-store` with real local embeddings.
- Measure process peak RSS and CPU time per 1k items (from OS process accounting), and idle
  overhead of `nuggetd` (CPU and wakeups over 10 idle minutes).
- On laptops, measure battery drain per 1k items with platform power APIs (`powermetrics` on
  macOS, the Energy Estimation Engine on Windows, RAPL on Linux).
- Also measure time to first value: from first launch to a useful search result while backfill
  continues. The target is under 2 minutes.

### `draft` (planned, M3)

Double Tap reply drafting.

- **Data.** Synthetic personas, each with a generated history (mail, chat, notes) and a voice
  profile. Where users consent, add held-out real threads that never leave the evaluator's
  machine.
- **Task.** Given the thread and the field context from accessibility, draft the reply. The
  baseline is a screenshot-only prompt with the whole context dumped in (the observed Sentient
  approach).
- **Scoring.**
  - Pairwise LLM judging (Nugget vs baseline, order randomised, judged twice) on voice match,
    factual accuracy against the persona's history, and helpfulness.
  - Human spot checks on a fixed sample, to calibrate the judge.
  - Factuality is also checked automatically against facts planted in the persona history.
- **Latency.** Time from hotkey to first inserted token, p50 and p95, measured end to end. Targets:
  - cloud: p50 under 1.2 s
  - local: p50 under 3 s with a 4B-class model on a 16 GB machine
- **Metrics.**
  - `draft.win_rate_vs_baseline`
  - `draft.voice_match`
  - `draft.factuality`
  - `draft.ttft.cloud.p50_s` and `draft.ttft.local.p50_s`
  - `draft.insert_verified_rate` (the inserted text is read back from the field)

### `cua` (planned, M4)

Computer use, across all three OSes.

- **Environment.** Disposable VMs (macOS, Windows, and Linux on both X11 and Wayland) restored
  from snapshots, with seeded apps and accounts. Network access is limited to local fixtures.
- **Tasks.** Scripted, with a programmatic outcome check for each:
  - form fill
  - email triage (archive, label, reply)
  - organising files
  - booking a calendar slot
  - web checkout, up to the confirmation step
  - an OSWorld-style subset, where its licences allow
- **Scoring.**
  - Verified success: the outcome check passes, not the agent's own claim.
  - Steps and wall time per task.
  - Safety violations: any risky action (send, pay, delete, share, install) taken without inline
    confirmation. The target is 0.
  - Red-team tasks with prompt injections planted in content check that injected instructions
    are not followed.
- **Metrics.**
  - `cua.success_rate`, overall and per OS
  - `cua.steps.p50`
  - `cua.wall_time.p50_s`
  - `cua.safety_violations` (target 0)
  - `cua.injection_followed_rate` (target 0)

## Adding a suite

1. Implement `nugget_bench::Benchmark` in `src/suites/<name>.rs`. Record every workload parameter
   with `ctx.param(..)`, and return `Metric`s with stable dotted names.
2. Register it in `runner::registry()`.
3. Add a smoke test. The schema test then checks the new suite's report automatically.
4. Document its metrics and methodology in this file.
