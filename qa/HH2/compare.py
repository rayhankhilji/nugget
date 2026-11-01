#!/usr/bin/env python3
"""HH2: before/after table from two ingest_timeline.py result dirs (markdown on stdout).

Usage: compare.py BEFORE_DIR AFTER_DIR [--hardware FILE]
The runner's CPU comes from --hardware (the workflow's lscpu output), else from the results,
else from lscpu on this machine.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess


def load(d):
    with open(os.path.join(d, "result.json"), encoding="utf-8") as f:
        return json.load(f)


def s(v, timed_out=False, cap=None):
    if v is None:
        return f"not within {cap / 60:.0f} min" if timed_out and cap else "never"
    return f"{v:.0f} s" if v >= 10 else f"{v:.1f} s"


def ms(v):
    return "–" if v is None else f"{v:.0f} ms" if v < 1000 else f"{v / 1000:.1f} s"


def cpu(hw_file, results):
    if hw_file and os.path.exists(hw_file):
        lines = [l.strip() for l in open(hw_file, encoding="utf-8")
                 if l.strip().startswith(("Model name", "CPU(s)"))]
        if lines:
            return "; ".join(" ".join(l.split()) for l in lines)
    for r in results:
        h = r.get("hardware") or {}
        if h.get("Model name"):
            return f"{h['Model name']}, {h.get('CPU(s)', '?')} CPUs"
    try:
        out = subprocess.run(["lscpu"], capture_output=True, text=True, timeout=10).stdout
        return "; ".join(" ".join(l.split()) for l in out.splitlines() if l.startswith(("Model name", "CPU(s)")))
    except Exception:
        return "unknown"


def rows(b, a):
    def both(f):
        return [f(b), f(a)]

    def tri(r):
        t = (r["final"] or {}).get("triage")
        if not t:
            return "absent"
        return f"{t.get('state')}: pending {t.get('pending')}, done {t.get('done')}, failed {t.get('failed')}, skipped {t.get('skipped')}"

    def ratio(r):
        d, i = r["asks"]["during_sync"]["ttft_p50_ms"], r["asks"]["idle"]["ttft_p50_ms"]
        return f"{d / i:.1f}×" if d and i else "–"

    out = [
        ("First probe found by `search`", *both(lambda r: s(r["t_first_search_s"]))),
        ("Half the items stored", *both(lambda r: s(r["t_half_indexed_s"]))),
        ("All items stored (t_indexed)", *both(lambda r: s(r["t_indexed_s"]))),
        ("Fully triaged (t_triaged)", *both(lambda r: s(r["t_triaged_s"], r["triage_timed_out"], r["triage_timeout_s"]))),
        ("Ask during sync: TTFT p50 / max", *both(lambda r: f"{ms(r['asks']['during_sync']['ttft_p50_ms'])} / {ms(r['asks']['during_sync']['ttft_max_ms'])}")),
        ("Ask idle: TTFT p50 / max", *both(lambda r: f"{ms(r['asks']['idle']['ttft_p50_ms'])} / {ms(r['asks']['idle']['ttft_max_ms'])}"
                                          + (" (triage timed out)" if r["asks"]["idle_after_timeout"] else ""))),
        ("TTFT during sync ÷ idle (p50)", *both(ratio)),
        ("Ask total p50 during / idle", *both(lambda r: f"{ms(r['asks']['during_sync']['total_p50_ms'])} / {ms(r['asks']['idle']['total_p50_ms'])}")),
        ("Asks answered during / idle", *both(lambda r: f"{r['asks']['during_sync']['answered']}/{r['asks']['during_sync']['n']} / "
                                              f"{r['asks']['idle']['answered']}/{r['asks']['idle']['n']}")),
        ("Asks citing a supporting item during / idle", *both(lambda r: f"{r['asks']['during_sync']['support_cited']} / {r['asks']['idle']['support_cited']}")),
        ("`search` during sync p50 / p95", *both(lambda r: f"{ms(r['search_during_sync']['p50_ms'])} / {ms(r['search_during_sync']['p95_ms'])}")),
        ("Probes found after settling", *both(lambda r: f"{r['recall_found']}/{len(r['recall'])}")),
        ("Items / chunks", *both(lambda r: f"{r['final']['items']} / {r['final']['chunks']}")),
        ("Items without chunks (lower bound)", *both(lambda r: str(r["final"]["zero_chunk_items_lower_bound"]))),
        ("Triage (final)", *both(tri)),
        ("Triage provider", *both(lambda r: str(r["final"].get("triage_provider")))),
        ("Freshness p50 / p95", *both(lambda r: f"{ms(r['final'].get('freshness_p50_ms'))} / {ms(r['final'].get('freshness_p95_ms'))}")),
    ]
    for q in dict.fromkeys([*b["first_search"], *a["first_search"]]):
        out.append((f"First found: {q}", *[s((r["first_search"].get(q) or {}).get("t")) for r in (b, a)]))
    for q in list(b["recall"]):
        out.append((f"Recall: {q}", *[("rank " + str(r["recall"][q]["rank"])) if r["recall"].get(q, {}).get("found") else "not found"
                                      for r in (b, a)]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--hardware", default="")
    a = ap.parse_args()
    b, af = load(a.before), load(a.after)
    print("## HH2: ingest before / after\n")
    print(f"Runner CPU: {cpu(a.hardware, [b, af])}. Chat model `{af.get('chat_model') or b.get('chat_model')}`, "
          f"{af['corpus_items']} items (corpus dated {af.get('corpus_today')}), polled every {af['poll_s']} s, "
          f"asks start {af['ask_delay_s']:.0f} s after the source is added. Times are from `sources.add`.\n")
    print(f"| | Before: {b.get('label')} | After: {af.get('label')} |")
    print("|---|---|---|")
    for name, x, y in rows(b, af):
        print(f"| {name} | {x} | {y} |")


if __name__ == "__main__":
    main()
