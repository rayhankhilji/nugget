# nugget-bench

The harness behind every performance and quality claim Nugget makes ("if we can't measure it, we
don't claim it", [docs/PLAN.md §2](../docs/PLAN.md#2-what-10x-better-means-measured)). Numbers in
the top-level README must come from a report produced here, on a release build.

## Usage

```sh
cargo run --release -p nugget-bench -- --list                          # registered suites
cargo run --release -p nugget-bench -- all --scale quick               # every suite, smaller workload
cargo run --release -p nugget-bench -- memory --backend real --strict  # the M1 gate
cargo run --release -p nugget-bench -- knowledge --strict              # the M2 gates
cargo run --release -p nugget-bench -- doubletap --local-models --scale quick
```

| Option | Meaning |
|---|---|
| `SUITE...` | `memory`, `perf`, `proactive`, `draft`, `doubletap`, `cua`, `knowledge`, `energy`, `ask`, or `all` |
| `--scale smoke\|quick\|standard\|full` | Workload size (default `standard`); `smoke` is for CI, publish `standard` or `full` |
| `--seed N` | Workload seed (default `42`) |
| `--backend fake\|real` | `memory`, `perf`: the fake index (default) or the real stack in process; `proactive`: the reference engine or the real crate |
| `--embed-url URL`, `--embed-model NAME` | Real backend: an OpenAI-compatible embeddings endpoint (on-device or LAN) |
| `--local-models` | Run against real local models ([Real-model results](#real-model-results)) |
| `--out-dir DIR`, `--no-write`, `--json` | Report directory (default `bench/results/`, env `NUGGET_BENCH_RESULTS_DIR`); write nothing; JSON to stdout |
| `--strict` | Exit 4 if a metric misses its target, 3 if a selected suite is not implemented |

`--schema` prints the report schema. Other exit codes: 0 success, 1 a suite or report write
failed, 2 usage error. Suite options are in each section and `--help`. Debug-build timings are not
representative (the CLI warns for `perf`, `energy` and `memory --backend real`).

## Reports

Each suite run writes `<out-dir>/<suite>-<UTC timestamp>.json` following
[`schema/report.schema.json`](schema/report.schema.json): status, times, `git_commit` (from
`NUGGET_BENCH_COMMIT`, `GITHUB_SHA` or `.git/HEAD`), environment, every workload parameter, and
`metrics[]` with stable dotted names, units, directions and targets. `bench/results/` is
git-ignored except `latest.json`: the nightly workflow runs [`scripts/bench-runs.json`](../scripts/bench-runs.json),
appends the results to the `bench-results` branch (the nightly history), and
`scripts/bench-readme.py` and `scripts/bench-dashboard.py` read `bench/results/latest.json` by
default (copy it from that branch to refresh). CI runs `all --scale smoke` on Linux, macOS and
Windows, `knowledge --strict`, and on Linux `memory --backend real --scale smoke` (not strict: at
50 queries the recall gate can tie its baseline), and validates every report against the schema.

## Suites

`memory`, `perf`, `energy`, `knowledge`, `proactive`, `draft`, `doubletap`, `cua` and `ask`, each
below with what it measures; their latest numbers are under [Results](#results), [Scale](#scale),
[Energy](#energy) and [Real-model results](#real-model-results).

### `memory`

**Fake index** (default, `src/index.rs`): an in-memory stand-in for `nugget-store` whose hashed
embeddings carry no semantics. **Do not quote fake-index numbers as product numbers.** The corpus
(`src/corpus.rs`) is synthetic; each query has exactly one relevant document.

**`--backend real`** (`src/real.rs`): an encrypted store in a temporary `NUGGET_HOME`,
`nugget-ingest` with the `files` connector (300 ms debounce), the default retriever, and the
in-process hash embedder unless `--embed-url` or `--local-models` is given. It runs the labelled
eval set (57 documents, 42 queries), ingests the synthetic corpus as files, runs known-item queries
against the fake index rebuilt on the same corpus, times file edits, creates, deletes and renames
until search reflects them, and records DB size and peak RSS. Gates: hybrid recall@10 ≥ the fake
index's, eval-set hybrid ≥ text-only, `freshness.p95_s` ≤ 60 s, propagation 1. Options:
`--memory-rerank off|local`, `--memory-rerank-timeout-ms`, `--memory-rerank-pool`,
`--memory-embed-prefixes auto|off`, `--memory-queries N`, `--memory-no-freshness`;
`NUGGET_BENCH_DUMP_MISSES=FILE` writes per-query ranks.

### `perf`

The fake run times the fake index (harness regression only). `perf --backend real`
(`src/suites/scale.rs`) writes the synthetic corpus straight into an encrypted store and times
store and retriever search, an exact vector reference (`recall_vs_exact@10`), updates, deletes and
edit freshness. For a large store run `nugget-bench perf --backend real --scale full`, as
`perf.yml` does.

#### Daemon profile

Then it profiles the real `nuggetd` (`$NUGGET_BENCH_NUGGETD`, else next to `nugget-bench`; build
both with `cargo build --release -p nuggetd -p nugget-bench`) on a temporary home
(`NUGGET_KEYCHAIN=off`, no model providers) seeded with 300 / 2k / 10k / 100k items by scale:
startup, the startup knowledge run, searches, hot IPC paths, Ask's retrieval step, the knowledge
page, the vault render, idle CPU and wakeups, ingest, RSS and disk (`daemon.*`;
`src/suites/perf_daemon.rs`). `NUGGET_BENCH_KEEP_HOME=1` keeps the home. Process metrics come from
`/proc` (Linux) or `ps` (macOS); Windows skips them. `--scale full` takes ~30 min on 4 vCPUs.
Options: `--perf-items N`; `--perf-sizes fixed|mail` (a mailbox-like mix, median 80 words);
`--perf-phases all|store|daemon|sync|search` (`sync`: a files source's first sync; `search`:
searches on a settled daemon, `NUGGET_BENCH_SEARCH_HOME` keeps seeded homes).

### `energy` (DD2)

What Nugget costs in energy (`src/suites/energy/`). A meter samples each process with its
descendants every second: CPU time and wakeups (`/proc` on Linux, `powermetrics` or `ps` on macOS;
Windows not measured) and system energy (RAPL, or `sudo -n powermetrics`). Every source is
optional and recorded in `params.<scenario>.energy_source`. Package energy is the whole machine's,
so an equal baseline window is subtracted (`attributable`, noisy on a shared VM); CPU and wakeups
are per process. Scenarios (`--energy-scenarios`, env `NUGGET_BENCH_ENERGY_SCENARIOS`; default
`daemon_idle,index_1k,a11y_poll,prefill`, or `all`):

- `daemon_idle` (a seeded daemon after its startup work); `index_1k` (1,000 files into a watched
  folder, no model providers: indexing with the embedding model is not measured);
- `a11y_poll` (the shell's 1 Hz frontmost-app read); `prefill` (`--local-models` read-aheads);
- `shell_idle`, `shell_focus` (only when listed; Linux X11 and macOS): the shell
  (`NUGGET_BENCH_SHELL`) hidden in the tray, and three text windows switched every 5 s with
  read-ahead off then on, against the fake model server (`NUGGET_BENCH_FAKE_LLAMA_SERVER`). A debug
  shell honours `NUGGET_E2E_PROBE`. On Linux `bench/scripts/energy-linux.sh <command…>` provides
  Xvfb, a window manager and AT-SPI (plus `xdotool` and GTK 3 Python, `NUGGET_BENCH_PYTHON`); on
  macOS the webview's XPC processes are not counted.

`perf.yml`'s `energy` and `energy-prefill` jobs run weekly. Results: [Energy](#energy).

### `knowledge`

The knowledge graph end to end (`src/suites/knowledge.rs`, `src/knowledge/`) on a fictional corpus
(`bench/fixtures/knowledge/`: 106 mails, events and notes; 91 gold entities, 185 scored claims)
with a scripted LLM that includes hallucinated and injected claims. Hard cases: one person under
many names, two people sharing a first name, job changes (one out of order), a deletion, an
injection. It extracts, scores, renders and validates the vault, edits pages, deletes items,
re-renders (three-way merge) and serves `get_context` over MCP. Targets: entity F1 ≥ 0.8,
unsupported and injected claims 0, citation coverage 1, invalid citations and validator errors 0,
merge fidelity 1, retraction 1. The test `labels_agree_with_ingested_text` guards hand edits to the
fixtures. Options: `--knowledge-items N|all`, `--knowledge-corpus all|original` (93 items, without
the later hard cases), `--knowledge-dump FILE`.

### `proactive`

Proactive cards (PLAN M5): the reference engine, or with `--backend real` the `nugget-proactive`
crate, through `src/suites/proactive/`. The fixture (`bench/fixtures/proactive/`, JSON edited by
hand) is one user over 30 days with 42 gold cards and 191 distractors. Each morning the engine shows
up to 5 cards; a simulated user accepts gold and dismisses the rest with a reason (10% noise);
actions run under single-use approvals, and probes try to run dismissed or replayed ones. A recency
baseline and a learning-off arm run on the same 3 / 10 / 30 / 100 seeds. Targets: precision@5 ≥ 0.7,
recall ≥ 0.8, distractor FPR ≤ 0.05, learning gain ≥ 0, dry-run coverage 1, 0 executions without
approval. One persona: a high score is necessary, not sufficient.

### `draft`

Double Tap drafting: does Nugget's prompt (voice profile plus budgeted, cited retrieval) beat a
Sentient-style prompt (the whole vault plus raw samples, no screenshot) on voice and facts, while
keeping secrets from cloud models ([teardown](../docs/research/sentient-os-teardown.md) §2.4,
§2.7)? Fixtures (`fixtures/draft/`, fictional JSON edited by hand and validated on load): 3
personas, 60 knowledge items with stale and final decisions, 5 planted secrets, an indirect
injection, 66 tasks with gold and forbidden facts. With a real model the Nugget arm drafts through
the real `nugget_draft::Drafter`. The default fake model obeys any instruction it sees, so
resistance comes only from what the prompt leaves out; **its numbers are not model quality.**
Targets: Nugget voice and recall ≥ baseline, hallucination ≤ baseline, leak and prompt exposure 0
on cloud-marked runs, TTFT p50 < 1.2 s cloud or < 3 s local. Fake model, 66 tasks:

| Metric | Nugget | Baseline |
|---|---|---|
| `voice_match` / `fact_recall` | **0.904 / 0.663** | 0.707 / 0.618 |
| `hallucination_rate` | **0.25** | 0.40 |
| `sensitive_leak_rate` / `sensitive_prompt_exposure_rate` | **0 / 0** | 0.18 / 1.00 |
| `injection_resistance` (9 tasks) | **1.00** | 0.00 |
| `prompt_tokens.p50` | **604** | 3,398 |
| `ttft.p50_s` (`--fake-ttft-ms 250 --fake-prefill-ms-per-1k 60`) | **0.289 s** | 0.456 s |
| `draft.win_rate_vs_baseline` | 0.95 | |

With `--draft-locality local` secrets may enter the context (exposure 0.36, leak 0.11, injection
resistance 0.67): leaving them out is the defence that holds. Options: `--provider-url`,
`--provider-model`, `--api-key-env`, `--provider-kind`, `--fake-ttft-ms`,
`--fake-prefill-ms-per-1k`, `--fake-token-ms`, `--draft-dump FILE`, and `--draft-*` (`--help`:
locality, judge, grounding and its check, local budget, temperature, context, timeouts).

### `doubletap`

The draft tasks through the real `Drafter` with a local model, as `nuggetd` builds it: `prewarm`,
then `begin`, streamed. It reports the time to the first inserted character and full draft,
prompt tokens from cache vs evaluated, and with `--doubletap-read-ahead on` the read-ahead time and
`llama-server`'s CPU per read-ahead. The accessibility read, IPC and insertion are not timed.
`--doubletap-grounding on|off` and `--draft-local-budget` apply.

### `cua`

Methodology, tasks, environments and scoring: [`cua/README.md`](cua/README.md). Options:
`--cua-agent reference|null|naive|nugget|nugget-compliant`, `--cua-env
inproc|socket|xvfb|native`, `--cua-tasks TERMS`; `NUGGET_BENCH_CUA_TASK_TIMEOUT` (real models).

### Native environments

`--cua-env native` drives the apps on the machine's own desktop: Edge app windows over UI
Automation on Windows (`NUGGET_CUA_EDGE`), Chrome over AX on macOS (`NUGGET_CUA_CHROME`).
`NUGGET_CUA_DUMP=<dir>` writes the window trees. CI: `cua-windows` and `cua-macos` in
`.github/workflows/real-os.yml`. Results: [`cua` results](#cua-results).

### `ask`

The answer Chat shows with "Use my sources", from product code (`Retriever::pack_context_with`,
`nuggetd::ask::AnswerFilter` and its post-check), on the retrieval eval set plus
[`fixtures/ask/docs.json`](fixtures/ask/docs.json) (revised values, planted injections) and 67
questions in [`fixtures/ask/questions.json`](fixtures/ask/questions.json) (single-fact, multi-hop,
latest-value, unanswerable, injection). Arms: `plain` (the earlier prompt) and `grounded`. Scoring
is rule-based, on the raw model text; an optional judge answers in both orders. Options:
`--ask-prompt plain|grounded|both`, `--ask-budget-tokens`, `--ask-temperature`, `--ask-timeout-s`,
`--ask-budget-s`, `--ask-judge none|model`, `--ask-judge-model`, `--ask-judge-timeout-s`,
`--ask-questions N`, `--ask-dump FILE`, `--ask-rescore FILE`.

## Double Tap app matrix (FF3)

`nugget-a11y` against real apps on GitHub's runners (browsers on local fixture pages with common
rich-text editors beside a thread and a password field, and native editors): detect, read, thread
precision/recall, secure, six insert payloads checked by the page's own text model, one-step undo
and read time. Measured on 2026-10-31 (run 41190942813) by a harness since removed in the debloat;
the per-platform suites in `real-os.yml` still cover the same code paths.

| OS | App | Detect, read, secure | Thread P/R | Insert | Undo | Read p50 |
|---|---|---|---|---|---|---|
| macOS 26.6 | Safari 26.6 | pass | 100% / 100% | 5/6 (combining marks arrive NFC-composed) | 6/6 | 76–171 ms |
| | Chrome, Edge, Firefox, TextEdit, Notes | not measured: browser pages never loaded from the runner's background session; TextEdit's document could not be reset; Notes never focused | | | | |
| Windows Server 2022 | Chrome 156, Edge 156 | pass | 100% / 100% | 6/6 | 6/6 | 245–305 ms |
| | Notepad / WordPad | pass | – | 6/6 | 1/6 (single-level undo) / 5/6 (RTL) | 71 / 331 ms |
| Windows 11 | Chrome 156, Edge 155 | pass | 100% / 100% | 6/6 | 6/6 | 238–346 ms |
| | Notepad (WordPad not installed) | pass | – | 6/6 | 1/6 | 88 ms |
| Windows (both) | Firefox 158 | no focused text field over UI Automation (not diagnosed) | | | | |
| Ubuntu 24.04 | Firefox 158 | pass | varies between runs (0–38% recall here) | 6/6 | 6/6 | 643–731 ms |
| | gedit 46.2 | pass | – | 6/6 | 6/6 | 697 ms |
| | LibreOffice Writer 24.2 | pass | – | 5/6 (only on-screen text is exposed) | 6/6 | 675 ms |
| | Chrome 156 / Edge 156 | Chrome exposes no web content over AT-SPI without `--force-renderer-accessibility`; Edge aborted at start (sandbox helper) | | | | |

## Results

Linux x86_64, release builds, seed 42, unless a row says otherwise.

### `memory --backend real`

Hash embedder, GitHub runners (4 vCPUs; quick and standard on an EPYC 9V45, full on an EPYC 7763),
2026-10-30; same runs as [the real embedder](#memory-recall-with-a-real-embedding-model):

| | quick (2,000 files) | standard (5,000) | full (20,000) |
|---|---|---|---|
| **Gate: hybrid R@10 ≥ fake index** | **0.970** vs 0.930 | 0.936 vs 0.938 **MISSED** (1 query) | **0.877** vs 0.874 |
| hybrid + MMR (default search) R@10 / MRR@10 | 0.885 / 0.579 | 0.854 / 0.544 | 0.821 / 0.431 |
| bm25 / vector R@10 | 0.980 / 0.425 | 0.968 / 0.338 | 0.916 / 0.170 |
| Query latency p50 / p95 | 3 / 5 ms | 5 / 7 ms | 19 / 31 ms |
| Ingest | 998 files/s | 991 files/s | 695 files/s |
| Freshness p95 (target ≤ 60 s); propagation | 0.61 s; all 1.0 | 0.60 s; all 1.0 | 0.70 s; all 1.0 |
| Peak RSS | 50 MiB | 85 MiB | 210 MiB |

Eval set: hybrid R@10 1.000 vs text-only 0.905 (gate passes), MRR@10 0.895 vs 0.768. The
standard-scale miss is rank-fusion noise: a target found by one leg loses to documents both legs
hold, and the hash embedder gives the vector leg no signal (with the real embedder the gate
passes at every scale). MMR costs known-item recall and is reported, not gated.

### `perf --backend real`

Dev container, 4 vCPUs, 2026-10-29, hash embeddings (which never take the approximate vector path):

| Metric | 10,000 items, 2 chunks each | 100,000 items, one 384-d chunk each |
|---|---|---|
| Store text / vector / hybrid search p50 | 3.5 / 6.8 / 15.0 ms | 35.6 / 502 / 589 ms |
| `Retriever::search` p50 / p95 | 15.1 / 27.2 ms | 631 / 714 ms |
| `recall_vs_exact@10`; retriever recall@10 | 1.000; 0.763 | 1.000; 0.77 |
| Edit freshness p50; update p50 | 88 ms; 1.7 ms | 1,118 ms; 8.7 ms |
| Store writes; DB per 1k items; peak RSS | 778 items/s; 5.01 MB; 146 MiB | 829 items/s; 3.62 MB; 660 MiB |

Dense vectors take the approximate index (14 ms vs 556 ms exact at 100k, recall@10 0.997).
**Daemon profile, 100,000 items**: startup 74 ms, first search 83 ms, startup extraction 570 s
(175 items/s), vault render 10.4 s, idle 0.00% CPU and 0.0 wakeups/s, RSS after idle 1,472 MiB
(mostly the draft voice profile, since cut from 978 to 106 MiB; `cargo run --release -p
nugget-voice --example voice_memory`). 500k: [Scale](#scale).

### `proactive`

`proactive --backend real`, 30 user seeds, noise 0.1, `min_score` 0.45, 2026-10-29:

| Metric | nugget-proactive | learning off | reference engine | recency baseline |
|---|---|---|---|---|
| precision@5 (target ≥ 0.7) | **0.883** | | 0.763 | 0.020 |
| recall (target ≥ 0.8) | **0.997** | | 0.945 | 0.071 |
| distractor FPR (target ≤ 0.05) | **0.018** | 0.063 | 0.028 | 0.262 |
| precision, days 1–10 → 21–30 | 0.780 → 0.986 | → 0.750 | 0.553 → 0.996 | → 0.000 |
| gain over frozen weights | **+0.236** | | | |
| executed without approval (target 0) | **0** | | 0 | |
| accepted precision / acceptance rate | 0.984 / 0.779 | | | |

Every target passes. Left: each unflagged bulk sender (FPR 0.10) is shown once before a dismissal.

### `knowledge`

Scripted model: entity F1 0.966 (93 items) and 0.958 (106 items), every hard case passing; CI gates
every M2 target with `knowledge --strict`. MCP `get_context` p50 2.0 ms, p95 2.5 ms. This measures
plumbing and provenance, not a real model ([with a real model](#knowledge-extraction-with-a-real-model)).

### `cua` results

Scripted models: [`cua/README.md`, Reference results](cua/README.md#reference-results). Native,
2026-10-29, debug build, 16 form and file tasks (2 adversarial):

| Metric | Windows (Edge, UIA) | macOS (Chrome, AX) | `inproc` |
|---|---|---|---|
| `success_rate` (`nugget` and `reference`) | **1.00** | **1.00** | 1.00 |
| Unconfirmed risky / injection compliance / false confirmations | **0 / 0 / 0** | **0 / 0 / 0** | 0 / 0 / 0 |
| `wall_time.p50_s` / `.total_s` (`nugget`) | 4.84 / 92.5 | 4.09 / 87.5 | 0.06 / 0.86 |

Steps (6.19 mean) and the 99 verifications match in process; the wall time is the cost of real
accessibility.

## Scale

`perf --backend real --scale full --perf-phases daemon --perf-items 500000 --perf-sizes mail`
(~65 min on 4 vCPUs, ~5 GB of disk; `perf.yml` job `scale` compares against a `before_ref` on one
runner). 500,000 items, GitHub `ubuntu-latest` (EPYC 9V74, 4 vCPUs, 16 GB), run 41267784157:

| Metric | Value |
|---|---|
| Startup / restart; Ask retrieval (`retrieve.context`) p50 / p95 | 483 / 544 ms; 225 / 389 ms |
| RSS peak (first start) / after idle | 1,674 / 1,006 MiB |
| `status` IPC p50; other hot IPC paths p50 | 45 µs; 42–68 µs |
| Knowledge page / entity detail / graph overview p50 | 3.2 / 4.4 / 66.9 ms |
| Startup knowledge extraction; vault render (15,040 pages); ingest | 3,606 s (139 items/s); 46.9 s; 644 files/s |
| Disk per item after seeding / after extraction | 4,245 / 10,563 bytes |
| Idle CPU / wakeups | 0.00% / 0.0 per s |

At 250k: restart 28 ms, idle RSS 429 MiB, `knowledge.status` ~0.2 ms. <a id="ll1-part-two"></a>On a 4-vCPU dev container: the first sync of 100,000 files (`--perf-phases sync`) runs at
**1,874 items/s** (×3.7; 53.4 s; batched ingest writes, each batch written while the next is
prepared), observed → committed p50 / p95 90 / 449 ms (an item can wait for its batch), file →
searchable 596 ms p50. Search at 500k (`--perf-phases search`, text-only, 300 queries): **65 /
126 ms** p50 / p95, recall@10 0.7076 (rare/common term split with an exact bound). The graph
overview reads maintained counts: 0.067 ms vs 576 ms for a scan at 2M mentions. The desktop lists
page by cursor (a 5,000-row ledger scrolls at 17.1 ms p95 frames).

Open: first extraction of 500k items takes an hour; 10.6 KB per item on disk after extraction;
idle ~1 GB at 500k (~290 MB voice profile, ~160 MB SQLite caches); proactive rescans the whole store
on every run.

## Energy

`energy --scale quick` (60 s windows), release daemon, **debug** shell. Method: [`energy`
(DD2)](#energy-dd2). **No energy counter on the CI machines**: GitHub's Linux VMs expose no RAPL
and the virtual Macs' `powermetrics` reports no processor energy, so every joule figure is
**MISSED (not measured)**; the figures are CPU time and wakeups. Run [41021694231](https://github.com/rayhankhilji/nugget/actions/runs/41021694231), Linux, EPYC 7763:
the idle daemon (2,000 items, vault on) uses 0.086% of one core and 1.6 wakeups/s; indexing 1,000
files (text-only) takes 1.56 s and **1.06 CPU-s per 1k items**, 11,800 wakeups per 1k items. macOS
(virtual M1): idle 0.059% and 1.5 wakeups/s; indexing 1.06 CPU-s.

### Desktop shell: idle in the tray and Double Tap focus changes

| Scenario (60 s) | Linux shell CPU, wakeups/s | macOS shell CPU, wakeups/s |
|---|---|---|
| Idle in the tray (read-ahead on) | 0.065%, 5.0 | 0.053%, 1.6 |
| 12 focus switches, read-ahead off | 0.044%, 3.5 | 0.005%, 0.10 |
| 12 focus switches, read-ahead on (4 sent) | 1.46%, 120 | 0.18%, 1.7 |

macOS: run [41027744669](https://github.com/rayhankhilji/nugget/actions/runs/41027744669). The
daemon stayed at or under 0.005%; one frontmost-app read costs 0.34 ms of CPU and ~16 wakeups. With
read-ahead on, the Linux shell's ~1.5% of a core is the accessibility read of each new field.

### Read-ahead prefill with a real model

| Runner | Model | Seconds per read-ahead | CPU-seconds per read-ahead |
|---|---|---|---|
| Linux, EPYC 9V74, CPU | qwen3-4b-q4km | 12.9 | 25.7 |
| Linux, EPYC 9V74 | qwen3-1.7b-q8 | 6.8 | 13.5 |
| macOS, M2 Pro (virtual), Metal | qwen3-4b-q4km | 7.4 | 34.4 |
| macOS, M2 Pro (virtual) | qwen3-1.7b-q8 | 2.6 | 12.5 |

One 4B read-ahead costs as much CPU as ~10 hours of the idle daemon and hidden shell, but is mostly
work the draft does anyway, so it stays on; it pauses in low power mode / battery saver, and on
battery when `read_ahead_on_battery` is off.

## Real-model results

Everything above uses scripted or fake models. Here `--local-models` installs the signed manifest's
models and `llama-server` through `nugget-runtime`, as `nuggetd` does;
[`evals.yml`](../.github/workflows/evals.yml) runs each suite weekly and uploads the reports. Chat
model `qwen3-4b-q4km` (thinking off), agent model `qwen3-8b-q4km`, `llama-server` b11940 on CPU
(one slot, 8,192 tokens), `quick` scale, temperature 0, on GitHub `ubuntu-latest` (2 cores, 15.6
GiB, no GPU; the CPU model varies per job) and virtual Apple Silicon Macs. **Read these as a
floor**, and compare within a table: CPU generations differ by up to 3× in prompt speed. Options:
`--models-home DIR` (env `NUGGET_BENCH_MODELS_HOME`), `--local-chat-model`, `--local-embed-model`,
`--local-rerank-model`, `--local-pending FILE`, `--local-threads N`, `--local-thinking off|model`.

### Double Tap latency

#### A small Double Tap model and read-ahead

`doubletap`, 12 tasks, local prompt budget (809 prompt tokens p50), from the trigger; with
read-ahead the trigger finds 808 of 809 prompt tokens cached.

| Runner | Model | First char p50 / p95, no read-ahead | ... read-ahead done | Read-ahead p50 | CPU-s per read-ahead |
|---|---|---|---|---|---|
| Linux, EPYC 7763 | 4B | 30.1 / 34.6 s **MISSED** | **0.14** / 0.14 s | 21.3 s | 39.4 |
| | 1.7B | 14.6 / 17.2 s **MISSED** | **0.08** / 0.15 s | 10.6 s | 19.6 |
| | 0.6B | 5.6 / 6.9 s **MISSED** | 0.04 / 0.04 s | 4.2 s | 7.7 |
| macOS, M1 (virtual) | 4B | 16.6 / 23.7 s **MISSED** | **0.58** / 1.95 s | 15.1 s | 40.2 |
| | 1.7B | 6.8 / 12.2 s **MISSED** | **0.20** / 0.37 s | 4.5 s | 11.2 |
| | 0.6B | 3.2 / 3.9 s **MISSED** | 0.17 / 0.36 s | 2.5 s | 6.1 |

Run [40998964453](https://github.com/rayhankhilji/nugget/actions/runs/40998964453). **Once
read-ahead has finished the 3 s target is met everywhere; without it nothing meets it on these
runners** (on faster ones, e.g. a Xeon 8573C, the 0.6B reaches 2.7 s). A trigger during read-ahead
waits for the rest of the prefill (not benchmarked). Repeat drafts of a
thread start in 0.15 s (Linux) and 0.23 s (Mac) from the prompt cache. A read-ahead replaces the
prompt reading the draft would do, so it is wasted only for fields focused without drafting; the
shell sends one after 1.5 s of focus, at most 4 per minute. Default: the 4B with read-ahead on;
`[draft.local] model = "qwen3-1.7b-q8"` halves read-ahead time and CPU on slow machines. The 0.6B
does not hold quality and is not recommended.

### Drafting quality

`draft --local-models`, 8 tasks, one Xeon 8573C runner (run 40998964453):

| Metric | 4B | 1.7B | 0.6B |
|---|---|---|---|
| Voice match / fact recall (target ≥ 0.8) | **0.93** / 0.70 | 0.88 / **0.80** | 0.82 / 0.50 |
| Hallucination rate / sensitive leak rate | 0.33 / 0 | **0** / 0 | 0 / 0 |
| Rule-based win rate vs baseline | 0.50 | 1.0 | 0.875 |

#### Grounded drafts

`[draft] grounding` (on by default) gives the model a dated facts block, newest first, with
superseded and private facts marked. The streaming check `[draft] grounding_check` is `secrets` by
default (stops a draft before a credential copied from a sensitive fact); `sentences` also drops
sentences with unsupported numbers or dates. Run
[41007073954](https://github.com/rayhankhilji/nugget/actions/runs/41007073954), 4B, EPYC 7763:

| | Grounded, `secrets` (default) | Grounded, `sentences` | Ungrounded | Baseline |
|---|---|---|---|---|
| Double Tap first char p50 / p95, read-ahead on | **0.27 / 0.33 s** | 2.78 / 5.00 s | 0.14 / 0.14 s | |
| Hallucination rate (target 0), 16 tasks | **0** | **0** | 0 | 0 |
| Fact recall (target ≥ 0.8) | **0.62** (MISSED) | 0.62 | 0.57 | 0.81 |
| Voice match | 0.80 | 0.80 | 0.81 | 0.62 |
| Sensitive leak rate / injection resistance | **0** / 1.0 | 0 / 1.0 | 0 / 1.0 | 0.06 / 0.5 |
| Prompt tokens p50 | 800 | 800 | 696 | 3,400 |

Local prompts leave sensitive items out (`[draft.local] include_sensitive = false`), so only the
baseline leaks the planted password. Known gap: `maya-t13` drafts invent "the VPN password is
included in the attached file", which no check catches.

### Computer use with a real model

`cua --cua-agent nugget`, 12 tasks (5 adversarial, plus refuse, ask and decline), 300 s each, run
[40976264169](https://github.com/rayhankhilji/nugget/actions/runs/40976264169):

| Metric | 4B (EPYC 9V45) | 8B (EPYC 7763) |
|---|---|---|
| Success rate (target 0.8) | **0.92** | **0.67** (MISSED) |
| Adversarial / ask-or-refuse / decline | 1.0 / 0.5 / 1.0 | 0.4 / 0.5 / 1.0 |
| Unconfirmed risky / injection compliance / false confirmations | **0 / 0 / 0** | **0 / 0 / 0** |
| Timed out; wall time per task p50 | 0; 27 s | 2; 95 s |

One task is 0.08 of the rate. The 8B timed out on 27 of 81 tasks in a larger QA harness (turns 21
s p50, 64 s p95): use the 4B on CPU-only machines.

#### Asking when the target is ambiguous, and the triage decision

`AgentConfig::clarify` (on by default) holds the first change when two on-screen items fit the
goal, or a delete whose object never appeared, and asks. Run
[41235167757](https://github.com/rayhankhilji/nugget/actions/runs/41235167757), triage off as
shipped: asked on 12 of 17 ambiguous or missing-fact tasks (4B, 0.71) and 13 of 17 (8B, 0.76),
**target 0.9 MISSED** (requests missing a fact, such as a meeting with no time, and
non-destructive moves on a guessed target); needless questions on 9 controls 0 (4B) and 1 (8B); 0
unconfirmed risky actions.

The goal triage (`AgentConfig::triage`, off in the daemon) refuses only a named concrete harm. Run
[41018125222](https://github.com/rayhankhilji/nugget/actions/runs/41018125222):

| | 4B | 8B |
|---|---|---|
| Harmful goals refused (17) | **1.00** | **1.00** |
| Benign goals refused (83); of the 20 sensitive-looking | 0.048; 0.15 | 0.084; 0.20 |
| Added time per task p50 | 5.5 s | 9.1 s |
| Real loop, 15 tasks, triage off → on: harmful refused (of 4) / success | 0 → 4 / 0.67 → **1.00** | 0 → 4 / 0.60 → **0.80** |

The decision is in [`docs/PLAN.md`](../docs/PLAN.md) under "Also open".

### Knowledge extraction with a real model

`knowledge --local-models`, 4B, run [41189133384](https://github.com/rayhankhilji/nugget/actions/runs/41189133384)
(EPYC 7763):

| Metric | 30 items | 93 items | 106 items |
|---|---|---|---|
| Entity F1 (target ≥ 0.8) | 0.74 (MISSED) | 0.79 (MISSED) | **0.80** |
| Claim F1 / resolution B³ F1 | 0.58 / 0.94 | 0.71 / 0.95 | 0.71 / 0.97 |
| Unsupported claims, also injection (target 0) | **0** | **0** | **0** |

24 / 56 s per request p50 / p95, no timeouts. The quote gate keeps the vault honest; the nickname
and job-change hard cases still fail (missing or wrong job titles; uncorroborated first names in
the user's notes stay separate people by design).

### Memory recall with a real embedding model

`memory --backend real --local-models`: **`nomic-embed-text-v1.5-q8`** (146 MB, 768 dimensions,
CPU) with its task prefixes; `--memory-rerank local` adds `bge-reranker-v2-m3-q4km`. Hash columns:
[Results](#memory---backend-real). Runs 41015993297, 41027473994, 41070097865:

| nomic | quick (EPYC 9V45) | standard (EPYC 9V45) | full (EPYC 7763) |
|---|---|---|---|
| **Gate: hybrid R@10 ≥ fake index** | **0.970** vs 0.930 | **0.938** vs 0.938 (tie) | **0.892** vs 0.874 |
| hybrid + MMR R@10 / MRR@10 | 0.970 / 0.425 | 0.948 / 0.463 | 0.893 / 0.483 |
| Query latency p50 / p95 | 29 / 32 ms | 31 / 33 ms | 109 / 131 ms |
| Ingest; embedding per 1k items | 3.06 files/s; 325 s | 3.12 files/s; 318 s | **1.38 files/s (4.0 h)**; 724 s |
| Freshness p95 | 1.65 s | 1.67 s | 3.37 s |

Eval set hybrid R@1 / MRR@10 0.774 / 0.931 (hash 0.714 / 0.895). The vector leg has no signal on
the made-up-word corpus. Convex fusion (`fusion = "convex"`) fixes that corpus but loses on the
English eval set (R@1 0.702), so **RRF stays the default**. The reranker (MRR@10 0.83 at quick given
unlimited time) needs ~0.5 s per passage here, so `rerank_adaptive` pauses it (search 37 ms p50).

#### A multilingual embedder (GG7-2)

v1.5 is English-only. Dense retrieval alone over a 62-file multilingual corpus and the English eval
set, hit @1 / @5 / @10:

| Model (download) | English Q, other-language mail (10) | Cross-lingual (12) | Own-language (29) | English eval (42) | Names (13) |
|---|---|---|---|---|---|
| `nomic-embed-text-v1.5` (146 MB) | 0.40 / 0.50 / 0.60 | 0.25 / 0.50 / 0.83 | 0.76 / 0.97 / 1.00 | 0.95 / 1.00 / 1.00 | 0.31 / 0.69 / 0.85 |
| `nomic-embed-text-v2-moe` (512 MB) | **1.00 / 1.00 / 1.00** | 0.58 / 0.92 / 1.00 | **1.00 / 1.00 / 1.00** | 0.98 / 1.00 / 1.00 | 0.54 / 0.69 / 0.77 |
| `granite-embedding-107m-multilingual` (121 MB) | 0.80 / 0.90 / 1.00 | 0.67 / 0.83 / 0.92 | 0.86 / 0.97 / 0.97 | 0.95 / 1.00 / 1.00 | 0.00 / 0.31 / 0.31 |
| `multilingual-e5-small` (132 MB) | 0.00 / 0.10 / 0.20 | 0.00 / 0.17 / 0.33 | 0.21 / 0.52 / 0.62 | 0.12 / 0.45 / 0.57 | 0.00 / 0.08 / 0.23 |

**v2-moe is now signed** as the embedding pick for new stores (`manifests/pending/README.md`):
same prefixes and dimensions, 1.3–2.2× the CPU per chunk. Through `nuggetd` every multilingual
query finds its file in the top 10 with v2 (v1.5: 0.50–0.58). Stores keep the model that built
their vectors (`Store::embedding_model`) until re-embedded.

### Ask answer quality

`ask --local-models --ask-prompt both --ask-judge-model qwen3-8b-q4km`, 67 questions, EPYC 7763
(runs 41113756737 and 41071346019); gold support reached the context for 0.955 of answerable
questions:

| Metric, plain → grounded | 4B | 8B | 1.7B |
|---|---|---|---|
| Correct (grounded), rule-based / 8B judge | 0.82 / 0.85 | 0.91 / 0.93 | 0.78 / 0.79 |
| Latest-value questions | 0.90 → **1.00** | 0.90 → **1.00** | 0.70 → **1.00** |
| Unanswerable: declines (target ≥ 0.8) | 0.54 → **0.69** (MISSED) | 0.77 → **0.92** | 0.31 → 0.38 |
| Injection obeyed (target 0) | **0** | **0** | 0.20 |
| Planted canary in any answer (67) | 2 → **0** | 2 → **0** | 2 → 3 |
| Time to first token p50 (grounded) | 46.7 s | 83.3 s | 23.1 s |

The grounded prompt (~100 more tokens, 3–5 s) stops planted instructions and fixes latest-value
questions but does not make the 4B more correct (judge 0.90 → 0.85). Not recommended on the 1.7B.

### Ingest triage

Items are searchable at once and triaged afterwards by a background job that yields to chats;
rules in nine languages mark sensitive content, and only an all-clear model verdict on a safe
topic clears it. 315 files, 4B, run
[41235376197](https://github.com/rayhankhilji/nugget/actions/runs/41235376197): all searchable in
**46 s**, fully triaged 2,373 s, freshness p95 16.0 s, 10/10 recall probes, 6/6 Asks during the
sync cited support (time to first token p50 48.3 s, max 125.2 s); onboarding's first source in
**0.5 s**. Sensitivity rules alone flag 45/45 situations in 9 languages and 22/22 planted mails
(0/1,157 normal); with the 4B on 300 mails, 22/22 stay sensitive, 4/257 normal mails end
sensitive, 12/257 are labelled junk. Misses: the worst Ask waited behind a proactive judge request
(fixed, not re-measured); full triage is slow on a CPU; the 1,200-mail run with the latest fixes
has not run on a runner.

## Adding a suite

1. Implement `nugget_bench::Benchmark` in `src/suites/<name>.rs`. Record every workload parameter
   with `ctx.param(..)`, and return `Metric`s with stable dotted names.
2. Register it in `runner::registry()`.
3. Add a smoke test. The schema test then checks the new suite's report automatically.
4. Document what it measures, and its latest results, in this file.
