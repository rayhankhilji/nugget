#!/usr/bin/env python3
"""Run the benchmark suites listed in scripts/bench-runs.json and collect one results file.

    python3 scripts/bench-run.py                          # every run, writes bench/results/latest.json
    python3 scripts/bench-run.py --only knowledge,draft   # a subset
    python3 scripts/bench-run.py --out-dir "$RUNNER_TEMP/bench" --history-dir history

Each run is `cargo run --release --locked -p nugget-bench -- <args> --out-dir <reports>/<id>`, so
its report lands in a directory of its own. The results file (format: see
docs/book/src/benchmarks/dashboard.md) lists every run in bench-runs.json order with its command
and its full report. Runs marked `linux_only` are skipped elsewhere.

`--collect-only` skips running and collects reports already in `<out-dir>/reports`.

Exit status: 0 when every run produced a report with status `ok`, 1 otherwise. The results file is
written either way, so a partial night still has a history entry. Standard library only.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FORMAT = 1


def load_runs(path: Path) -> list[dict]:
    return json.loads(path.read_text(encoding="utf-8"))["runs"]


def command_for(run: dict) -> str:
    """The command a reader runs to reproduce one row (without the CI-only flags)."""
    return "cargo run --release -p nugget-bench -- " + shlex.join(run["args"])


def latest_report(directory: Path) -> dict | None:
    """The newest report in a run's directory (report names start with a UTC timestamp)."""
    reports = sorted(directory.glob("*.json")) if directory.is_dir() else []
    if not reports:
        return None
    return json.loads(reports[-1].read_text(encoding="utf-8"))


def run_one(run: dict, reports: Path) -> int:
    out = reports / run["id"]
    out.mkdir(parents=True, exist_ok=True)
    cmd = ["cargo", "run", "--release", "--locked", "-p", "nugget-bench", "--"]
    cmd += run["args"] + ["--out-dir", str(out)]
    print(f"::group::{run['id']}: {shlex.join(cmd)}", file=sys.stderr, flush=True)
    code = subprocess.call(cmd, cwd=ROOT)
    print("::endgroup::", file=sys.stderr, flush=True)
    return code


def collect(runs: list[dict], reports: Path, date: str, commit: str | None) -> tuple[dict, list[str]]:
    entries, problems = [], []
    for run in runs:
        report = latest_report(reports / run["id"])
        if report is None:
            problems.append(f"{run['id']}: no report")
            continue
        if report.get("status") != "ok":
            problems.append(f"{run['id']}: status {report.get('status')}: {report.get('message')}")
        entries.append({"id": run["id"], "command": command_for(run), "report": report})
    commits = {e["report"].get("git_commit") for e in entries} - {None}
    if commit is None and len(commits) == 1:
        commit = commits.pop()
    results = {
        "format": FORMAT,
        "date": date,
        "git_commit": commit,
        "generated_at": dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "runs": entries,
    }
    return results, problems


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=Path, default=ROOT / "scripts" / "bench-runs.json")
    parser.add_argument("--only", help="comma-separated run ids (default: all)")
    parser.add_argument("--out-dir", type=Path, default=ROOT / "bench" / "results",
                        help="reports go to <out-dir>/reports/<id>/ (default bench/results)")
    parser.add_argument("--latest", type=Path, help="results file (default <out-dir>/latest.json)")
    parser.add_argument("--history-dir", type=Path, help="also write <history-dir>/<date>.json")
    parser.add_argument("--date", help="UTC date of the run, YYYY-MM-DD (default today)")
    parser.add_argument("--commit", help="commit benchmarked (default: the reports' git_commit)")
    parser.add_argument("--collect-only", action="store_true", help="do not run, only collect")
    args = parser.parse_args()

    runs = load_runs(args.runs)
    if args.only:
        wanted = set(args.only.split(","))
        unknown = wanted - {r["id"] for r in runs}
        if unknown:
            parser.error(f"unknown run ids: {', '.join(sorted(unknown))}")
        runs = [r for r in runs if r["id"] in wanted]
    if not sys.platform.startswith("linux"):
        runs = [r for r in runs if not r.get("linux_only")]

    reports = args.out_dir / "reports"
    failed = []
    if not args.collect_only:
        for run in runs:
            code = run_one(run, reports)
            if code != 0:
                failed.append(f"{run['id']}: exit code {code}")

    date = args.date or dt.datetime.now(dt.UTC).strftime("%Y-%m-%d")
    results, problems = collect(runs, reports, date, args.commit or os.environ.get("NUGGET_BENCH_COMMIT"))
    latest = args.latest or args.out_dir / "latest.json"
    write_json(latest, results)
    print(f"wrote {latest} ({len(results['runs'])} runs)", file=sys.stderr)
    if args.history_dir:
        write_json(args.history_dir / f"{date}.json", results)
        print(f"wrote {args.history_dir / (date + '.json')}", file=sys.stderr)

    for problem in failed + problems:
        print(f"error: {problem}", file=sys.stderr)
    return 1 if failed or problems else 0


if __name__ == "__main__":
    sys.exit(main())
