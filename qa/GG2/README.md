# GG2: Ask and Chat QA

Independent QA of Ask/Chat with real local models (brief: `docs/waves/wave-gg/GG2.md`; report:
`docs/qa/GG2.md`). Everything here is synthetic and written for this package; it shares no code with
the bench crate.

| Path | What |
|---|---|
| `harness/gen_corpus.py` | Generates the persona corpus (315 items) and `questions.jsonl` (123 conversations, 149 turns): gold facts as regex alternatives, supporting items, `must_not` canaries. Dates are anchored to `--today`; file mtimes are set to item dates. |
| `harness/ask_qa.py` | Stdlib-only JSON-RPC client. Writes the config, starts `nuggetd`, installs models with `models.install`, adds the corpus as a `files` source, waits for ingest, then asks every conversation with `retrieve.ask` the way `Chat.tsx` does. `--retrieval-only` needs no model. `--probe-search` runs `search` probes after ingest. |
| `harness/score.py` | Keyword/citation rubric: correct, retrieved, cited, cite precision, canaries, latency. |
| `corpus/`, `questions.jsonl`, `corpus-manifest.json` | Snapshot generated with `--today 2026-10-30`. |
| `explore/<area>/` | Explore-layer scenarios, fake model servers that capture requests, outputs and `FINDINGS.md` drafts. |
| `verify/` | Verification-layer results: each candidate reproduced by a fresh agent from the written repro. |
| `results/run1-41114925672/` | Run 1: answers, scores, configs, daemon logs, llama-server timings, `manual-grades.json`. |
| `results/run2-*` | Run 2: a repro of the model-level findings, with `search` probes. |
| `repro/search-probes.txt` | Exact-word searches used to detect items that triage dropped. |
| `run.env` | Defaults for a dispatched run (`RUNS=full` or `repro`, `ONLY=ids`); the workflow inputs override it. |

Local run without models:

```sh
cargo build --release -p nuggetd
python3 qa/GG2/harness/gen_corpus.py --out /tmp/c --questions /tmp/q.jsonl
python3 qa/GG2/harness/ask_qa.py --home /tmp/h --corpus /tmp/c --questions /tmp/q.jsonl --out /tmp/o --retrieval-only
python3 qa/GG2/harness/score.py --questions /tmp/q.jsonl --answers /tmp/o/answers.jsonl --out /tmp/o/score.json
```

Real models: run `.github/workflows/qa-GG2.yml` (workflow_dispatch; inputs `only` and `runs`).
