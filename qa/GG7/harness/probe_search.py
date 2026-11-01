#!/usr/bin/env python3
"""Ask a running nuggetd (indexed with the GG7 corpus by gg7.py) a list of search queries.

    probe_search.py SOCKET [QUERY ...]   (default: the GG7-1/2/3 tokenisation probes)
"""
import sys

from gg7 import Ipc

PROBES = [
    "SK-2026-1027", "会社案内パンフレット", "パンフレット", "フロンティアビル5階", "フロンティアビル",
    "湯豆腐", "ＬＥＤ照明", "LED照明", "红烧肉", "糖色", "民宿",
    "فاتورة", "فاتوره", "والكراسي", "الكراسي", "كراسي",
    "Königstraße", "Konigstrasse", "Koenigstrasse", "Lenkungsausschuss", "Ausschuss",
]

ipc = Ipc(sys.argv[1])
for q in sys.argv[2:] or PROBES:
    hits = ipc.call("search", {"query": q, "limit": 5})
    print(f"{q!r}: {len(hits)} hits", [h["url"].rsplit("/", 1)[-1] for h in hits[:2]])
