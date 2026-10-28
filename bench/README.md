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
cargo run --release -p nugget-bench -- memory --backend real --strict  # the M1 gate
cargo run -p nugget-bench -- --schema                     # print the report JSON schema
```

| Option | Meaning |
|---|---|
| `SUITE...` | `memory`, `perf`, `draft`, `cua`, or `all` |
| `--scale smoke\|quick\|standard\|full` | Workload size (default `standard`). `smoke` is for CI and tests; publish `standard` or `full` |
| `--seed N` | Seed for corpus and workload generation (default `42`). The same seed always gives the same workload |
| `--backend fake\|real` | What `memory` runs against (default `fake`): the in-crate fake index, or the real store, ingest pipeline, files watcher and retriever in process. Other suites ignore it |
| `--embed-url URL` | Real backend: an OpenAI-compatible embeddings endpoint (e.g. a local `llama-server`, `http://127.0.0.1:8080/v1`) instead of the in-process deterministic embedder |
| `--embed-model NAME` | Model name sent to `--embed-url` |
| `--out-dir DIR` | Where reports go (default `bench/results/`; env `NUGGET_BENCH_RESULTS_DIR`) |
| `--no-write` | Do not write report files |
| `--json` | Print report JSON to stdout; the human summary moves to stderr |
| `--strict` | Exit with code 4 if a metric misses its target, or 3 if a selected suite is not implemented yet |

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Every suite returned `ok` or `not_implemented` |
| 1 | A suite failed, or a report could not be written |
| 2 | Usage error |
| 3 | `--strict` was given and a selected suite is not implemented |
| 4 | `--strict` was given and a metric missed its target (takes precedence over 3) |

Debug builds work, but their timings are not representative; the CLI warns when you run `perf`
or `memory --backend real` on one.

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
| `targets[]` | Optional (added for M1). One pass/fail entry per metric with a target: `metric`, `comparison` (`at_least` or `at_most`), `target`, `value`, `pass`. Omitted when no metric has a target |

Each metric's `direction` is `higher_is_better`, `lower_is_better` or `info`. `target` is the
product target from the plan, where there is one, and `target_met` says whether it was hit.
`targets[]` repeats those verdicts as a flat list, so a dashboard or CI check can read pass/fail
without knowing metric names. `targets` is an optional, additive field: `schema_version` stays 1
and every report written before it still validates against the current schema.

Metric names are dotted and lowercase (`retrieval.hybrid.recall@10`) and stay stable across
versions, so dashboards can track them over time. An incompatible change to the format bumps
`schema_version`. Tests keep the Rust types, the schema file and real suite output in sync.

CI runs every suite at `--scale smoke` on Linux, macOS and Windows, plus
`memory --backend real --scale smoke` on Linux (about 15 s on a debug build), validates the
reports against the schema, and uploads them as build artifacts. The real smoke run fails the job
if it cannot complete. It does not use `--strict`, because at 50 queries the recall gate can
exactly tie its baseline, and score ties break on chunk ids, which follow the file system's
directory order. The Linux test `real_memory_backend_smoke` (in `tests/schema.rs`) asserts the
timing-robust gates instead: every file operation propagates, and freshness meets its target.

## Suites

| Suite | Status | Measures |
|---|---|---|
| `memory` | **M1: fake index (default) or the real stack (`--backend real`)** | Retrieval recall@k and MRR; change-to-searchable freshness; edit, insert, delete and rename propagation; ingest throughput; DB size and RSS (real) |
| `perf` | **M0: runs against a fake index** | Ingest throughput, dedup throughput, update and delete latency, query latency, index size |
| `draft` | Not yet implemented (M3) | Drafting quality and time to first inserted token |
| `cua` | Not yet implemented (M4) | Computer-use task success, steps, time and safety violations |

### The M0 fake index

By default (and always for `perf`), `memory` and `perf` run against `FakeIndex`
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

### `memory` (fake backend, the default)

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

### `memory --backend real`

The real stack, in process (`src/real.rs`). Nothing is simulated except the user:

- an encrypted `nugget-store` (SQLCipher) in a temporary `NUGGET_HOME`;
- `nugget-ingest` with the `files` connector (registered through
  `nugget_connector_files::register`, default settings: native watcher, 300 ms debounce) watching
  a temporary folder;
- `nugget-retrieve` (`RetrieveConfig::default()`) for every query;
- embeddings from `nugget_retrieve::eval::HashEmbedder`, an in-process deterministic
  bag-of-words embedder (256 dimensions, on-device locality), routed through a `Router` as the
  `Embedding` tier, both for ingest and for queries. `--embed-url` swaps in an OpenAI-compatible
  server (it must be on-device or LAN: ingest never sends content to a cloud embedder by default).

It runs in phases:

1. **Eval set.** `nugget-retrieve`'s labelled fixtures (57 documents, 42 queries, graded by hand
   with binary relevance) are written as Markdown files (`# Title`, then the text), ingested
   through the pipeline, and queried text-only, hybrid and hybrid+MMR. The same set indexed
   directly into a store (one chunk per paragraph, as the `nugget-retrieve` eval test does) is
   reported alongside as `baseline.eval_direct.*`.
2. **Initial ingest.** The fake backend's synthetic corpus (same scale and seed, so the same
   documents) is written as Markdown files in folders of 250. The source is then added, and the
   clock runs until every file is indexed with its version (which the pipeline writes last).
3. **Known-item retrieval** over that corpus, with the same queries as the fake backend. The
   fake index is rebuilt on the identical corpus in the same run to give the baseline.
4. **Freshness.** File operations hit the watched folder at exponentially distributed gaps:
   50% edits, 20% creates, 15% deletes and 15% renames, never two pending on the same file. For
   each one the clock starts when the file system call returns and stops when
   `Retriever::search` reflects it completely. Pending operations are probed every 20 ms (each
   probe is one to three real searches). An operation not reflected within 120 s counts as a
   failure, at 120 s.
   - *create*: the new file's marker finds it;
   - *edit*: the new marker finds it, and the old marker no longer does (full-text);
   - *delete*: the item is gone from the store and the old marker no longer finds it;
   - *rename*: the marker finds the new path, and the old path's item is gone.
   
   Afterwards the store's keys are compared with the files on disk, and every changed file is
   searched once more.
5. **Resources.** Database size on disk, and the process's peak RSS (Linux only, from
   `/proc/self/status` `VmHWM`; it covers the whole benchmark process, harness included).

| Metric | Definition |
|---|---|
| `retrieval.eval.{text_only,hybrid,hybrid_mmr}.{recall@1,recall@5,recall@10,mrr@10,ndcg@10}` | Eval set through the pipeline. Hybrid recall@10 has a **target: at least text-only's** (fusion must not lose what full-text finds) |
| `baseline.eval_direct.{text_only,hybrid,hybrid_mmr}.{recall@10,mrr@10}` | The same set indexed directly, for reference (`info`) |
| `ingest.wall_s`, `ingest.files_per_s`, `ingest.chunks_per_s`, `ingest.mib_per_s`, `ingest.chunks` | Initial ingest: scan, extract, chunk, embed and store, single source |
| `retrieval.{bm25,vector,hybrid,hybrid_mmr}.{recall@1,recall@5,recall@10,mrr@10}` | Known-item queries through `Retriever::search_with` (text-only, vector-only, hybrid without MMR, and the default hybrid with MMR). `retrieval.hybrid.recall@10` has the **M1 target: at least the fake index's hybrid recall@10** on the same corpus and queries |
| `retrieval.{hybrid,hybrid_mmr}.text_only_fallbacks` | Queries the retriever had to answer text-only (should be 0) |
| `baseline.fake.retrieval.{bm25,vector,hybrid}.{recall@10,mrr@10}` | The fake index on the identical corpus and queries (`info`) |
| `query.latency.{p50,p95}_ms` | `Retriever::search` with the default configuration (hybrid + MMR, query embedding included) |
| `freshness.{p50,p95,max}_s` | File operation to searchable. `p95` has the **M1 target: at most 60 s** |
| `freshness.{edit,insert,delete,rename}.p95_s`, `freshness.changes`, `freshness.timeouts` | Per kind, and counts |
| `freshness.ingest_reported.p95_s` | Ingest's own measurement (change reaching the pipeline to committed), from `Ingest::status` |
| `propagation.{edit,insert,delete,rename}_rate` | Share of operations fully reflected within the timeout (**target 1.0**) |
| `propagation.final_keys_match`, `propagation.final_content_match` | After all operations: Jaccard overlap of indexed keys and files on disk, and share of changed files whose current content finds them (**target 1.0**) |
| `resources.db_bytes_per_1k_items`, `resources.db_bytes` | Encrypted database file plus WAL and SHM, after the initial ingest (per 1k items) and at the end |
| `resources.peak_rss_mib` | Peak RSS of the benchmark process (Linux) |

Why the recall gate is on `retrieval.hybrid` and not `retrieval.hybrid_mmr`: the fake index's
hybrid search has no MMR, so `hybrid` is the like-for-like comparison. MMR trades a little
known-item recall for diversity; its numbers are reported next to it and in the results below.

What the real backend does not cover yet: mail and calendar connectors (a mocked IMAP or Gmail
delta source), the daemon and IPC (`nuggetd`), a real embedding model by default (use
`--embed-url`), citation accuracy, and graded relevance beyond the binary eval set.

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

**Plan:**

- Run the same workload through `nugget-store` with real local embeddings (`memory --backend
  real` already measures real ingest throughput, DB size and peak RSS).
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

## Results

### `memory --backend real`, M1 (2026-10-28)

**Machine.** A cloud VM with 4 vCPUs (Intel Xeon @ 2.80 GHz) and 15 GiB RAM, running Ubuntu
24.04.5 on Linux 6.18. Temporary folders were on ext4. **Build.** Release profile (thin LTO, one
codegen unit) with rustc 1.97.0, except the smoke row, which is the debug build CI runs.
**Workload.** Seed 42, the in-process `HashEmbedder` (256 dimensions), and every setting at its
default (300 ms watcher debounce, default chunking, `RetrieveConfig::default()`). Each run took
12 s (smoke), 90 s (quick) and 329 s (standard).

M1 targets:

| Target | smoke (debug) | quick | standard |
|---|---|---|---|
| `freshness.p95_s` ≤ 60 s | 0.44 s **pass** | 0.45 s **pass** | 0.60 s **pass** |
| `retrieval.hybrid.recall@10` ≥ fake-index hybrid recall@10, same corpus and queries | 0.880 vs 0.880 **pass** (tie) | 0.955 vs 0.930 **pass** | 0.922 vs 0.938 **MISSED** (−0.016, 8 of 500 queries) |
| `propagation.{edit,insert,delete,rename}_rate` = 1, final keys and content match = 1 | all 1.0 **pass** | all 1.0 **pass** | all 1.0 **pass** |
| `retrieval.eval.hybrid.recall@10` ≥ eval text-only recall@10 | 1.000 vs 0.905 **pass** | same | same |

Freshness, from the file operation to `Retriever::search` reflecting it:

| | smoke | quick | standard |
|---|---|---|---|
| operations (edit / create / delete / rename) | 16 | 100 | 300 |
| p50 / p95 / max | 0.37 / 0.44 / 0.44 s | 0.39 / 0.45 / 0.53 s | 0.48 / 0.60 / 0.72 s |
| p95 per kind: edit, insert, delete, rename | 0.40, 0.41, 0.38, 0.44 s | 0.45, 0.45, 0.45, 0.46 s | 0.60, 0.58, 0.50, 0.65 s |
| timeouts (120 s) | 0 | 0 | 0 |
| ingest's own p95 (pipeline only) | 0.45 s | 0.38 s | 0.54 s |

Most of the half second is the watcher's 300 ms debounce. The 60 s target has about a 100×
margin on a local folder.

Known-item retrieval on the synthetic corpus (R@k = recall@k):

| | quick (2 000 docs, 200 queries) | standard (5 000 docs, 500 queries) |
|---|---|---|
| bm25: R@1 / R@10 / MRR@10 | 0.900 / 0.980 / 0.930 (fake: R@10 0.980, MRR 0.941) | 0.830 / 0.966 / 0.880 (fake: 0.972, 0.893) |
| vector: R@10 / MRR@10 | 0.415 / 0.236 (fake: 0.360, 0.196) | 0.322 / 0.160 (fake: 0.276, 0.141) |
| hybrid: R@1 / R@5 / R@10 / MRR@10 | 0.455 / 0.810 / 0.955 / 0.601 (fake R@10 0.930, MRR 0.531) | 0.400 / 0.800 / 0.922 / 0.561 (fake 0.938, 0.531) |
| hybrid + MMR, the default: R@5 / R@10 / MRR@10 | 0.720 / 0.890 / 0.576 | 0.686 / 0.842 / 0.521 |
| query latency (default search) p50 / p95 | 45 / 67 ms | 108 / 157 ms |

Eval set through the pipeline (57 docs, 42 queries; the same at every scale):

| mode | R@1 | R@5 | R@10 | MRR@10 | nDCG@10 |
|---|---|---|---|---|---|
| text-only | 0.619 | 0.893 | 0.905 | 0.785 | 0.810 |
| hybrid | 0.714 | 0.964 | 1.000 | 0.889 | 0.913 |
| hybrid + MMR | 0.714 | 0.964 | 1.000 | 0.880 | 0.901 |
| *direct index, for reference: text-only / hybrid R@10* | | | *0.881 / 0.988* | *0.679 / 0.792* | |

Ingest and resources:

| | smoke (debug) | quick | standard |
|---|---|---|---|
| initial ingest | 300 files in 1.9 s: 158 files/s | 2 000 files in 7.9 s: 255 files/s | 5 000 files in 25.5 s: 196 files/s |
| chunks/s (one chunk per file here) | 158 | 255 | 196 |
| DB on disk after ingest, per 1k items (file + WAL + SHM) | 22.1 MB | 7.5 MB | 6.2 MB |
| DB on disk at the end | 6.6 MB | 15.6 MB | 32.8 MB |
| peak RSS of the whole process | 46 MiB | 37 MiB | 57 MiB |

**Reading these honestly.**

- **The recall gate is missed at standard scale by 1.6 points.** The cause was measured, not
  guessed. The pipeline's chunks start with the Markdown H1 twice: `nugget-extract`'s chunker
  prepends the heading path to a chunk that already begins with that heading
  (`"Title\n# Title\n\n…"`). The synthetic titles are random topic words, so this doubles
  their weight. Indexing the same 5 000 documents directly with the title once gives text R@10
  0.968 and hybrid R@10 0.936, level with the fake's 0.938. Indexing them in the pipeline's form
  reproduces the real run's 0.966 and 0.922 exactly. The fix belongs in `nugget-extract` (don't
  repeat a heading the chunk already starts with). The corpus was not changed to hide it.
- **Two retrieval fixes came out of this benchmark.** The first run at quick scale had hybrid
  R@10 at 0.855, below the fake's 0.930. The retriever over-fetched candidates (8×) and the store
  over-fetched again (4×) for rank fusion, so each list fed 320 hits into RRF for 10 results.
  Deep lists let chunks that are mediocre in both lists outrank a strong match from one list. On
  the 5 000-doc corpus, depth 40 / 80 / 320 gives hybrid R@10 0.95 / 0.855 / 0.87. The fusion
  depth now follows the result count (`RetrieveConfig::fusion_depth_factor`, `min_fusion_depth`;
  `Store::search_hybrid_with_depth`). The second fix is the title index from the store
  upgrades, whose weight was measured on both corpora (see `nugget-store`'s `TITLE_WEIGHT`).
- **MMR costs known-item recall.** The default search path (hybrid + MMR) is below the fake
  baseline at every scale (0.890 at quick, 0.842 at standard). It also misses at smoke (0.86 vs
  0.88). Known-item queries are where diversity helps least. On the eval set, MMR loses nothing
  at R@10. It is reported, not gated, because the fake has no MMR.
- **The embedder is a hashing bag of words.** Vector recall is low by construction, and
  `--embed-url` with a real model will change every hybrid number. Ingest throughput there will
  be bound by the model, not by the pipeline measured here.
- **Query latency grows with the corpus** (sqlite-vec's kNN is a full scan, then MMR over
  stored vectors). It is fine at 5 000 items, but worth watching at 50 000.
- **The DB size per 1k items is inflated on small corpora** by fixed overhead (sqlite-vec
  allocates vectors in blocks of 1024) and by the WAL.

## Adding a suite

1. Implement `nugget_bench::Benchmark` in `src/suites/<name>.rs`. Record every workload parameter
   with `ctx.param(..)`, and return `Metric`s with stable dotted names.
2. Register it in `runner::registry()`.
3. Add a smoke test. The schema test then checks the new suite's report automatically.
4. Document its metrics and methodology in this file.
