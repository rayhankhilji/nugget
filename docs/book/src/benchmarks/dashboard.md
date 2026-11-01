# Benchmark dashboard

Each chart follows one headline metric of the nightly benchmark run (listed in
`scripts/bench-runs.json`). `.github/workflows/nightly.yml` runs them as release builds on a shared
`ubuntu-latest` runner, so timings are noisy: read the trend. Each point is one night (hover for date,
value and commit; click to open the commit); a dashed line is the target; a gap means no result that
night. To reproduce a number, check out the commit and run its command from
[`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md).

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
