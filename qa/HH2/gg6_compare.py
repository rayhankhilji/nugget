#!/usr/bin/env python3
"""GG6-1 before/after: compare two runs of QA GG6's qa/GG6/real-os/run-bridge.mjs.

    gg6_compare.py BEFORE_OUT AFTER_OUT [--hardware hardware.txt] [--labels B A] [--out compare.md]

Each *_OUT is a GG6_OUT directory (results.json, gg6.log). Writes a markdown table to stdout,
to --out, and appended to $GITHUB_STEP_SUMMARY when that is set. Stdlib only.

"Time to First source done" is GG6's `source_searchable` step: results.json `steps[]` entry
named "source_searchable", `took_ms` (= marks.source_searchable - marks.source_added): from the
folder dialog closing (the step starts showing "getting ready to index…" / "indexing…") to the
step's row saying "N items indexed. Search works now." (data-phase="ready").
"""
import argparse
import json
import os
import re
import statistics
import sys

SOURCE_CHECK = "onboarding: add the documents folder and wait until searchable"
# GG6's timed steps, in journey order, and what each one's took_ms means.
STEPS = [
    ("daemon_ready", "nuggetd start to ready"),
    ("welcome_visible", "Welcome visible"),
    ("install_clicked", "AI step until Install clicked"),
    ("model_installed", "model download + install"),
    ("ai_step_done", "AI step Continue"),
    ("source_added", "folder dialog (typed path, Add)"),
    ("source_searchable", "**First source: added to Search works now**"),
    ("onboarding_done", "Permissions, Double Tap, Done"),
    ("first_search_results", "first search results"),
]


def load(d):
    p = os.path.join(d, "results.json")
    if not os.path.isfile(p):
        return None
    with open(p, encoding="utf-8") as f:
        return json.load(f)


def secs(ms):
    return "–" if ms is None else f"{ms / 1000:.1f}"


def items_in(text):
    m = re.search(r"(\d+)\s+items?\b", text or "")
    return int(m.group(1)) if m else None


def source_step(r):
    """(seconds or None, items at ready or at give-up, ready text, timed-out?, phase trace seconds)."""
    step = next((s for s in r.get("steps", []) if s.get("name") == "source_searchable"), None)
    chk = next((c for c in r.get("checks", []) if c.get("name") == SOURCE_CHECK), None)
    ob = r.get("onboarding") or {}
    ready_text = ob.get("index_ready_text")
    timed_out = False
    last_seen = None
    if chk and not chk.get("ok"):
        err = chk.get("error") or ""
        timed_out = "timed out" in err
        m = re.search(r"last saw (.*)", err)
        last_seen = m.group(1)[:300] if m else err[:300]
    # The phases the step's row went through, as seconds after the first one shown.
    phases = ob.get("index_phases") or []
    trace = None
    if phases and isinstance(phases[0], dict) and "at_ms" in phases[0]:
        t0 = phases[0]["at_ms"]
        trace = [(round((p["at_ms"] - t0) / 1000, 1), p.get("phase"), p.get("text")) for p in phases]
    return {
        "took_ms": step.get("took_ms") if step else None,
        "items": items_in(ready_text) if ready_text else items_in(last_seen),
        "ready_text": ready_text,
        "timed_out": timed_out,
        "check": None if chk is None else ("pass" if chk.get("ok") else "FAIL"),
        "check_ms": chk.get("ms") if chk else None,
        "last_seen": last_seen,
        "trace": trace,
    }


def chat(r, label="chat_day1"):
    c = r.get(label)
    if not isinstance(c, dict) or not c.get("answers"):
        return None
    tot = [a.get("total_ms") for a in c["answers"] if a.get("total_ms") is not None]
    ftt = [a.get("first_token_ms") for a in c["answers"] if a.get("first_token_ms") is not None]
    return {
        "n": c.get("n"),
        "correct": c.get("correct"),
        "total_med": statistics.median(tot) if tot else None,
        "total_max": max(tot) if tot else None,
        "ftt_med": statistics.median(ftt) if ftt else None,
    }


def cpu(hardware, runs):
    lines = []
    if hardware and os.path.isfile(hardware):
        with open(hardware, encoding="utf-8", errors="replace") as f:
            for line in f:
                if re.match(r"\s*(Model name|CPU\(s\)|Mem):", line):
                    lines.append(" ".join(line.split()))
    if not lines:
        for r in runs:
            if r:
                lines.append(f"{r.get('cpu_model')} x{r.get('cpus')}, {r.get('ram_gb')} GB")
                break
    return "; ".join(lines) or "unknown"


def render(runs, labels, hardware):
    out = ["## GG6-1 before/after: onboarding's First source step (real model, Linux)", ""]
    out.append(f"CPU: {cpu(hardware, runs)}")
    out.append("")
    src = [source_step(r) if r else None for r in runs]
    head = "| | " + " | ".join(labels) + " |"
    out += [head, "|---|" + "---|" * len(labels)]

    def row(name, vals):
        out.append(f"| {name} | " + " | ".join(vals) + " |")

    def each(fn):
        return [fn(r, s) if r else "no results.json" for r, s in zip(runs, src)]

    row("nuggetd", each(lambda r, s: f"`{os.path.basename(r.get('nuggetd') or '?')}`"))
    row("model", each(lambda r, s: str((r.get("onboarding") or {}).get("model") or "–")))
    row(
        "**Time to First source done (s)**",
        each(
            lambda r, s: (f"**{secs(s['took_ms'])}**" if s["took_ms"] is not None else f"**timeout** (gave up after {secs(s['check_ms'])})" if s["timed_out"] else "–")
        ),
    )
    row("items indexed at that point", each(lambda r, s: "–" if s["items"] is None else str(s["items"]) + ("" if not s["timed_out"] else " (last seen)")))
    row("corpus files", each(lambda r, s: str(r.get("corpus_files", "–"))))
    row("source-step check", each(lambda r, s: f"{s['check'] or '–'}" + (f": {s['last_seen']}" if s["last_seen"] else "")))
    row("row text at the end", each(lambda r, s: s["ready_text"] or "–"))
    row(
        "first phase shown to ready (s)",
        each(lambda r, s: "–" if not s["trace"] or s["trace"][-1][1] != "ready" else secs(s["trace"][-1][0] * 1000)),
    )
    for name, what in STEPS:
        if name == "source_searchable":
            continue
        row(
            f"{what} (`{name}`, s)",
            each(lambda r, s, n=name: secs(next((x.get("took_ms") for x in r.get("steps", []) if x.get("name") == n), None))),
        )
    row("onboarding start to done (s)", each(lambda r, s: secs((r.get("marks") or {}).get("onboarding_done"))))
    for lab in ("chat_day1", "chat_day7"):
        if any(r and chat(r, lab) for r in runs):
            row(
                f"{lab}: correct, median / max total, median first token (s)",
                each(lambda r, s, lab=lab: (lambda c: "–" if not c else f"{c['correct']}/{c['n']}, {secs(c['total_med'])} / {secs(c['total_max'])}, {secs(c['ftt_med'])}")(chat(r, lab))),
            )
    row("checks passed", each(lambda r, s: f"{sum(1 for c in r.get('checks', []) if c.get('ok'))}/{len(r.get('checks', []))}"))
    row("fatal", each(lambda r, s: "yes" if r.get("fatal") else "no"))
    out.append("")

    for lab, r, s in zip(labels, runs, src):
        if not r:
            continue
        failed = [c["name"] for c in r.get("checks", []) if not c.get("ok")]
        if failed:
            out.append(f"**{lab}: failed checks:** " + "; ".join(failed))
            out.append("")
        if r.get("fatal"):
            out.append(f"**{lab}: fatal:** `{r['fatal'].splitlines()[0][:300]}`")
            out.append("")
        if s["trace"]:
            out.append(f"<details><summary>{lab}: the source step's row over time</summary>\n")
            out += [f"- +{t:.1f} s `{ph}`: {txt}" for t, ph, txt in s["trace"]]
            out.append("\n</details>\n")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--hardware", default="")
    ap.add_argument("--labels", nargs=2, default=["before", "after"])
    ap.add_argument("--out", default="")
    a = ap.parse_args()
    runs = [load(a.before), load(a.after)]
    md = render(runs, a.labels, a.hardware)
    sys.stdout.write(md)
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(md)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(md)


if __name__ == "__main__":
    main()
