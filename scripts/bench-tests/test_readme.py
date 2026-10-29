"""Tests for scripts/bench-readme.py. Run: python3 -m unittest discover -s scripts/bench-tests -v"""

from __future__ import annotations

import contextlib
import copy
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "bench-readme.py"
_spec = importlib.util.spec_from_file_location("bench_readme", SCRIPT)
br = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(br)

SHA = "308f775c0ffee1234567890abcdef1234567890a"
ENV = {"os": "linux", "arch": "x86_64", "cpus": 4, "build_profile": "release"}

RUNS = {
    "format": 1,
    "runs": [
        {"id": "knowledge", "title": "Knowledge graph (M2)", "args": ["knowledge"],
         "headline": [
             {"metric": "claims.f1", "label": "Claim extraction F1"},
             {"metric": "claims.unsupported_rate", "label": "Unsupported claims"},
             {"metric": "mcp.get_context.p50_ms", "label": "MCP get_context p50"}]},
        {"id": "perf", "title": "Store | retriever", "args": ["perf", "--scale", "quick"],
         "headline": [
             {"metric": "ingest.items_per_s", "label": "Ingest throughput"},
             {"metric": "query.p95_us", "label": "Search p95"}]},
        {"id": "proactive", "title": "Proactive", "args": ["proactive"],
         "headline": [{"metric": "actions.executed_without_approval", "label": "Unapproved"}]},
        {"id": "draft", "title": "Draft", "args": ["draft"],
         "headline": [{"metric": "draft.win_rate", "label": "Win rate"}]},
        {"id": "cua-xvfb", "title": "CUA Xvfb", "args": ["cua", "--cua-env", "xvfb"],
         "linux_only": True, "setup": "sudo bench/cua/env/setup-debian.sh",
         "headline": [{"metric": "success_rate", "label": "Verified task success"}]},
    ],
}


def metric(name, value, unit, direction="higher_is_better", target=None, met=None):
    return {"name": name, "value": value, "unit": unit, "direction": direction,
            "target": target, "target_met": met, "description": "d"}


def report(suite, metrics, status="ok", targets=None):
    r = {"schema_version": 1, "suite": suite, "status": status,
         "started_at": "2026-10-29T03:00:00Z", "finished_at": "2026-10-29T03:01:00Z",
         "duration_ms": 60000, "harness_version": "0.1.0", "git_commit": SHA,
         "environment": dict(ENV), "config": {"scale": "quick", "seed": 42, "params": {}},
         "metrics": metrics, "message": None if status == "ok" else "boom"}
    if targets is not None:
        r["targets"] = targets
    return r


def latest_fixture():
    return {
        "format": 1, "date": "2026-10-29", "git_commit": SHA,
        "generated_at": "2026-10-29T03:12:00Z",
        "runs": [
            {"id": "knowledge", "command": "x", "report": report("knowledge", [
                metric("claims.f1", 0.9123, "ratio", target=0.85, met=True),
                metric("claims.unsupported_rate", 0.07, "ratio", "lower_is_better", 0.02, False),
                metric("mcp.get_context.p50_ms", 12.345, "ms", "lower_is_better", 50, True),
            ])},
            {"id": "perf", "command": "x", "report": report("perf", [
                metric("ingest.items_per_s", 12345.6, "items/s", "higher_is_better"),
                # query.p95_us deliberately missing
            ])},
            {"id": "proactive", "command": "x", "report": report("proactive", [
                metric("actions.executed_without_approval", 0, "count", "lower_is_better", 0, True),
            ], targets=[{"metric": "actions.executed_without_approval", "comparison": "at_most",
                         "target": 0, "value": 0, "pass": True}])},
            {"id": "draft", "command": "x", "report": report("draft", [], status="not_implemented")},
            # cua-xvfb missing entirely
        ],
    }


def rows(block):
    return [line for line in block.splitlines() if line.startswith("| ") and "---" not in line][1:]


def cells(row):
    # Split on unescaped pipes.
    out, cur, i = [], "", 0
    while i < len(row):
        if row[i] == "\\" and i + 1 < len(row) and row[i + 1] == "|":
            cur += "\\|"
            i += 2
            continue
        if row[i] == "|":
            out.append(cur.strip())
            cur = ""
        else:
            cur += row[i]
        i += 1
    return out[1:]


class FormatTests(unittest.TestCase):
    def test_ratio_as_percent(self):
        self.assertEqual(br.format_value(0.9123, "ratio"), "91.2%")
        self.assertEqual(br.format_value(1.0, "ratio"), "100%")
        self.assertEqual(br.format_value(0, "ratio"), "0%")

    def test_units(self):
        self.assertEqual(br.format_value(12.345, "ms"), "12.3 ms")
        self.assertEqual(br.format_value(1.5, "s"), "1.50 s")
        self.assertEqual(br.format_value(0.01234, "s"), "0.0123 s")
        self.assertEqual(br.format_value(842.7, "us"), "843 us")
        self.assertEqual(br.format_value(12345.6, "items/s"), "12,346 items/s")
        self.assertEqual(br.format_value(3, "count"), "3")
        self.assertEqual(br.format_value(2.0, "ms"), "2 ms")
        self.assertEqual(br.format_value(None, "ms"), "n/a")

    def test_targets(self):
        r = report("x", [])
        self.assertEqual(br.format_target(metric("a", 1, "ratio", target=0.85, met=True), r), "≥ 85% ✓")
        self.assertEqual(br.format_target(
            metric("a", 1, "ms", "lower_is_better", 50, False), r), "≤ 50 ms ✗")
        self.assertEqual(br.format_target(metric("a", 1, "ms", target=None), r), "—")
        self.assertEqual(br.format_target(metric("a", 1, "count", target=2), r), "≥ 2")
        r2 = report("x", [], targets=[{"metric": "a", "comparison": "at_most", "target": 2,
                                       "value": 1, "pass": True}])
        self.assertEqual(br.format_target(metric("a", 1, "count", "info", 2, True), r2), "≤ 2 ✓")

    def test_command(self):
        self.assertEqual(br.run_command(RUNS["runs"][0]),
                         "cargo run --release -p nugget-bench -- knowledge")
        self.assertEqual(br.run_command(RUNS["runs"][4]),
                         "sudo bench/cua/env/setup-debian.sh && "
                         "cargo run --release -p nugget-bench -- cua --cua-env xvfb")


class RenderTests(unittest.TestCase):
    def setUp(self):
        self.block = br.render(latest_fixture(), RUNS)
        self.rows = [cells(r) for r in rows(self.block)]

    def test_provenance(self):
        first = self.block.splitlines()[0]
        self.assertIn("2026-10-29", first)
        self.assertIn(f"[`308f775`](https://github.com/rayhankhilji/nugget/commit/{SHA})", first)
        self.assertIn("linux/x86_64, 4 CPUs, release build", first)
        self.assertIn("GitHub-hosted Linux", first)  # the default when `machine` is absent

    def test_machine_from_results(self):
        latest = latest_fixture()
        latest["machine"] = "a local machine"
        first = br.render(latest, RUNS).splitlines()[0]
        self.assertIn("on a local machine (", first)
        self.assertNotIn("GitHub-hosted", first)
        self.assertIn("(docs/book/src/benchmarks/dashboard.md)", first)
        self.assertIn("bench-results", self.block.splitlines()[-1])

    def test_one_row_per_headline_in_order(self):
        names = [r[1] for r in self.rows]
        expected = [h["metric"] for run in RUNS["runs"] for h in run["headline"]]
        self.assertEqual(len(names), len(expected))
        for n, e in zip(names, expected):
            self.assertIn(f"`{e}`", n)

    def test_values_and_targets(self):
        self.assertEqual(self.rows[0][2:4], ["91.2%", "≥ 85% ✓"])
        self.assertEqual(self.rows[1][2:4], ["7%", "≤ 2% ✗"])
        self.assertEqual(self.rows[2][2:4], ["12.3 ms", "≤ 50 ms ✓"])
        self.assertEqual(self.rows[3][2:4], ["12,346 items/s", "—"])
        self.assertEqual(self.rows[5][2:4], ["0", "≤ 0 ✓"])

    def test_na_cases(self):
        self.assertEqual(self.rows[4][2:4], ["n/a (missing)", "—"])
        self.assertEqual(self.rows[6][2:4], ["n/a (not_implemented)", "—"])
        self.assertEqual(self.rows[7][2:4], ["n/a (not run)", "—"])

    def test_suite_and_reproduce_on_first_row_only(self):
        self.assertEqual(self.rows[0][0], "Knowledge graph (M2)")
        self.assertEqual(self.rows[0][4], "`cargo run --release -p nugget-bench -- knowledge`")
        self.assertEqual(self.rows[1][0], "")
        self.assertEqual(self.rows[1][4], "")
        self.assertEqual(self.rows[7][0], "CUA Xvfb (Linux only)")
        self.assertIn("setup-debian.sh && cargo run", self.rows[7][4])

    def test_pipe_escaped(self):
        self.assertEqual(self.rows[3][0], "Store \\| retriever")

    def test_missing_commit_and_runs(self):
        latest = {"format": 1, "date": "2026-10-29", "git_commit": None, "runs": []}
        block = br.render(latest, RUNS)
        self.assertIn("unrecorded commit", block)
        self.assertTrue(all(cells(r)[2] == "n/a (not run)" for r in rows(block)))

    def test_deterministic(self):
        changed = latest_fixture()
        changed["generated_at"] = "2030-01-01T00:00:00Z"
        self.assertEqual(br.render(changed, RUNS), self.block)


class CliTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = Path(self.tmp.name)
        self.latest, self.runs, self.readme = d / "latest.json", d / "runs.json", d / "README.md"
        self.latest.write_text(json.dumps(latest_fixture()), encoding="utf-8")
        self.runs.write_text(json.dumps(RUNS), encoding="utf-8")
        self.readme.write_text(
            f"# T\n\n## Benchmarks\n\nIntro.\n\n{br.BEGIN}\nplaceholder\n{br.END}\n\n## Next\n",
            encoding="utf-8")

    def tearDown(self):
        self.tmp.cleanup()

    def run_main(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = br.main(["--latest", str(self.latest), "--runs", str(self.runs),
                            "--readme", str(self.readme), *extra])
        return code, out.getvalue(), err.getvalue()

    def test_check_fails_when_stale_then_passes(self):
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn(br.STALE_MSG, err)
        self.assertEqual(self.run_main()[0], 0)
        self.assertEqual(self.run_main("--check")[0], 0)

    def test_replacement_is_idempotent_and_keeps_surroundings(self):
        self.run_main()
        once = self.readme.read_text(encoding="utf-8")
        self.run_main()
        self.assertEqual(self.readme.read_text(encoding="utf-8"), once)
        self.assertTrue(once.startswith("# T\n\n## Benchmarks\n\nIntro.\n\n" + br.BEGIN + "\n"))
        self.assertTrue(once.endswith(br.END + "\n\n## Next\n"))
        self.assertNotIn("placeholder", once)

    def test_check_detects_changed_results(self):
        self.run_main()
        data = latest_fixture()
        data["runs"][0]["report"]["metrics"][0]["value"] = 0.5
        self.latest.write_text(json.dumps(data), encoding="utf-8")
        self.assertEqual(self.run_main("--check")[0], 1)

    def test_missing_markers(self):
        self.readme.write_text("# T\nno markers\n", encoding="utf-8")
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("marker", err)
        before = self.readme.read_text(encoding="utf-8")
        self.assertEqual(self.run_main()[0], 1)
        self.assertEqual(self.readme.read_text(encoding="utf-8"), before)

    def test_markers_out_of_order(self):
        self.readme.write_text(f"{br.END}\n{br.BEGIN}\n", encoding="utf-8")
        self.assertEqual(self.run_main("--check")[0], 1)

    def test_missing_latest(self):
        self.latest.unlink()
        code, _, err = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("results file not found", err)

    def test_print_does_not_write(self):
        before = self.readme.read_text(encoding="utf-8")
        code, out, _ = self.run_main("--print")
        self.assertEqual(code, 0)
        self.assertIn("| Suite | Metric | Value | Target | Reproduce |", out)
        self.assertEqual(self.readme.read_text(encoding="utf-8"), before)


class RepoConfigTests(unittest.TestCase):
    def test_real_runs_file_renders(self):
        runs = json.loads((SCRIPT.parent / "bench-runs.json").read_text(encoding="utf-8"))
        block = br.render({"format": 1, "date": "2026-10-29", "git_commit": SHA, "runs": []}, runs)
        expected = sum(len(r["headline"]) for r in runs["runs"])
        self.assertEqual(len(rows(block)), expected)


if __name__ == "__main__":
    unittest.main()
