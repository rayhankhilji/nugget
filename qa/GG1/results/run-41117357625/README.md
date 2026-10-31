# Real-model run 41117357625 (+ hybrid run 41117471532)

- https://github.com/rayhankhilji/nugget/actions/runs/41117357625 (commit ea46d10): 177 scenarios,
  qwen3-4b-q4km and qwen3-1.7b-q8 from the signed manifest via nugget-runtime llama-server,
  read-ahead off/on, ubuntu-latest (4 vCPU AMD EPYC 9V74, 16 GB) and macos-latest (Apple M1
  virtual, 3 cores, 7 GB). 4b-ra-off has 2 repeats per scenario.
- `drafts-4b-embed-ra-off.jsonl`: https://github.com/rayhankhilji/nugget/actions/runs/41117471532
  (commit 73df19c), same with hybrid retrieval (`--embed`, the manifest's nomic-embed-text-v1.5).
- `check.txt`, `checks.json`: `qa/GG1/check.py` over all of them. Judge: `judged.jsonl` (if present).
