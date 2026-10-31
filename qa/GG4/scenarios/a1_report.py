#!/usr/bin/env python3
"""A1: summarise a replay output dir (day-NN.json) into a per-morning view.

For each morning: open cards (kind, title, rank, sensitive), cards that changed state since the
previous morning (cards_all: resolved/expired/done/dismissed), sensitive words leaking into card
titles/details/brief, noise that got through, duplicates by evidence item / Message-ID, and the
truth check (hits/misses/forbidden/out-of-window).

    python3 a1_report.py OUT_DIR [--md] [--json FILE]
"""
import json
import re
import sys
from pathlib import Path

SENSITIVE_WORDS = ["metformin", "hba1c", "lab result", "test result", "therapy", "session",
                   "salary", "compensation", "118,500", "fraud", "4417", "verification code",
                   "482-913", "clinic", "nurse", "results"]
NOISE = ["ux weekly", "boots", "build failed", "automatic reply", "accepted:", "check in now",
         "cloudvault"]


def card_text(c):
    return " ".join([c.get("title", ""), c.get("detail", ""), c.get("summary") or ""]).lower()


def main():
    out = Path(sys.argv[1])
    md = "--md" in sys.argv
    days = sorted(out.glob("day-*.json"))
    prev_states = {}
    rows = []
    for f in days:
        d = json.loads(f.read_text())
        cards = d["cards"]["cards"]
        allc = {c["id"]: c for c in d["cards_all"]}
        trans = []
        for cid, c in allc.items():
            p = prev_states.get(cid)
            if p is None:
                trans.append(("+" + c["state"], c["kind"], c["title"]))
            elif p != c["state"]:
                trans.append((f"{p}->{c['state']}", c["kind"], c["title"]))
        prev_states = {cid: c["state"] for cid, c in allc.items()}
        leaks = []
        for c in cards:
            t = card_text(c)
            for w in SENSITIVE_WORDS:
                if w in t:
                    leaks.append((c["kind"], c["title"], w, c["sensitive"]))
        brief = d.get("brief") or {}
        btxt = json.dumps(brief.get("top", [])).lower()
        bleaks = [w for w in SENSITIVE_WORDS if w in btxt]
        noise = [(c["kind"], c["title"]) for c in cards if any(n in card_text(c) for n in NOISE)]
        # duplicates by Message-ID-ish evidence (file basename) across open cards
        ev = {}
        dups = []
        for c in cards:
            for e in c.get("evidence", [])[:1]:
                base = Path(e["item"]["key"]).name
                if base in ev and ev[base] != c["id"]:
                    dups.append((base, ev[base], c["id"]))
                ev[base] = c["id"]
        chk = d["check"]
        rows.append({
            "day": d["day"], "date": d["date"], "tz": d["tz"],
            "open": [(c["kind"], c["title"], round(c["rank"], 2), c["sensitive"], c["state"]) for c in cards],
            "below_threshold_or_hidden": [(c["kind"], c["title"], round(c["rank"], 2), c["state"])
                                          for c in d["cards_all"] if c["state"] in ("new", "seen")
                                          and c["id"] not in {x["id"] for x in cards}],
            "transitions": trans, "leaks": leaks, "brief_leaks": bleaks, "noise": noise,
            "evidence_dups": dups,
            "hits": [(h["scenario"], h["expect"]["match"]) for h in chk["hits"]],
            "misses": [(h["scenario"], h["expect"]["match"], h["expect"].get("optional", False)) for h in chk["misses"]],
            "forbidden": [(x["scenario"], x["title"], x["why"]) for x in chk["forbidden"]],
            "out_of_window": [(x["scenario"], x["expect"]["match"], x["cards"]) for x in chk["out_of_window"]],
            "dups": chk["duplicates"],
            "user": d.get("user"),
            "refresh": d.get("refresh"),
            "brief_top": [(t["kind"], t["title"]) for t in brief.get("top", [])],
            "brief_renewals": brief.get("renewals"), "brief_overdue": brief.get("overdue"),
        })
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(rows, indent=1))
    for r in rows:
        print(f"\n## day {r['day']} {r['date']} {r['tz']}  refresh={r['refresh']}")
        for o in r["open"]:
            print(f"  OPEN  {o[0]:18s} r={o[2]:.2f} sens={o[3]!s:5s} {o[1]}")
        for o in r["below_threshold_or_hidden"]:
            print(f"  HIDDEN {o[0]:17s} r={o[2]:.2f} {o[1]}")
        for t in r["transitions"]:
            print(f"  TRANS {t[0]:20s} {t[1]:18s} {t[2]}")
        for k in ("hits", "misses", "forbidden", "out_of_window", "dups", "leaks", "brief_leaks",
                  "noise", "evidence_dups"):
            if r[k]:
                print(f"  {k.upper()}: {r[k]}")
        if r["user"]:
            for u in r["user"]:
                print(f"  USER {u['action']:8s} {u['reason'] or '':16s} {u['title']} ({u['because']}) {u['error'] or ''}")


if __name__ == "__main__":
    main()
