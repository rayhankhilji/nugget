# HH2: ingest before/after

Measures what QA GG2 found in GG2-5/6/7 (`docs/qa/GG2.md` on `qa/ask`): with the Qwen3-4B, the
315-file GG2 corpus took ~41 min to become searchable because triage made one model call per item
before indexing. Ask was ~3× slower during that sync, and the 4B's "junk" verdicts left real items
(`mail/3-home/boiler.eml`, `calendar/onboarding-call.ics`, receipts) with no chunks. HH2 indexes
first and triages afterwards in the background. The same job runs both builds on one runner,
so they share a CPU, the corpus (dates fixed to 2026-10-30) and the cached models. Each build
gets a fresh home.

| File | What |
|---|---|
| `ingest_timeline.py` | Starts nuggetd with GG2's 4B config (default config, `chat_model`/`embedding_model` set, `idle_stop_minutes = 0`, knowledge off). Installs the models, adds the corpus as a `files` source, then polls `ingest.status` every 2 s. Writes `result.json` and `summary.md`. |
| `compare.py` | `compare.py BEFORE_DIR AFTER_DIR [--hardware FILE]`: a before/after markdown table with the runner's CPU. |
| `../../.github/workflows/hh2-ingest.yml` | Builds `nuggetd --release` at 7add95a (BEFORE) and at the pushed head (AFTER), runs both and posts the comparison to the job summary. Uploads everything as `hh2-ingest-<run id>`. |

The GG2 harness is not copied here. `ingest_timeline.py` imports `ask_qa.py` (the IPC client,
config edit, daemon start, `models.install`, `retrieve.ask` timing) from QA commit `1f2b786`.
It uses `--gg2-dir`, or extracts the file into `<out>/gg2` with `git show`. The workflow
extracts `gen_corpus.py` and `search-probes.txt` from the same commit.

## What is measured (all times in seconds after `sources.add`)

- **First search:** the first time a `search` (top 10) returns the target of each timed probe.
  The probes are boiler, onboarding call, Kestrel, City Power and Northgate. A probe's targets
  are the corpus files that contain every one of its words. The earliest of these times is
  `t_first_search_s`.
- **t_indexed:** the source's `items` reaches the corpus size (315).
- **t_triaged:** all items are stored and either `ingest.status.triage` is absent (old build), or
  its `pending` is 0 and its `state` is not `running`. This is capped at `--triage-timeout`
  (default 75 min), and a timeout is reported as such. With `--no-models`, triage state `off`
  also counts as done.
- **Ask during sync:** starting 60 s after the source is added, 6 questions are asked one after
  another through `retrieve.ask` as Chat sends them. The questions are L01, L05, L12, L15, L19
  and L25, all backed by ordinary items. Each records time to first token, total time, whether
  a supporting item was cited, the gold regex, and the ingest/triage state when the question
  started.
- **Ask idle:** the same 6 questions after t_triaged. If triage timed out, they are flagged as
  not idle.
- **Recall after settling:** every probe in GG2's `search-probes.txt` is run once more, recording
  found or not and the rank.
- **Items without chunks:** IPC has no per-item chunk count (no `items.get`), so this reports
  the source's `items - chunks`. That is a lower bound, and it is exact for this corpus, where
  every kept item is one chunk (text-only: 315 items / 315 chunks).

## Run

Real models: dispatch `.github/workflows/hh2-ingest.yml`. Its inputs are `chat_model`,
`before_ref` and `triage_timeout_min`. It also runs on pushes to `hh/ingest-triage`, but that
trigger is temporary.

Local, text-only (no models, so asks fail fast and are recorded as errors). This checks the
polling, the probes and the reports:

```sh
cargo build -p nuggetd
git show 1f2b786:qa/GG2/harness/gen_corpus.py > /tmp/gen_corpus.py
python3 /tmp/gen_corpus.py --out /tmp/hh2/corpus --questions /tmp/hh2/q.jsonl --today 2026-10-30
python3 qa/HH2/ingest_timeline.py --nuggetd target/debug/nuggetd --home /tmp/hh2/home \
  --corpus /tmp/hh2/corpus --questions /tmp/hh2/q.jsonl --out /tmp/hh2/out --no-models --ask-delay 3
python3 qa/HH2/compare.py /tmp/hh2/out /tmp/hh2/out   # (normally: before dir, after dir)
```

`--models-cache DIR` shares one model store between runs. Before the daemon starts, the
contents of DIR are moved into `<home>/data/models`, and they are moved back when it stops.

## GG6-1 re-check: onboarding's First source step (`hh2-gg6.yml`)

QA GG6 (`docs/qa/GG6.md` on `qa/first-week`, GG6-1) found that with the recommended 4B installed,
onboarding's "First source" step (the 24-file corpus in `qa/GG6/real-os/corpus`) took 474 s on
Linux before "Search works now". `.github/workflows/hh2-gg6.yml` (workflow_dispatch only) reruns
GG6's own harness, `qa/GG6/real-os/run-bridge.mjs`, before and after on one `ubuntu-22.04` runner.
The harness is extracted at run time from QA commit `e1eb203` with `git archive`, and it is not
copied into this branch.

- **Builds:** `nuggetd --release` (with the dev manifest key trusted, as release dry runs are) is
  built twice. BEFORE comes from a `git worktree` of `before_ref` (default `ea3f86c`, the
  integration commit GG6 re-checked) with its own `--target-dir`. AFTER comes from the pushed
  head. The job fails if the two binaries are identical. The frontend (`pnpm build`) and
  Playwright are installed in both trees. The harness is placed at `<tree>/qa/GG6/real-os` in
  each, so every run uses the `apps/desktop/dist`, `e2e/bridge.mjs` and `en.json` of its own
  commit.
- **Runs:** BEFORE, then AFTER, each with a fresh Nugget home and a real model download. The
  model is downloaded twice because pre-seeding it would skip GG6's Install click. With
  `soak_minutes` = 0 (the default), each run uses `--days none`: onboarding (On this device,
  install the recommended model, add the corpus folder and wait for Search works now,
  Permissions, Double Tap, Done), the first search and day 1's 10 chat questions. A value above
  0 also runs GG6's days 2–7 with that soak. `index_timeout_min` (default 30) raises the source
  step's timeout in the extracted `journey.mjs` (GG6's own value is 10). Any model server left
  running is killed between the two runs.
- **Result:** `gg6_compare.py BEFORE_OUT AFTER_OUT [--hardware F] [--labels B A] [--out F]`
  writes the table to the job summary. The headline is **Time to First source done**: GG6's
  `results.json` → `steps[]` entry `source_searchable`, `took_ms` (from the folder dialog
  closing to the step's row reaching `data-phase="ready"`, "N items indexed. Search works
  now."). The table also gives the item count from that text (or the last text seen, on a
  timeout), GG6's other onboarding steps, the day 1 chat timings, failed checks and the CPU from
  `lscpu`. Everything is uploaded as `hh2-gg6-<run id>`.

## JJ1-1 re-check: sensitive labels after real 4B triage (`hh2-jj1.yml`)

QA JJ1 (`docs/qa/JJ1.md` on `qa/wave-ii`, JJ1-1) found that after real 4B triage only 9 of 22
planted sensitive mails stayed sensitive: the model's single "sensitive" answer cleared every
health, money and legal mail, and the rules missed most non-English ones. Commit 8f606a20 adds
multilingual rules and asks the model per category. `.github/workflows/hh2-jj1.yml` reruns JJ1's
own measurement before and after on one `ubuntu-latest` runner. The harness (`qa/JJ1/harness`,
`qa/JJ1/triage`) is extracted at run time from QA commit `2cfd682` with `git archive`, and it is
not copied into this branch. It runs on `workflow_dispatch` only (so only once it is on the
default branch); the temporary push trigger used for run 41269745457 is gone.

- **Builds:** `nuggetd --release` (with the dev manifest key trusted, as in `qa-JJ1.yml`) is built
  twice. BEFORE comes from a `git worktree` of `before_ref` (default `adc2ee50`) with its own
  `--target-dir`, and AFTER comes from the pushed head. The job fails if the two binaries are
  identical. `adc2ee50` is `8f606a20^`, which isolates the JJ1-1 change.
- **Runs:** BEFORE, then AFTER, each with a fresh home and JJ1's exact invocation:
  `realmodel.py --nuggetd BIN --home H --out O --part triage --chat-model qwen3-4b-q4km
  --models-cache $RUNNER_TEMP/models-cache`. This uses 1200 mails (`gen_corpus.py`, seed 7) and
  the default config with the 4B and nomic embedder, waits up to 7200 s for triage, and reads the
  labels through `export.run`. One run took 74 min on JJ1's runner. Models are cached like
  `qa-JJ1.yml`. Leftover `nuggetd`/`llama-server` processes are killed between runs.
- **Result:** `jj1_compare.py BEFORE_OUT AFTER_OUT [--hardware F] [--labels B A] [--out F]
  [--labels-dir D]` writes the table to the job summary. It shows the 22 planted mails that stay
  sensitive (overall, per language and per kind), what the model said (`triage.sensitive`)
  against the rules (`sensitive_rule`), and normal and junk mails that end sensitive. Those are
  split by cause (rule, model kept it, or triage not done, since pending and unknown count as
  sensitive) and given with triage state counts. It also shows junk labelled and still
  searchable, the full-triage time, first search, Ask during the sync and when idle, and the CPU
  from `lscpu`. Everything goes in the `hh2-jj1-<run id>` artifact, with the export reduced to
  `labels-before.jsonl` and `labels-after.jsonl`.

## Triage prompt variants (`triage_prompts.py`, `hh2-prompts.yml`)

`triage_prompts.py --server URL --corpus DIR --out DIR [--normal-per-lang 40] [--batch 8]
[--variants A,B,C]` sends the request `nugget-ingest`'s `ai::triage_batch` sends (instructions,
numbered `<document>` elements, JSON schema, temperature 0, thinking off) straight to a
llama-server with the chat model, for several prompt/schema variants, on a sample of a JJ1 triage
corpus (every planted sensitive and junk mail plus N normal mails per language, seed 7). It
reports the model's recall on the planted sensitive mails, normal mails flagged sensitive or
junk, and output tokens and wall time per variant (`prompts.md`, `prompts.json`). `--batch`
fixes the batch size, which is how the small-batch effect behind the JJ1-1 junk regression was
found. `serve_chat.py` starts the daemon's own chat llama-server and model outside the daemon
for it, and `hh2-prompts.yml` (dispatch only) runs both on a runner. Locally, any llama-server
with the manifest's Qwen3-4B Q4_K_M works the same way.
