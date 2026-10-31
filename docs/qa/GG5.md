# GG5 QA: knowledge graph and Markdown vault, with real models

Independent QA of entity and claim extraction, the typed graph, the rendered Markdown vault, merging
the user's vault edits back, and the Knowledge pages of the desktop app (entity view, "Why?", graph
view), used the way a person would on a realistic synthetic life.

- Product under test: integration branch at `e7299584` (harness commits up to `qa/knowledge` HEAD);
  EE1's unmerged branch `ee/entity-resolution` at `a670826` for comparison.
- Branch with everything: `qa/knowledge`. Persona and corpus: `qa/GG5/PERSONA.md`,
  `qa/GG5/corpus/` (42 items: 21 mails, 7 calendar files, 14 notes, in two phases; `gold.json` holds
  the ground truth). Harness: `qa/GG5/harness/nugget_qa.py` (drives a real `nuggetd` over IPC: a
  `files` source on the corpus, snapshots of every `knowledge.*` result and of the vault after each
  step), grader `qa/GG5/harness/grade.py`, workflow `.github/workflows/qa-GG5.yml`.
- Explore layer (5 areas): `qa/GG5/findings/{resolution,vault-edits,truth-sensitive,app,real-model}.md`,
  with their scenario scripts and captured outputs under `qa/GG5/scenarios/`. Verification layer: a
  fresh agent reproduced each finding from the written repro alone, `qa/GG5/verify/*.md`.
- Real-model runs (GitHub `ubuntu-latest`, 4 vCPU AMD EPYC, 16 GB, CPU only, the signed manifest's
  models installed and served by `nuggetd`'s own runtime): outputs in `qa/GG5/runs/`.

REALMODEL_RUNS

All data is synthetic (`.example` domains, invented people). The container cannot run
`llama-server`, so the deterministic-only scenarios ran locally against a debug `nuggetd`; every
real-model observation comes from the GitHub runs.

## Verdict

VERDICT_TABLE

## Findings

Severity is user impact. "Verified" means a second, fresh agent reproduced it from the repro text
alone (`qa/GG5/verify/<area>.md`); real-model findings are verified by a second run of the workflow.
Each finding names its detailed write-up (`qa/GG5/findings/<area>.md`, same title), which has the full
quotes and the code walk; ids differ between runs, names do not.

FINDINGS

## What works well

WORKS_WELL

## Notes on the harness and the grader

- The grader's gold-fact recall is strict: it needs the exact predicate from the extractor's
  vocabulary and the gold object, so deterministic `attended`/`corresponded_with` facts and LLM facts
  with a near-synonym predicate do not count. Read the recall numbers as a floor; the findings are
  each checked by hand.
- `quote_not_in_source` counts quotes not found verbatim in the raw file. Most are Nugget's faithful
  decoding of `.ics` and mail headers (GG5-19); they are not invented text.
- Files from git have no meaningful modification time, and Nugget dates notes and calendar files by
  it (GG5-5), so the harness sets each file's mtime from the date in its name or frontmatter, as a
  user's own files would have.
- Rerun: push to `qa/knowledge` is no longer a trigger; dispatch `QA GG5` with `models`
  (comma-separated manifest ids) and optionally `product_ref` (build the product from another
  branch, harness and corpus unchanged).
