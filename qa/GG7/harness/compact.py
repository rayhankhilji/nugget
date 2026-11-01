#!/usr/bin/env python3
"""Keep a scored run small enough for the repo: score.md plus one short line per failed
row, the rest stays in the workflow artifact.

    compact.py RESULTS_DIR OUT_DIR
"""
import json
import re
import sys
from pathlib import Path

src, dst = Path(sys.argv[1]), Path(sys.argv[2])
dst.mkdir(parents=True, exist_ok=True)


def clip(t, n):
    t = re.sub(r"<think>.*?</think>", "", t or "", flags=re.S)
    t = re.sub(r"\s+", " ", t).strip()
    return t if len(t) <= n else t[:n] + "…"


def load(name):
    p = src / f"scored-{name}.jsonl"
    return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines()] if p.exists() else []


lines = []
for r in load("ask"):
    if not (r["facts_ok"] and r["cite_ok"] and r["lang_ok"]):
        lines.append({"id": r["id"], "a": clip(r.get("answer"), 120), "facts_ok": r["facts_ok"],
                      "cited": [c.rsplit("/", 1)[-1][:28] for c in r["cited"][:2]],
                      "answer_lang": r["answer_lang"]})
for r in load("draft"):
    if r["lang_ok"] and r["register_ok"] and not r["wrong_side"]:
        continue
    lines.append({"id": r["id"], "register": r.get("register"), "lang": r["draft_lang"],
                  "register_seen": r["register_seen"], "wrong_side": r["wrong_side"],
                  "draft": clip(r.get("draft"), 90)})
with open(dst / "rows.jsonl", "w", encoding="utf-8") as f:
    for x in lines:
        f.write(json.dumps(x, ensure_ascii=False) + "\n")
for name in ("score.md", "hardware.txt"):
    if (src / name).exists():
        (dst / name).write_text((src / name).read_text(encoding="utf-8"), encoding="utf-8")
