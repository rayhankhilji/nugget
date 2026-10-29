#!/usr/bin/env python3
"""Render the Benchmarks table in README.md from the latest nightly benchmark results.

Usage:
    python3 scripts/bench-readme.py            # rewrite the table between the bench markers
    python3 scripts/bench-readme.py --check    # exit 1 if README.md is stale (CI)
    python3 scripts/bench-readme.py --print    # print the rendered block, change nothing

Options (paths are relative to the repository root unless absolute):
    --latest PATH   results file (default bench/results/latest.json)
    --runs PATH     runs and headline metrics (default scripts/bench-runs.json)
    --readme PATH   file to update (default README.md)

The table lives between `<!-- bench:begin -->` and `<!-- bench:end -->`. It has one row per
headline metric in scripts/bench-runs.json, in that order, and each run's exact command to
reproduce it. The output depends only on the two input files (no clock, no environment), so
`--check` is stable. The nightly workflow publishes latest.json on the `bench-results` branch.
Python 3.11+ standard library only.
"""

from __future__ import annotations

import argparse
import json
import math
import shlex
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BEGIN = "<!-- bench:begin -->"
END = "<!-- bench:end -->"
COMMIT_URL = "https://github.com/rayhankhilji/nugget/commit/{sha}"
DASHBOARD = "docs/book/src/benchmarks/dashboard.md"
BENCH_CMD = "cargo run --release -p nugget-bench --"
STALE_MSG = "README.md benchmarks table is stale; run python3 scripts/bench-readme.py"


class BenchReadmeError(Exception):
    """A problem with the inputs or the README markers."""


def load_json(path: Path, what: str) -> dict:
    try:
        with path.open(encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        raise BenchReadmeError(f"{what} not found: {path}") from None
    except json.JSONDecodeError as e:
        raise BenchReadmeError(f"{what} is not valid JSON: {path}: {e}") from None
    if not isinstance(data, dict):
        raise BenchReadmeError(f"{what} must be a JSON object: {path}")
    return data


# ---------------------------------------------------------------- formatting


def _num(v: float) -> str:
    """A number with sensible precision: integers plain, others to about three figures."""
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
        return str(v)
    if float(v).is_integer() and abs(v) < 1e15:
        return f"{int(v):,}"
    a = abs(v)
    if a >= 100:
        return f"{v:,.0f}"
    if a >= 10:
        return f"{v:.1f}"
    if a >= 1:
        return f"{v:.2f}"
    return f"{v:.3g}"


def format_value(value, unit: str) -> str:
    """Format a metric value with its unit. Ratios (0..1) are shown as percentages."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return "n/a"
    if unit == "ratio":
        pct = round(value * 100, 9)
        if float(pct).is_integer():
            return f"{int(pct)}%"
        return f"{pct:.1f}%"
    if unit == "count":
        return _num(value)
    return f"{_num(value)} {unit}" if unit else _num(value)


def _comparison(metric: dict, report: dict) -> str | None:
    for t in report.get("targets") or []:
        if t.get("metric") == metric.get("name"):
            return {"at_least": "≥", "at_most": "≤"}.get(t.get("comparison"))
    return {"higher_is_better": "≥", "lower_is_better": "≤"}.get(metric.get("direction"), "=")


def format_target(metric: dict, report: dict) -> str:
    target = metric.get("target")
    if target is None:
        return "—"
    text = f"{_comparison(metric, report)} {format_value(target, metric.get('unit', ''))}"
    met = metric.get("target_met")
    if met is True:
        text += " ✓"
    elif met is False:
        text += " ✗"
    return text


def escape_cell(text: str) -> str:
    return " ".join(str(text).split()).replace("|", "\\|")


def run_command(run: dict) -> str:
    cmd = f"{BENCH_CMD} {shlex.join(run.get('args') or [])}".rstrip()
    if run.get("setup"):
        cmd = f"{run['setup']} && {cmd}"
    return cmd


# ---------------------------------------------------------------- rendering


def _provenance(latest: dict) -> str:
    date = latest.get("date") or "unknown date"
    sha = latest.get("git_commit")
    if sha:
        commit = f"commit [`{sha[:7]}`]({COMMIT_URL.format(sha=sha)})"
    else:
        commit = "an unrecorded commit"
    envs: list[str] = []
    for entry in latest.get("runs") or []:
        env = (entry.get("report") or {}).get("environment") or {}
        if not env:
            continue
        cpus = env.get("cpus")
        text = (f"{env.get('os', '?')}/{env.get('arch', '?')}, "
                f"{cpus} CPU{'' if cpus == 1 else 's'}, {env.get('build_profile', '?')} build")
        if text not in envs:
            envs.append(text)
    env_text = "; ".join(envs) if envs else "environment not recorded"
    machine = latest.get("machine") or "a GitHub-hosted Linux runner"
    return (f"Run of {date} at {commit} on {machine} ({env_text}). "
            f"Full history: [benchmark dashboard]({DASHBOARD}).")


def render(latest: dict, runs_cfg: dict) -> str:
    """The Markdown that goes between the markers (no leading or trailing newline)."""
    by_id: dict[str, dict] = {}
    for entry in latest.get("runs") or []:
        if isinstance(entry, dict) and entry.get("id") not in by_id:
            by_id[entry.get("id")] = entry

    lines = [
        _provenance(latest),
        "",
        "| Suite | Metric | Value | Target | Reproduce |",
        "|---|---|---|---|---|",
    ]
    for run in runs_cfg.get("runs") or []:
        entry = by_id.get(run.get("id"))
        report = (entry or {}).get("report") or {}
        status = report.get("status")
        metrics = {m.get("name"): m for m in report.get("metrics") or [] if isinstance(m, dict)}
        title = run.get("title") or run.get("id", "")
        if run.get("linux_only") and "linux" not in title.lower():
            title += " (Linux only)"
        repro = f"`{run_command(run)}`"
        for i, head in enumerate(run.get("headline") or []):
            name = head.get("metric", "")
            label = head.get("label") or name
            metric = metrics.get(name)
            if entry is None:
                value, target = "n/a (not run)", "—"
            elif status != "ok":
                value, target = f"n/a ({status or 'no status'})", "—"
            elif metric is None:
                value, target = "n/a (missing)", "—"
            else:
                value = format_value(metric.get("value"), metric.get("unit", ""))
                target = format_target(metric, report)
            cells = [
                escape_cell(title) if i == 0 else "",
                f"{escape_cell(label)} (`{escape_cell(name)}`)",
                escape_cell(value),
                escape_cell(target),
                escape_cell(repro) if i == 0 else "",
            ]
            lines.append("| " + " | ".join(cells) + " |")
    lines += [
        "",
        "To refresh: `python3 scripts/bench-readme.py` with a new `bench/results/latest.json` "
        "(the nightly workflow publishes `latest.json` on the `bench-results` branch).",
    ]
    return "\n".join(lines)


def split_markers(text: str) -> tuple[str, str, str]:
    """Return (before-and-begin-marker, current block, end-marker-and-after)."""
    if text.count(BEGIN) != 1 or text.count(END) != 1:
        raise BenchReadmeError(
            f"README must contain exactly one {BEGIN} and one {END} marker")
    b = text.index(BEGIN) + len(BEGIN)
    e = text.index(END)
    if e < b:
        raise BenchReadmeError(f"{END} comes before {BEGIN} in the README")
    return text[:b], text[b:e], text[e:]


def replace_block(text: str, block: str) -> str:
    head, _, tail = split_markers(text)
    return f"{head}\n{block}\n{tail}"


# ---------------------------------------------------------------- CLI


def _path(p: str) -> Path:
    path = Path(p)
    return path if path.is_absolute() else REPO / path


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--latest", default="bench/results/latest.json")
    ap.add_argument("--runs", default="scripts/bench-runs.json")
    ap.add_argument("--readme", default="README.md")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="exit 1 if the README is stale")
    mode.add_argument("--print", action="store_true", help="print the block, change nothing")
    args = ap.parse_args(argv)

    try:
        block = render(load_json(_path(args.latest), "results file"),
                       load_json(_path(args.runs), "runs file"))
        if args.print:
            print(block)
            return 0
        readme = _path(args.readme)
        try:
            text = readme.read_text(encoding="utf-8")
        except FileNotFoundError:
            raise BenchReadmeError(f"README not found: {readme}") from None
        new = replace_block(text, block)
    except BenchReadmeError as e:
        print(f"bench-readme: {e}", file=sys.stderr)
        if args.check:
            print(STALE_MSG, file=sys.stderr)
        return 1

    if args.check:
        if new != text:
            print(STALE_MSG, file=sys.stderr)
            return 1
        print("README.md benchmarks table is up to date")
        return 0
    if new != text:
        readme.write_text(new, encoding="utf-8")
        print(f"updated {readme}")
    else:
        print(f"{readme} already up to date")
    return 0


if __name__ == "__main__":
    sys.exit(main())
