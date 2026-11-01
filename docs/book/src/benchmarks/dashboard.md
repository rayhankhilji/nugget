# Benchmark dashboard

Each chart follows one headline metric of the nightly run (`.github/workflows/nightly.yml`, release
builds on a shared `ubuntu-latest` runner, so read the trend). Points link to their commit; a dashed
line is the target; a gap means no result. Commands: [`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md).

## Results format

`scripts/bench-run.py` writes `bench/results/latest.json` (the README table is generated from it) and,
on the `bench-results` branch, `history/YYYY-MM-DD.json` plus `latest.json`. Each is a JSON object with
`format` (1), `date` (UTC, `YYYY-MM-DD`), `git_commit`, `machine`, `generated_at`, and `runs`: one entry
per run in `scripts/bench-runs.json` order, with `id`, `command` and the full nugget-bench `report`
(valid against `bench/schema/report.schema.json`). `scripts/bench-dashboard.py` turns the history into
the data this page draws.

## Charts

<link rel="stylesheet" href="dashboard.css">

<div id="bench-dashboard">
<noscript>The charts need JavaScript. The raw data is in <a href="history.json">history.json</a>.</noscript>
</div>

<script src="dashboard.js"></script>
