"""Tests for scripts/bench-dashboard.py. Run: python3 -m unittest discover -s scripts/bench-tests -v"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "bench-dashboard.py"
spec = importlib.util.spec_from_file_location("bench_dashboard", SCRIPT)
dash = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dash)

RUNS = {
    "format": 1,
    "runs": [
        {"id": "alpha", "title": "Alpha run", "args": ["alpha"], "headline": [
            {"metric": "a.f1", "label": "A F1"},
            {"metric": "a.p50_ms", "label": "A p50"},
        ]},
        {"id": "beta", "title": "Beta run", "args": ["beta"], "headline": [
            {"metric": "b.rate", "label": "B rate"},
        ]},
    ],
}


def metric(name, value, unit="ratio", direction="higher_is_better", target=None, met=None):
    return {"name": name, "value": value, "unit": unit, "direction": direction,
            "target": target, "target_met": met, "description": ""}


def run(rid, metrics, status="ok"):
    return {"id": rid, "command": f"cargo run --release -p nugget-bench -- {rid}",
            "report": {"status": status, "metrics": metrics}}


def results(date, commit, runs):
    return {"format": 1, "date": date, "git_commit": commit,
            "generated_at": f"{date}T03:00:00Z", "runs": runs}


class DashboardTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.runs = self.dir / "runs.json"
        self.runs.write_text(json.dumps(RUNS))
        self.out = self.dir / "out" / "history.json"

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, sub, doc):
        d = self.dir / sub
        d.mkdir(exist_ok=True)
        (d / f"{doc['date']}.json").write_text(json.dumps(doc))
        return d

    def main(self, *args):
        err = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            code = dash.main(["--runs", str(self.runs), "--out", str(self.out), *args])
        return code, err.getvalue()

    def generate(self, *dirs):
        args = [a for d in dirs for a in ("--history", str(d))]
        self.assertEqual(self.main(*args)[0], 0)
        return json.loads(self.out.read_text())

    def series(self, data, rid, name):
        return next(s for s in data["series"] if s["run"] == rid and s["metric"] == name)

    def test_series_follow_runs_file_order(self):
        data = self.generate(self.dir / "missing")
        self.assertEqual(
            [(s["run"], s["metric"]) for s in data["series"]],
            [("alpha", "a.f1"), ("alpha", "a.p50_ms"), ("beta", "b.rate")],
        )
        self.assertEqual(data["format"], 1)
        self.assertEqual(data["generated_from"], 0)
        self.assertEqual(data["repository"], "https://github.com/rayhankhilji/nugget")
        self.assertTrue(all(s["points"] == [] and s["target"] is None for s in data["series"]))
        self.assertEqual(self.series(data, "alpha", "a.f1")["run_title"], "Alpha run")
        self.assertEqual(self.series(data, "alpha", "a.f1")["label"], "A F1")

    def test_points_sorted_by_date_with_commit_and_target(self):
        h = self.dir / "h"
        for date, commit, v in [("2026-10-03", "ccc", 0.9), ("2026-10-01", "aaa", 0.7),
                                ("2026-10-02", None, 0.8)]:
            h = self.write("h", results(date, commit, [
                run("alpha", [metric("a.f1", v, target=0.85, met=v >= 0.85)])]))
        s = self.series(self.generate(h), "alpha", "a.f1")
        self.assertEqual([p["date"] for p in s["points"]], ["2026-10-01", "2026-10-02", "2026-10-03"])
        self.assertEqual([p["commit"] for p in s["points"]], ["aaa", None, "ccc"])
        self.assertEqual([p["target_met"] for p in s["points"]], [False, False, True])
        self.assertEqual((s["unit"], s["direction"], s["target"]), ("ratio", "higher_is_better", 0.85))

    def test_skips_failed_reports_missing_runs_and_metrics(self):
        self.write("h", results("2026-10-01", "a1", [
            run("alpha", [metric("a.f1", 0.5)]), run("beta", [], status="failed")]))
        self.write("h", results("2026-10-02", "a2", [
            run("alpha", [metric("a.p50_ms", 12, unit="ms", direction="lower_is_better")])]))
        self.write("h", results("2026-10-03", "a3", [
            run("alpha", [metric("a.f1", 0.6)], status="not_implemented")]))
        h = self.write("h", results("2026-10-04", "a4", [
            run("beta", [metric("b.rate", None), metric("other", 1)])]))
        (h / "notes.json").write_text("{}")  # not a dated file: ignored
        data = self.generate(h)
        self.assertEqual(data["generated_from"], 4)
        self.assertEqual([p["date"] for p in self.series(data, "alpha", "a.f1")["points"]], ["2026-10-01"])
        p50 = self.series(data, "alpha", "a.p50_ms")
        self.assertEqual([p["value"] for p in p50["points"]], [12])
        self.assertEqual((p50["unit"], p50["direction"]), ("ms", "lower_is_better"))
        self.assertEqual(self.series(data, "beta", "b.rate")["points"], [])

    def test_later_history_dir_wins_on_same_date(self):
        old = self.write("old", results("2026-10-01", "old", [run("beta", [metric("b.rate", 0.1)])]))
        self.write("old", results("2026-10-02", "keep", [run("beta", [metric("b.rate", 0.2)])]))
        new = self.write("new", results("2026-10-01", "new", [run("beta", [metric("b.rate", 0.9)])]))
        pts = self.series(self.generate(old, new), "beta", "b.rate")["points"]
        self.assertEqual([(p["commit"], p["value"]) for p in pts], [("new", 0.9), ("keep", 0.2)])
        pts = self.series(self.generate(new, old), "beta", "b.rate")["points"]
        self.assertEqual(pts[0]["commit"], "old")

    def test_output_is_deterministic_and_readable(self):
        h = self.write("h", results("2026-10-01", "x", [run("alpha", [metric("a.f1", 0.5)])]))
        self.generate(h)
        first = self.out.read_text()
        self.generate(h)
        self.assertEqual(first, self.out.read_text())
        self.assertTrue(first.endswith("}\n"))
        self.assertIn('        {"date": "2026-10-01", "commit": "x", "value": 0.5, "target_met": null}\n',
                      first)

    def test_check_reports_stale_and_fresh(self):
        h = self.write("h", results("2026-10-01", "x", [run("alpha", [metric("a.f1", 0.5)])]))
        code, err = self.main("--history", str(h), "--check")
        self.assertEqual(code, 1)  # no file yet
        self.assertIn("re-run", err)
        self.generate(h)
        self.assertEqual(self.main("--history", str(h), "--check")[0], 0)
        self.write("h", results("2026-10-02", "y", [run("alpha", [metric("a.f1", 0.6)])]))
        self.assertEqual(self.main("--history", str(h), "--check")[0], 1)
        self.assertFalse("2026-10-02" in self.out.read_text())  # --check writes nothing


if __name__ == "__main__":
    unittest.main()
