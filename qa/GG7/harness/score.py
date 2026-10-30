#!/usr/bin/env python3
"""Score a GG7 run directory (the *.jsonl gg7.py wrote) against qa/GG7/gold.

    score.py RESULTS_DIR [--json OUT]

Prints a Markdown table per suite and per language, and writes scored rows to
RESULTS_DIR/scored-<suite>.jsonl so every verdict can be traced to its transcript.
"""

import json
import re
import sys
import unicodedata
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
GOLD = HERE.parent / "gold"

STOP = {
    "en": "the and you to of is for that with have this will be are your we on it can thanks please".split(),
    "de": "der die das und ich sie ihnen nicht mit ist für wir ein eine zu den gerne bitte vielen dank grüße auf auch".split(),
    "fr": "le la les et je vous de des est pour nous une un pas avec merci bien que dans cordialement bonjour sur".split(),
    "es": "el la los las y que de en por para con una un es no muy gracias saludos usted pero lo te hola".split(),
    "pt-BR": "o a os as e que de em para com uma um não muito obrigado obrigada você abraço olá do da no na".split(),
    "hi-Latn": "hai hain kya nahi main mai aap tum kar karo ho haan ji bhi toh mujhe kal accha theek yaar se ka ki ke".split(),
}
SCRIPTS = {
    "hi": (0x0900, 0x097F),
    "ar": (0x0600, 0x06FF),
}


def strip_think(t):
    return re.sub(r"<think>.*?</think>", "", t or "", flags=re.S).strip()


def norm(s):
    """Fold for fact matching: NFKC (full-width digits), Arabic-Indic digits, case, spaces."""
    s = unicodedata.normalize("NFKC", s or "")
    s = s.translate(str.maketrans("٠١٢٣٤٥٦٧٨٩۰۱۲۳۴۵۶۷۸۹", "01234567890123456789"))
    s = s.replace("٬", ",").replace("٫", ".").replace(" ", " ").replace(" ", " ")
    s = re.sub(r"[ً-ْـ]", "", s)  # tashkeel, tatweel
    return re.sub(r"\s+", " ", s).casefold()


def detect(text):
    """A coarse language id: script first, then stopword counts for Latin-script languages."""
    t = strip_think(text)
    letters = [c for c in t if c.isalpha()]
    if not letters:
        return "empty"
    n = len(letters)
    count = lambda lo, hi: sum(lo <= ord(c) <= hi for c in letters)  # noqa: E731
    kana = count(0x3040, 0x30FF)
    han = count(0x4E00, 0x9FFF)
    if kana / n > 0.05:
        return "ja"
    if han / n > 0.3:
        return "zh-CN"
    for lang, (lo, hi) in SCRIPTS.items():
        if count(lo, hi) / n > 0.3:
            return lang
    words = re.findall(r"[^\W\d_]+", t.casefold())
    scores = {k: sum(w in set(v) for w in words) for k, v in STOP.items()}
    best = max(scores, key=scores.get)
    return best if scores[best] else "unknown"


def lang_ok(detected, accept):
    acc = set(accept)
    if "pt" in acc:
        acc.add("pt-BR")
    if "zh" in acc:
        acc.add("zh-CN")
    return detected in acc


def rows(d, name):
    p = Path(d) / f"{name}.jsonl"
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text(encoding="utf-8").splitlines() if line.strip()]


def hit_file(h):
    s = " ".join(str(h.get(k) or "") for k in ("url", "title", "item"))
    s = unicodedata.normalize("NFC", s.replace("%2F", "/"))
    m = re.findall(r"([A-Za-z-]+/\d\d-[^/\s\"']+?\.(?:eml|md|ics))", s)
    return m[0] if m else s


def score_search(d):
    out, agg = [], defaultdict(lambda: defaultdict(lambda: [0, 0]))
    for r in rows(d, "search"):
        files = [hit_file(h) for h in r.get("hits", [])]
        at = lambda k: any(any(e in f for f in files[:k]) for e in r["expect"])  # noqa: E731
        r["hit@1"], r["hit@5"], r["hit@10"] = at(1), at(5), at(10)
        r["files"] = files
        out.append(r)
        for key in (r["lang"], "all"):
            for kind in (r.get("kind", "?"), "all"):
                a = agg[key][kind]
                a[0] += r["hit@5"]
                a[1] += 1
    return out, agg


def facts_ok(answer, facts):
    a = norm(answer)
    miss = [alts for alts in facts if not any(norm(x) in a for x in alts)]
    return not miss, miss


def score_ask(d):
    out = []
    for r in rows(d, "ask"):
        ans = strip_think(r.get("answer", ""))
        ok, miss = facts_ok(ans, r.get("facts", []))
        cites = [c.get("title") or "" for c in (r.get("started") or {}).get("citations", [])]
        cite_files = [hit_file(c) for c in (r.get("started") or {}).get("citations", [])]
        want_lang = r.get("question_lang") or r["lang"]
        det = detect(ans)
        r.update(
            facts_ok=ok,
            facts_missing=miss,
            cite_ok=any(any(e in f for f in cite_files) for e in r.get("cite", [])),
            cited=cite_files or cites,
            answer_lang=det,
            lang_ok=(want_lang == "mixed") or lang_ok(det, [want_lang]),
            error=r.get("error") or (r.get("done") or {}).get("error"),
        )
        out.append(r)
    return out


def register(text, d):
    t = " " + norm(strip_think(text)) + " "
    f = sum(t.count(norm(m)) for m in d.get("formal_markers", []))
    i = sum(t.count(norm(m)) for m in d.get("informal_markers", []))
    if f == i == 0:
        return "neutral"
    return "formal" if f > i else "informal" if i > f else "mixed"


def score_draft(d):
    out = []
    for r in rows(d, "draft"):
        txt = strip_think(r.get("draft", ""))
        det = detect(txt)
        reg = register(txt, r)
        want = r.get("register")
        r.update(
            draft_lang=det,
            lang_ok=lang_ok(det, r.get("accept_lang") or [r["lang"]]),
            register_seen=reg,
            register_ok=(want in (None, "", "any")) or reg == want or (reg == "neutral" and want != "informal"),
            nonempty=bool(txt.strip()),
            error=r.get("error") or (r.get("done") or {}).get("error"),
        )
        out.append(r)
    return out


def score_knowledge(d):
    ks = rows(d, "knowledge")
    if not ks:
        return [], {}
    k = ks[-1]
    blob_by_name = {}
    for e in k.get("details", []):
        names = [e.get("name", "")] + list(e.get("aliases") or [])
        blob_by_name[e.get("id")] = (names, json.dumps(e, ensure_ascii=False))
    all_blob = norm(" ".join(b for _, b in blob_by_name.values()))
    out = []
    for lang in sorted(p.stem for p in GOLD.glob("*.json")):
        g = json.loads((GOLD / f"{lang}.json").read_text(encoding="utf-8")).get("extract", {})
        for p in g.get("people", []):
            cands = [p["name"]] + p.get("aliases", [])
            found = [
                i for i, (names, _) in blob_by_name.items()
                if any(norm(c) == norm(n) or norm(c).replace(" ", "") == norm(n).replace(" ", "") for c in cands for n in names)
            ]
            out.append({"lang": lang, "type": "person", "gold": p["name"], "ok": bool(found), "entities": len(found)})
        for o in g.get("orgs", []):
            ok = norm(o["name"]) in all_blob
            out.append({"lang": lang, "type": "org", "gold": o["name"], "ok": ok})
        for dt in g.get("dates", []):
            ok = dt["date"] in all_blob
            out.append({"lang": lang, "type": "date", "gold": dt["date"], "text": dt.get("text"), "ok": ok})
    return out, k.get("status", {})


def score_proactive(d):
    ps = rows(d, "proactive")
    if not ps:
        return []
    p = ps[-1]
    cards = (p.get("cards.list") or {}).get("cards", []) if isinstance(p.get("cards.list"), dict) else []
    blob = [json.dumps(c, ensure_ascii=False) for c in cards]
    out = []
    for lang in sorted(x.stem for x in GOLD.glob("*.json")):
        g = json.loads((GOLD / f"{lang}.json").read_text(encoding="utf-8"))
        for e in g.get("proactive", []):
            stem = Path(e["file"]).name
            hits = [c for c, b in zip(cards, blob) if stem in unicodedata.normalize("NFC", b)]
            due_ok = any(e.get("due", "\0") in json.dumps(c) for c in hits) if e.get("due") else None
            out.append({"lang": lang, **e, "surfaced": bool(hits), "due_ok": due_ok,
                        "card_kinds": [c.get("kind") for c in hits], "titles": [c.get("title") for c in hits]})
    return out


def pct(a, b):
    return f"{a}/{b} ({100 * a // b}%)" if b else "-"


def main():
    d = sys.argv[1]
    print(f"# GG7 scores: {d}\n")
    s, agg = score_search(d)
    if s:
        kinds = sorted({k for v in agg.values() for k in v} - {"all"})
        print("## Search hit@5\n")
        print("| lang | all | " + " | ".join(kinds) + " |")
        print("|---" * (len(kinds) + 2) + "|")
        for lang in sorted(agg, key=lambda x: (x == "all", x)):
            cells = [pct(*agg[lang][k]) if agg[lang][k][1] else "" for k in kinds]
            print(f"| {lang} | {pct(*agg[lang]['all'])} | " + " | ".join(cells) + " |")
        print()
    for name, rs in (("ask", score_ask(d)), ("draft", score_draft(d))):
        if not rs:
            continue
        keys = ["facts_ok", "cite_ok", "lang_ok"] if name == "ask" else ["nonempty", "lang_ok", "register_ok"]
        by = defaultdict(list)
        for r in rs:
            by[r["lang"]].append(r)
            by["all"].append(r)
        print(f"## {name}\n\n| lang | n | " + " | ".join(keys) + " | errors |\n" + "|---" * (len(keys) + 3) + "|")
        for lang in sorted(by, key=lambda x: (x == "all", x)):
            v = by[lang]
            print(f"| {lang} | {len(v)} | " + " | ".join(pct(sum(bool(r[k]) for r in v), len(v)) for k in keys)
                  + f" | {sum(bool(r.get('error')) for r in v)} |")
        print()
        with open(Path(d) / f"scored-{name}.jsonl", "w", encoding="utf-8") as f:
            for r in rs:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    k, st = score_knowledge(d)
    if k:
        by = defaultdict(lambda: defaultdict(lambda: [0, 0]))
        for r in k:
            by[r["lang"]][r["type"]][0] += r["ok"]
            by[r["lang"]][r["type"]][1] += 1
        print(f"## Knowledge (status {json.dumps(st, ensure_ascii=False)})\n\n| lang | people | orgs | dates |\n|---|---|---|---|")
        for lang in sorted(by):
            print(f"| {lang} | " + " | ".join(pct(*by[lang][t]) for t in ("person", "org", "date")) + " |")
        print()
        with open(Path(d) / "scored-knowledge.jsonl", "w", encoding="utf-8") as f:
            for r in k:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    p = score_proactive(d)
    if p:
        by = defaultdict(lambda: [0, 0])
        for r in p:
            by[r["lang"]][0] += r["surfaced"]
            by[r["lang"]][1] += 1
        print("## Proactive (gold situation surfaced as a card)\n\n| lang | surfaced |\n|---|---|")
        for lang in sorted(by):
            print(f"| {lang} | {pct(*by[lang])} |")
        with open(Path(d) / "scored-proactive.jsonl", "w", encoding="utf-8") as f:
            for r in p:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
