#!/usr/bin/env python3
"""Build the data file behind the benchmark dashboard in the Nugget book.

Reads the nightly history (one `YYYY-MM-DD.json` results file per day, in the format the nightly
workflow writes) and the headline metrics listed in `scripts/bench-runs.json`, and writes
`docs/book/src/benchmarks/history.json`: one series per headline metric, in bench-runs.json order,
each holding one point per day that has an `ok` report with that metric.

Usage:
    python3 scripts/bench-dashboard.py                       # reads bench/results/history
    python3 scripts/bench-dashboard.py --history DIR [--history DIR2 ...]
    python3 scripts/bench-dashboard.py --check               # exit 1 if history.json is stale

Options:
    --history DIR  a directory of YYYY-MM-DD.json files; repeatable. When two directories hold the
                   same date, the later one wins. A missing or empty directory adds nothing.
    --runs FILE    the run list (default: scripts/bench-runs.json).
    --out FILE     where to write (default: docs/book/src/benchmarks/history.json).
    --check        write nothing; exit 1 when FILE differs from what would be written.

Standard library only (Python 3.11+). Output is deterministic: rerunning on the same input gives a
byte-identical file.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/rayhankhilji/nugget"
DATE_FILE = re.compile(r"^(\d{4}-\d{2}-\d{2})\.json$")


def load_history(dirs: list[Path]) -> dict[str, dict]:
    """date -> results document; later directories win on duplicate dates."""
    days: dict[str, dict] = {}
    for d in dirs:
        if not d.is_dir():
            continue
        for f in sorted(d.iterdir()):
            m = DATE_FILE.match(f.name)
            if not m or not f.is_file():
                continue
            try:
                doc = json.loads(f.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as e:
                print(f"bench-dashboard: skipping {f}: {e}", file=sys.stderr)
                continue
            if isinstance(doc, dict):
                days[m.group(1)] = doc
    return days


def number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def build(runs_doc: dict, days: dict[str, dict]) -> dict:
    series = []
    for run in runs_doc.get("runs", []):
        for head in run.get("headline", []):
            s = {
                "run": run["id"],
                "run_title": run.get("title", run["id"]),
                "metric": head["metric"],
                "label": head.get("label", head["metric"]),
                "unit": None,
                "direction": None,
                "target": None,
                "points": [],
            }
            for date in sorted(days):
                doc = days[date]
                entry = next(
                    (r for r in doc.get("runs") or [] if isinstance(r, dict) and r.get("id") == run["id"]),
                    None,
                )
                report = (entry or {}).get("report") or {}
                if report.get("status") != "ok":
                    continue
                metric = next(
                    (m for m in report.get("metrics") or []
                     if isinstance(m, dict) and m.get("name") == head["metric"]),
                    None,
                )
                if metric is None or not number(metric.get("value")):
                    continue
                met = metric.get("target_met")
                s["points"].append({
                    "date": date,
                    "commit": doc.get("git_commit") or report.get("git_commit") or None,
                    "value": metric["value"],
                    "target_met": met if isinstance(met, bool) else None,
                })
                # Unit, direction and target follow the newest report that has the metric.
                s["unit"] = metric.get("unit")
                s["direction"] = metric.get("direction")
                s["target"] = metric["target"] if number(metric.get("target")) else None
            series.append(s)
    return {
        "format": 1,
        "generated_from": len(days),
        "repository": REPOSITORY,
        "series": series,
    }


def render(data: dict) -> str:
    """JSON with one series header and one point per line, so nightly diffs stay small."""
    dump = lambda v: json.dumps(v, ensure_ascii=False, separators=(", ", ": "))
    out = ["{"]
    for key in ("format", "generated_from", "repository"):
        out.append(f'  "{key}": {dump(data[key])},')
    out.append('  "series": [')
    for i, s in enumerate(data["series"]):
        out.append("    {")
        for key in ("run", "run_title", "metric", "label", "unit", "direction", "target"):
            out.append(f'      "{key}": {dump(s[key])},')
        if s["points"]:
            out.append('      "points": [')
            pts = [f"        {dump(p)}" for p in s["points"]]
            out.append(",\n".join(pts))
            out.append("      ]")
        else:
            out.append('      "points": []')
        out.append("    }" + ("," if i < len(data["series"]) - 1 else ""))
    out.append("  ]")
    out.append("}")
    return "\n".join(out) + "\n"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--history", action="append", type=Path,
                   help="directory of YYYY-MM-DD.json files (repeatable; later wins)")
    p.add_argument("--runs", type=Path, default=ROOT / "scripts" / "bench-runs.json")
    p.add_argument("--out", type=Path, default=ROOT / "docs" / "book" / "src" / "benchmarks" / "history.json")
    p.add_argument("--check", action="store_true", help="exit 1 if --out is stale")
    a = p.parse_args(argv)

    dirs = a.history or [ROOT / "bench" / "results" / "history"]
    runs_doc = json.loads(a.runs.read_text(encoding="utf-8"))
    data = build(runs_doc, load_history(dirs))
    text = render(data)

    if a.check:
        try:
            current = a.out.read_text(encoding="utf-8")
        except OSError:
            current = None
        if current != text:
            print(f"bench-dashboard: {a.out} is out of date; re-run "
                  "`python3 scripts/bench-dashboard.py` with the same --history.", file=sys.stderr)
            return 1
        print(f"bench-dashboard: {a.out} is up to date")
        return 0

    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(text, encoding="utf-8")
    print(f"bench-dashboard: wrote {a.out} ({len(data['series'])} series, {data['generated_from']} days)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
