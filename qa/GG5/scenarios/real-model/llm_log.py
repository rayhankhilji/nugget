#!/usr/bin/env python3
"""Summarise a GG5 run's daemon log: knowledge runs per harness phase (items processed, run
time, ms per processed item) and LLM failures per item.

usage: llm_log.py RUN_DIR"""
import json, sys
from collections import Counter, defaultdict
from pathlib import Path

run = Path(sys.argv[1])
steps = [json.loads(l) for l in (run / "log.jsonl").read_text().splitlines() if l.strip()]
# phase boundaries: a phase starts at the step that triggers it and ends at its 'settled'.
starts = {"phase1": next(s["t"] for s in steps if s["step"] == "sources.add")}
for s in steps:
    if s["step"] == "corpus.phase2.copied":
        starts["phase2"] = s["t"]
    if s["step"] == "edits.done":
        starts["edits"] = s["t"]
    if s["step"] == "sources.touched":
        starts["reextract"] = s["t"]
    if s["step"] == "daemon.stopped" and "reextract" in starts and "restart" not in starts and s["t"] > starts["reextract"]:
        starts["restart"] = s["t"]
order = sorted(starts.items(), key=lambda x: x[1])
settled = {s["label"]: s["seconds"] for s in steps if s["step"] == "settled"}


def phase_of(ts):
    p = None
    for name, t in order:
        if ts[:19] >= t[:19]:
            p = name
    return p


runs = defaultdict(lambda: Counter())
fails = defaultdict(list)
for line in (run / "nuggetd.2026-10-30.log").read_text().splitlines():
    r = json.loads(line)
    f = r.get("fields", {})
    m = f.get("message", "")
    ts = r["timestamp"]
    ph = phase_of(ts)
    if m == "knowledge run":
        c = runs[ph]
        c["runs"] += 1
        c["processed"] += f["processed"]
        c["took_ms"] += f["took_ms"]
    elif m == "LLM extraction failed":
        fails[f["item"]].append((ts[11:19], ph, f["error"].split(": ")[-1]))
        runs[ph]["llm_failures"] += 1
print(f"{'phase':10} {'settled_s':>9} {'runs':>4} {'processed':>9} {'run_s':>6} {'s/item':>6} {'llm_fail':>8}")
for name, _ in order:
    c = runs[name]
    per = c["took_ms"] / 1000 / c["processed"] if c["processed"] else 0
    print(f"{name:10} {settled.get(name, '-'):>9} {c['runs']:>4} {c['processed']:>9} {c['took_ms']/1000:>6.0f} {per:>6.1f} {c['llm_failures']:>8}")
tot = sum(runs[n]["llm_failures"] for n, _ in order)
print(f"LLM failures total {tot} on {len(fails)} items:")
for k, v in sorted(fails.items()):
    print(f"  {len(v)}x {k[2:]}: {v}")
