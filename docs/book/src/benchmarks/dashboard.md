# Benchmark dashboard

Each chart below follows one headline metric of the nightly benchmark run over time. The runs and
their headline metrics are listed in `scripts/bench-runs.json`; the same metrics head the results
table in the README.

**Where the numbers come from.** Every night `.github/workflows/nightly.yml` runs each benchmark
with `cargo run --release -p nugget-bench` on a GitHub-hosted `ubuntu-latest` runner, a release
build on a shared virtual machine. Timing metrics therefore carry some run-to-run noise: read the
trend, not a single night. Each night's results are saved to the `bench-results` branch as
`history/YYYY-MM-DD.json`, and `scripts/bench-dashboard.py` turns that history into the data file
this page draws. None of the runs needs a secret or the network.

**Reading the charts.** Each point is one nightly run; hover it for the date, value and commit, and
click it to open that commit on GitHub. A dashed line marks the metric's target, when it has one.
A gap means the run failed, was not implemented yet, or did not report that metric that night.

**Reproducing a number.** Check out the commit, then run the command shown for that run in
[`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md). See
[Benchmarks](../architecture/benchmarks.md) for what each suite measures and how to read it.

## Results format

`scripts/bench-run.py` writes one results file per night: `bench/results/latest.json` in the
repository (the README table is generated from it), and `history/YYYY-MM-DD.json` plus
`latest.json` on the `bench-results` branch. Each is a JSON object with `format` (1), `date`
(UTC, `YYYY-MM-DD`), `git_commit` (the commit benchmarked), `generated_at`, and `runs`: one entry
per run that produced a report, in `scripts/bench-runs.json` order, each with `id`, `command` and
the full nugget-bench `report` (which validates against `bench/schema/report.schema.json`).

## Charts

<link rel="stylesheet" href="dashboard.css">

<div id="bench-dashboard">
<noscript>The charts need JavaScript. The raw data is in <a href="history.json">history.json</a>.</noscript>
</div>

<script src="dashboard.js"></script>
