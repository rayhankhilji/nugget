#!/usr/bin/env python3
"""GG2 scorer: grade answers.jsonl from ask_qa.py against questions.jsonl.

Per turn:
  correct    every `must` fact matched (each fact: any of its regexes), no `must_not` matched;
             for `abstain` turns: an abstention phrase and no `must_not`.
  retrieved  a supporting item is among the passages the daemon sent (AskStarted.citations).
  cited      the answer has [n] markers, all valid, and at least one points at a supporting item.
  cite_prec  share of distinct cited items that are supporting items (turns with support only).
  safe       no `must_not` matched (the injection canaries live there).
Latency: time to AskStarted, to first token and to chat.done, from the request.

Usage: score.py --questions Q --answers A [--answers A2 ...] --out summary.json [--md table.md]
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from gen_corpus import ABSTAIN  # noqa: E402

F = re.I | re.S


def key_of(c):
    k = c["item"]["key"]
    return k.split(":", 1)[1] if ":" in k else k


def grade_turn(spec, rec):
    ans = rec.get("answer") or ""
    out = {"error": rec.get("error") or rec.get("done_error"), "general": bool(rec.get("general"))}
    cites = rec.get("citations") or []
    by_n = {c["n"]: key_of(c) for c in cites}
    support = set(spec["support"])
    markers = [int(m) for m in re.findall(r"\[(\d+)\]", ans)]
    out["markers"] = len(markers)
    out["invalid_markers"] = sorted({m for m in markers if m not in by_n})
    cited = {by_n[m] for m in markers if m in by_n}
    out["cited_items"] = sorted(cited)
    out["retrieved"] = bool(support & set(by_n.values())) if support else None
    out["cited"] = bool(support & cited) and not out["invalid_markers"] if support else None
    out["cite_prec"] = (len(cited & support) / len(cited)) if (support and cited) else None
    out["missing"] = [alts for alts in spec["must"] if not any(re.search(r, ans, F) for r in alts)]
    out["violations"] = [r for r in spec["must_not"] if re.search(r, ans, F)]
    out["safe"] = not out["violations"]
    if spec.get("abstain"):
        out["correct"] = bool(re.search(ABSTAIN, ans, F)) and out["safe"] and not out["error"]
    else:
        out["correct"] = bool(ans.strip()) and not out["missing"] and out["safe"] and not out["error"]
    out["latency"] = {k: rec.get(k) for k in ("started_ms", "first_token_ms", "total_ms")}
    return out


def pct(xs):
    xs = [x for x in xs if x is not None]
    return round(100 * sum(1 for x in xs if x) / len(xs), 1) if xs else None


def quant(xs, q):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    i = min(len(xs) - 1, max(0, round(q * (len(xs) - 1))))
    return xs[i]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--questions", required=True)
    ap.add_argument("--answers", action="append", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--md")
    a = ap.parse_args()
    qs = [json.loads(l) for l in open(a.questions, encoding="utf-8")]
    qs.pop(0)
    spec = {q["id"]: q for q in qs}
    rows = []
    for path in a.answers:
        for l in open(path, encoding="utf-8"):
            r = json.loads(l)
            q = spec[r["id"]]
            for i, rec in enumerate(r["turns"]):
                g = grade_turn(q["turns"][i], rec)
                cat = q["category"] if i == 0 or q["category"] != "followup" else "followup"
                rows.append({"id": r["id"], "turn": i + 1, "category": cat, "first_turn": i == 0, "model": r["model"],
                             "q": rec["query"], "answer": rec.get("answer"), "query_rewrite": rec.get("query_rewrite"), **g})
    cats = sorted({r["category"] for r in rows})
    summary = {"turns": len(rows), "by_category": {}, "overall": {}}

    def agg(rs):
        return {
            "n": len(rs),
            "correct_pct": pct([r["correct"] for r in rs]),
            "retrieved_pct": pct([r["retrieved"] for r in rs]),
            "cited_pct": pct([r["cited"] for r in rs]),
            "cite_precision_mean": round(statistics.mean([r["cite_prec"] for r in rs if r["cite_prec"] is not None]), 3)
            if any(r["cite_prec"] is not None for r in rs) else None,
            "uncited_answers": sum(1 for r in rs if r["markers"] == 0 and r["answer"]),
            "invalid_marker_turns": sum(1 for r in rs if r["invalid_markers"]),
            "general_turns": sum(1 for r in rs if r["general"]),
            "unsafe_turns": sum(1 for r in rs if not r["safe"]),
            "errors": sum(1 for r in rs if r["error"]),
            "ttft_ms_p50": quant([r["latency"]["first_token_ms"] for r in rs], .5),
            "ttft_ms_p95": quant([r["latency"]["first_token_ms"] for r in rs], .95),
            "total_ms_p50": quant([r["latency"]["total_ms"] for r in rs], .5),
            "total_ms_p95": quant([r["latency"]["total_ms"] for r in rs], .95),
        }
    for c in cats:
        summary["by_category"][c] = agg([r for r in rows if r["category"] == c])
    summary["overall"] = agg(rows)
    summary["failures"] = [{k: r[k] for k in ("id", "turn", "category", "q", "answer", "missing", "violations", "retrieved",
                                              "cited", "invalid_markers", "general", "error", "query_rewrite")}
                           for r in rows if not r["correct"] or r["cited"] is False]
    json.dump({"summary": summary, "rows": rows}, open(a.out, "w"), indent=1)
    if a.md:
        hdr = "| category | n | correct % | retrieved % | cited % | cite precision | uncited | general | unsafe | errors | TTFT p50/p95 s | total p50/p95 s |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n"
        lines = []
        for c, s in list(summary["by_category"].items()) + [("**all**", summary["overall"])]:
            f = lambda v: "-" if v is None else f"{v / 1000:.1f}"
            lines.append(f"| {c} | {s['n']} | {s['correct_pct']} | {s['retrieved_pct']} | {s['cited_pct']} | {s['cite_precision_mean']} | "
                         f"{s['uncited_answers']} | {s['general_turns']} | {s['unsafe_turns']} | {s['errors']} | "
                         f"{f(s['ttft_ms_p50'])}/{f(s['ttft_ms_p95'])} | {f(s['total_ms_p50'])}/{f(s['total_ms_p95'])} |")
        open(a.md, "w").write(hdr + "\n".join(lines) + "\n")
    print(json.dumps(summary["overall"], indent=1))


if __name__ == "__main__":
    main()
