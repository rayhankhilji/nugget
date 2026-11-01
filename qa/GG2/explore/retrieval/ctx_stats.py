#!/usr/bin/env python3
"""Citation-mechanics stats over a retrieval-only answers.jsonl: [n] headers vs citations, duplicates, sizes."""
import json, re, sys, collections
hdr = re.compile(r"^\[(\d+)\] (.*) \((\S+)\)$", re.M)
items, chars, words, bad, dups, empty = [], [], [], [], 0, 0
for l in open(sys.argv[1]):
    r = json.loads(l)
    for i, t in enumerate(r["turns"]):
        cits, text = t["citations"], t["context"]
        if not cits:
            empty += 1; continue
        ns = [c["n"] for c in cits]
        hs = hdr.findall(text)
        hn = [int(h[0]) for h in hs]
        keys = [c["item"]["key"] for c in cits]
        if len(set(keys)) != len(keys): dups += 1
        urls_ok = all(h[2] == (c.get("url") or c["item"]["source"]) for h, c in zip(hs, cits))
        if ns != list(range(1, len(ns) + 1)) or hn != ns or not urls_ok:
            bad.append((r["id"], i + 1, ns, hn))
        items.append(len(cits)); chars.append(len(text)); words.append(len(text.split()))
q = lambda xs, p: sorted(xs)[int(p * (len(xs) - 1))]
print(f"turns with context: {len(items)}, empty: {empty}")
print(f"items per context: min {min(items)} p50 {q(items,.5)} max {max(items)}; ==10: {sum(x==10 for x in items)}")
print(f"chars: p50 {q(chars,.5)} max {max(chars)}; words p50 {q(words,.5)} max {max(words)}")
print(f"header/citation mismatches: {len(bad)} {bad[:5]}; contexts with a duplicate item: {dups}")
