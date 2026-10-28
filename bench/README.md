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
| `SUITE...` | `memory`, `perf`, `proactive`, `draft`, `cua`, or `all` |
| `--scale smoke\|quick\|standard\|full` | Workload size (default `standard`). `smoke` is for CI and tests; publish `standard` or `full` |
| `--seed N` | Seed for corpus and workload generation (default `42`). The same seed always gives the same workload |
| `--backend fake\|real` | What `memory` runs against (default `fake`): the in-crate fake index, or the real store, ingest pipeline, files watcher and retriever in process. For `proactive`: the in-crate reference engine (`fake`) or the real `nugget-proactive` crate (`real`, not wired until wave 2). Other suites ignore it |
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
| `proactive` | **M5: reference engine (default); the real crate in wave 2** | Card precision@5 and recall, distractor false positives, dedup, the learning curve, dry-run coverage and the approval gate, against a recency-ordered baseline |
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

### `proactive` (M5)

Proactive cards and the morning brief. PLAN M5 asks that **the precision of accepted cards be
tracked and every action have a dry-run diff**. Code: `src/suites/proactive.rs` and
`src/suites/proactive/`.

**The engine under test.** The suite drives any implementation of the `ProactiveEngine` adapter
trait (`src/suites/proactive/engine.rs`):

- `observe(item)`;
- `brief(day, k)`, which returns at most k cards, each with a kind, a subject and an optional
  typed action carrying a dry-run diff (`target`, `before`, `after`);
- `feedback(card, accepted | dismissed)`;
- `execute(card, approval)`;
- `audit_log()`.

`nugget-proactive` is still a stub, so the default (`--backend fake`) scores the in-crate
**reference engine** (`reference.rs`). It follows the design in PLAN §3:

- a deterministic candidate store with dedup on `(kind, subject)`;
- filters for header-flagged bulk mail, for billing mail from merchants with no receipt, and for
  cancelled events;
- a fixed prior plus feedback-learned weights (logistic SGD on sender, kind, internal or recurring
  meeting, and renewal cadence);
- a dry-run diff on every action, and execution only under an approval for that exact card.

`--backend real` reports `not_implemented` until wave 2 wires the real crate.

**Fixture world** (`bench/fixtures/proactive/`). One fictional user, Sam Rivera
(`sam@example.com`), over 30 days from 2026-09-01. Every address is on `example.com` or a `.test`
domain.

- `world.json`: 312 mails, 40 calendar events, 5 cancellations, 14 notes and 20 receipts, one item
  per line. Items with a day ≤ 0 are history the engine has seen before day 1.
- `gold.json`: what engines never see.
  - **42 gold cards**, each `{kind, subject, first_day, last_day, source, why}`:
    - 13 unanswered threads (due from 2 days after the question, for 6 days);
    - 8 follow-ups (the user's unanswered question; due from 4 days after it, for 6 days);
    - 8 commitments (`I'll send X by <date>` in mail, or `due <date>` in notes; due the 2 days
      before and the day itself);
    - 8 external meetings to prepare (the day before and the day itself);
    - 5 annual renewals (the 7 days before renewal).
  - **191 distractors**, each with the subjects that would make it a false positive:
    - 71 newsletters (`List-Unsubscribe`; some ask questions or mention renewing);
    - 46 automated notifications (`Auto-Submitted` or no-reply senders);
    - 24 unflagged bulk mails (four senders that look personal and ask questions, about weekly);
    - 29 answered threads;
    - 4 completed commitments;
    - 5 cancelled meetings (cancelled two days before they start);
    - 12 spam renewal and charge notices from merchants the user never paid.
  - Neutral traffic is neither gold nor distractor: FYI mail, routine internal meetings, social
    plans, monthly subscriptions the user does not want reminders for, and one-off receipts.

Card subjects are `thread:<id>`, `event:<id>`, `commit:<mail or note id>` or
`sub:<merchant domain>`. A shown card matches a gold label when its kind and subject are equal and
the day is inside the label's window. The fixture is built by `generate.rs`: gold items are written
out by hand, and distractors and neutral traffic come from templates. A test fails if the committed
JSON drifts from the generator; `NUGGET_BENCH_REGEN=1 cargo test -p nugget-bench --lib
committed_fixture` rewrites it. Other tests check that:

- every subject resolves;
- every window lies inside days 1 to 30, and its source is visible before the window opens;
- no subject is both gold and a distractor;
- only reserved domains are used;
- an oracle engine scores precision and recall of 1.0 at k = 5.

**Simulation** (`sim.rs`). On each morning d the engine has observed every item with `day < d`
and returns its brief (k = 5).

- **Feedback.** The simulated user accepts a card that matches gold and dismisses anything else,
  and each decision is flipped with probability 0.1. That noise is what the engine has to learn
  through.
- **Execution.** An accepted card's action is executed with a fresh `Approval{card_id, token}`.
- **Approval probes.** After day 30 the benchmark asks the engine to execute:
  - every dismissed card, with no approval;
  - every dismissed card, with another card's approval;
  - every executed card again, with its own approval replayed.

  All of these must be refused.
- **Audit log.** Every logged execution must carry a token the benchmark issued for that card,
  and no card may execute twice.
- **Runs.** The run repeats over several user seeds (`--scale` smoke/quick/standard/full gives
  3/10/30/100), derived from `--seed`, and every metric is the mean over seeds.

**Baseline arm: Sentient-style recency judge** (`baseline.rs`). This models the teardown (§2.8):
a judge reads the last 7 days plus the calendar and returns up to 8 items, at most 5 of them become
prepared actions, and ranking is purely the LLM's ordering. Here the judge is a deterministic fake
that **orders by recency only**. It is given the parts an LLM gets right for free:

- it skips header-flagged newsletters and notifications, and cancelled events;
- it types each item with a simple mapping:
  - inbound mail becomes a reply card, or a renewal card if it mentions renewing;
  - the user's own mail becomes a follow-up, or a commitment if it promises something by a date;
  - an event in the next 7 days becomes meeting prep, entering the context a week ahead;
  - a note becomes a commitment;
  - a receipt becomes a renewal.

It has no candidate store, so it keeps no memory across days, and it ignores feedback. Its dry
run is not scored: the teardown does not describe a preview, and the arm exists to compare
ranking.

**Learning ablation.** The same reference engine with learning off (weights frozen at the prior)
runs on the same seeds. Its days 21 to 30 isolate what feedback buys. Comparing days 1 to 10 with
days 21 to 30 alone would mix learning with the days simply being different.

| Metric | Definition | Target |
|---|---|---|
| `cards.precision@5` | Mean over days with at least one card of (gold-matching cards) / (cards shown). An engine may show fewer than 5 | ≥ 0.7 |
| `cards.precision@5.micro` | All gold-matching shows / all shows | info |
| `cards.recall`, `cards.recall.<kind>` | Gold cards shown at least once inside their window | ≥ 0.8 |
| `cards.acceptance_rate` | Accepted / shown: the "precision of accepted cards" the product can track live without labels | info |
| `cards.accepted_precision` | Accepted cards that are gold / accepted (the rest is user noise) | info |
| `cards.shown_per_day` | Mean brief size | info |
| `cards.duplicate_shows` | Shows of a `(kind, subject)` already shown, per run | = 0 |
| `distractors.false_positive_rate` | Distractors that produced any card / all distractors | ≤ 0.05 |
| `distractors.header_flagged.false_positives` | Newsletters and notifications that produced a card (no feedback should be needed) | = 0 |
| `distractors.<category>.fpr` | Per category | info |
| `learning.precision.days_01_10`, `learning.precision.days_21_30` | The learning curve | info |
| `learning.precision_gain` | Days 21 to 30 minus days 1 to 10 | ≥ 0 |
| `learning.gain_vs_frozen` | Days 21 to 30, learning on minus learning off | ≥ 0 |
| `actions.dry_run_coverage` | Shown actions with an effective dry-run diff / shown actions | = 1.0 |
| `actions.diff_fidelity` | Approved executions whose applied change equals the approved diff | = 1.0 |
| `actions.executed_without_approval` | Audit-log executions without a valid, single-use approval for that card, plus probe executions the engine accepted | = 0 |
| `actions.count`, `actions.executions`, `actions.approved_failures`, `actions.approval_probes` | Counts, per run | info |
| `baseline.recency_judge.{precision@5, recall, distractors.false_positive_rate, duplicate_shows, precision.days_21_30}` | The baseline arm | info |
| `comparison.precision@5_vs_baseline` | precision@5 minus the baseline's | ≥ 0 |
| `comparison.recall_vs_baseline` | Recall minus the baseline's | info |

**What this does not cover yet:**

- the LLM judge that enriches candidates (PLAN §3), and with it LLM-written card text;
- real connectors: the world is fed as typed items, not through `nugget-ingest`;
- the cards inbox UI and morning-brief rendering;
- a judged comparison against a real LLM-ordered baseline.

The fixture is one persona, so a high score here is necessary, not sufficient. Add personas
before quoting the numbers as product numbers.

**Wiring the real crate (wave 2).**

1. Add `nugget-proactive` to `bench/Cargo.toml`.
2. Implement `ProactiveEngine` for an adapter in `src/suites/proactive/` that:
   - maps `world::Item` into the crate's candidate inputs;
   - maps its cards to `(kind, subject)`;
   - forwards feedback;
   - executes through the crate's approval API;
   - returns its audit log.
3. Select the adapter in `ProactiveBench::run` when `ctx.backend() == Backend::Real`.

The scoring, the baseline and the targets stay as they are. Keep the reference engine as the
`fake` arm, so the real crate has a fixed bar to clear.

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
| `retrieval.hybrid.recall@10` ≥ fake-index hybrid recall@10, same corpus and queries | 0.880 vs 0.880 **pass** (tie) | 0.955 vs 0.930 **pass** | 0.936 vs 0.938 **MISSED** (−0.002, 1 of 500 queries; was 0.922 before the chunker fix) |
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
  **Update (after the chunker fix in `nugget-extract`, 2026-10-28):** re-run at standard scale,
  seed 42, release build: hybrid R@10 **0.936 vs 0.938**, still missed by 0.2 points (one query of
  500). Freshness p95 0.61 s, all propagation rates 1.0 and the eval-set gate (R@10 1.000 vs
  0.905 text-only) pass. Ranking was not tuned further against this synthetic corpus, because its
  hash embedder has no semantic signal and tuning to it would overfit; the gap is tracked for a
  re-run with a real embedding model (`--embed-url`).
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

### `proactive`, M5 reference engine (2026-10-28)

**Setup.** Release build. Seed 42, 30 user seeds (standard), user noise 0.1, k = 5. Same machine as
above. A run takes 0.13 s. The `full` scale (100 user seeds) agrees to within 0.015 on every
headline number.

| Metric | reference engine | learning off | recency judge (baseline) |
|---|---|---|---|
| precision@5 (target ≥ 0.7) | **0.763** | | 0.020 |
| recall (target ≥ 0.8) | **0.945** | | 0.071 |
| recall: unanswered / follow-up / commitment / meeting / renewal | 0.92 / 1.00 / 1.00 / 0.98 / 0.78 | | |
| distractor false-positive rate (target ≤ 0.05) | **0.028** | 0.120 | 0.262 |
| header-flagged false positives (target 0) | 0 | | |
| duplicate shows per run (target 0) | 0 | | 11 |
| precision, days 1 to 10 → days 21 to 30 | 0.553 → **0.996** | → 0.363 | → 0.000 |
| dry-run coverage / diff fidelity (target 1.0) | 1.0 / 1.0 | | |
| executed without approval (target 0) | 0, over 66 probes and 36.6 approved executions per run | | |
| acceptance rate / accepted precision | 0.713 / 0.966 | | |

Every target passes.

**Reading these honestly.**

- **The reference engine was written alongside the fixture.** Its rules (`I'll ... by <date>`,
  `due <date>`, header filters) match the fixture's phrasing. The safety gates are what transfers
  to the real crate: dedup, dry-run coverage, the approval gate, zero header-flagged false
  positives, and no lost precision over the month. Treat its precision and recall as the bar
  `nugget-proactive` has to clear on this world, not as product numbers.
- **Most of the early-days misses are learnable on purpose.** Unflagged bulk mail (22.5% of it
  produces a card, almost all from each sender's first message) and routine internal meetings
  produce cards until the first one or two dismissals.
- **Renewal recall is 0.78, and that is a real weakness.** A noisy dismissal of an annual renewal
  pushes the shared `renewal:annual` weight below zero. Nothing is shown in that class afterwards,
  so nothing can correct it: there is no exploration. Bounding per-class negative weights, or a
  small exploration rate, is the fix to try in the real crate.
- **The baseline is weak by construction, not by tuning.** Recency-only ordering puts FYI mail,
  bulk mail and replies already sent at the top of a 7-day context. Meetings enter that context a
  week ahead, so they are pushed out by fresher mail. With no memory, the same items reappear.
  This measures **ordering alone**. A real LLM judge reads content and will do much better. A
  judged comparison is listed above under what this does not cover.

## Adding a suite

1. Implement `nugget_bench::Benchmark` in `src/suites/<name>.rs`. Record every workload parameter
   with `ctx.param(..)`, and return `Metric`s with stable dotted names.
2. Register it in `runner::registry()`.
3. Add a smoke test. The schema test then checks the new suite's report automatically.
4. Document its metrics and methodology in this file.
