#!/usr/bin/env python3
"""JJ1 real-model scenarios, run on a GitHub runner by .github/workflows/qa-JJ1.yml.

  realmodel.py --nuggetd BIN --home DIR --out DIR --part refine|triage|messaging
               [--chat-model qwen3-4b-q4km] [--embed-model nomic-embed-text-v1.5-q8] [--models-cache DIR]

refine     Double Tap refinements as the overlay sends them (apps/desktop/src-tauri/src/doubletap/
           flow.rs `refined_request`: the original context, the draft as the field's selected text,
           the refinement's instruction). For each context (English and German): a base draft, then
           Shorter, Longer, More formal, More casual. Checks: Shorter has fewer words than the base,
           Longer more; formal/casual move a style score the right way (contractions, slang,
           greetings; German du/Sie); the answer stays in the context's language.
triage     HH2: the triage corpus (qa/JJ1/triage/gen_corpus.py) as a files source with the 4B as the
           triage model. Times first search, asks 4 questions during the sync and again after triage,
           waits for triage, then exports items (export.run) and scores each planted item's
           sensitive / junk labels, and checks junk is still searchable.
messaging  II1: the messaging fixture (qa/JJ1/messaging/realmodel_fixture.py) as whatsapp/telegram
           (/imessage with db_path) sources, private by default; asks its questions through
           retrieve.ask and scores answers and citations. Also asks with a fake "cloud" provider
           routed first to check that the chats are excluded (excluded_sensitive) rather than sent.

Writes OUT/<part>.json and OUT/<part>.md.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from nq import Daemon, RpcError, dump  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
QA = os.path.dirname(HERE)


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


# ---------------------------------------------------------------------------------------------
# setup

def prepare_home(a):
    """Default config with the chosen models, idle stop off, knowledge off; models from the cache."""
    d = Daemon(a.home, nuggetd=a.nuggetd)
    if not os.path.exists(d.config_path()):
        d.start()
        d.stop()
    s = open(d.config_path(), encoding="utf-8").read()
    s = s.replace('# chat_model = "qwen3-4b-q4km"', f'chat_model = "{a.chat_model}"', 1)
    s = s.replace('# embedding_model = "nomic-embed-text-v1.5-q8"', f'embedding_model = "{a.embed_model}"', 1)
    s = re.sub(r"idle_stop_minutes = \d+", "idle_stop_minutes = 0", s, count=1)
    s = s.replace("[knowledge]\nenabled = true", "[knowledge]\nenabled = false", 1)
    open(d.config_path(), "w", encoding="utf-8").write(s)
    shutil.copy(d.config_path(), os.path.join(a.out, f"{a.part}-config.toml"))
    if a.models_cache and os.path.isdir(a.models_cache):
        md = os.path.join(a.home, "data", "models")
        os.makedirs(md, exist_ok=True)
        for n in os.listdir(a.models_cache):
            dst = os.path.join(md, n)
            if not os.path.exists(dst):
                shutil.move(os.path.join(a.models_cache, n), dst)
    return d


def restore_cache(a):
    if not a.models_cache:
        return
    md = os.path.join(a.home, "data", "models")
    os.makedirs(a.models_cache, exist_ok=True)
    if os.path.isdir(md):
        for n in os.listdir(md):
            dst = os.path.join(a.models_cache, n)
            if not os.path.exists(dst):
                shutil.move(os.path.join(md, n), dst)


def install(d, model_id, timeout=5400):
    models = d.call("models.list", timeout=60)
    m = next((m for m in models if m["id"] == model_id), None)
    if m is None:
        raise SystemExit(f"{model_id} not in models.list")
    if m["installed"]:
        log(model_id, "already installed")
        return
    d.call("models.install", {"id": model_id}, timeout=60)
    t = time.time()
    while time.time() - t < timeout:
        _, method, p = d.c.notes.get(timeout=timeout)
        if method == "models.progress" and p.get("id") == model_id:
            if p["phase"] == "installed":
                log(model_id, f"installed in {time.time() - t:.0f}s")
                return
            if p["phase"] == "failed":
                raise SystemExit(f"{model_id}: {p.get('error')}")
    raise SystemExit(f"{model_id}: install timed out")


def wait_runtime(d, timeout=600):
    t = time.time()
    while time.time() - t < timeout:
        rt = d.call("runtime.status")
        if rt and all(r["state"] == "ready" for r in rt if r["role"] == "chat"):
            return rt
        time.sleep(2)
    return d.call("runtime.status")


def ask(d, q, timeout=900):
    try:
        return d.ask(q, timeout=timeout)
    except (RpcError, TimeoutError) as e:
        return {"query": q, "error": str(e)}


def draft(d, req, timeout=600):
    t0 = time.monotonic()
    try:
        st = d.call("draft.begin", req, timeout=timeout)
    except (RpcError, TimeoutError) as e:
        return {"error": str(e)}
    text, done, first = d.c.stream(st["stream_id"], timeout)
    return {"text": text, "provider": st.get("provider"), "locality": st.get("locality"),
            "field_kind": st.get("field_kind"), "total_ms": round((time.monotonic() - t0) * 1000),
            "first_ms": round((first - t0) * 1000) if first else None, "done_error": done.get("error")}


# ---------------------------------------------------------------------------------------------
# refine

REFINE = {  # apps/desktop/src-tauri/src/doubletap/overlay.rs Refinement::instruction (verbatim)
    "shorter": "Rewrite the selected draft shorter: keep what matters, drop the rest.",
    "longer": "Rewrite the selected draft longer, with a little more detail, without inventing facts.",
    "formal": "Rewrite the selected draft in a more formal tone.",
    "casual": "Rewrite the selected draft in a more casual, relaxed tone.",
}

CONTEXTS = [
    {"id": "en-mail-deadline", "lang": "en", "app": "Thunderbird", "role": "text_area", "subject": "Report deadline",
     "thread": [("Priya Nandakumar", "Hi, could you send me the Q3 vendor report by Thursday? Finance needs it for the board pack. Thanks!")]},
    {"id": "en-chat-dinner", "lang": "en", "app": "Slack", "role": "text_field", "subject": None,
     "thread": [("Tomás", "hey are we still on for dinner friday? I booked the thai place for 7:30")]},
    {"id": "en-mail-decline", "lang": "en", "app": "Thunderbird", "role": "text_area", "subject": "Speaking invitation",
     "thread": [("Conference Team", "We would be delighted to have you speak at the Northern Data Summit on 12 March in Leeds. Could you confirm by the end of the month?")],
     "instruction": "decline politely, I'm travelling that week"},
    {"id": "de-mail-termin", "lang": "de", "app": "Thunderbird", "role": "text_area", "subject": "Termin nächste Woche",
     "thread": [("Katrin Albers", "Hallo, hätten Sie nächste Woche Zeit für ein kurzes Gespräch über das Angebot? Dienstag oder Mittwoch Vormittag würde mir passen. Viele Grüße, Katrin Albers")]},
    {"id": "de-chat-umzug", "lang": "de", "app": "Signal", "role": "text_field", "subject": None,
     "thread": [("Jonas", "Hey, kannst du mir am Samstag beim Umzug helfen? Wir fangen so um 10 an, Pizza gibt's danach 🍕")]},
    {"id": "de-mail-rechnung", "lang": "de", "app": "Thunderbird", "role": "text_area", "subject": "Rechnung 2026-114",
     "thread": [("Buchhaltung Brenner GmbH", "Sehr geehrte Damen und Herren, die Rechnung 2026-114 ist seit dem 15. Oktober fällig. Bitte prüfen Sie den Zahlungseingang.")],
     "instruction": "sag, dass wir die Rechnung diese Woche überweisen"},
]

EN_CASUAL = re.compile(r"\b(hey|hi|yeah|yep|gonna|wanna|cool|awesome|cheers|thx|thanks!|no worries|sounds good|totally|super)\b|!|\b\w+'(s|re|ll|m|ve|d|t)\b", re.I)
EN_FORMAL = re.compile(r"\b(dear|kind regards|best regards|sincerely|please|would|could|appreciate|regarding|confirm|thank you|i am|i will|we will|do not|cannot)\b", re.I)
DE_SIE = re.compile(r"\b(Sie|Ihnen|Ihr|Ihre|Ihren|Ihrem|Ihrer)\b")
DE_DU = re.compile(r"\b(du|dir|dich|dein|deine|deinen|deinem|euch|ihr seid|hey|hallo|lg|klar|super|cool)\b", re.I)
DE_FORMAL = re.compile(r"\b(Sehr geehrte|Mit freundlichen Grüßen|Freundliche Grüße|Beste Grüße|bitte|gern|würde|könnten)\b")
DE_WORDS = re.compile(r"\b(und|der|die|das|ich|wir|nicht|mit|für|ist|zu|gerne|gern|Grüße|Uhr|bitte)\b", re.I)
EN_WORDS = re.compile(r"\b(the|and|to|you|I|we|is|for|with|will|be|on|thanks)\b")


def style(text, lang):
    if lang == "en":
        return len(EN_FORMAL.findall(text)) - len(EN_CASUAL.findall(text))
    return 2 * len(DE_SIE.findall(text)) + len(DE_FORMAL.findall(text)) - 2 * len(DE_DU.findall(text))


def lang_of(text):
    de, en = len(DE_WORDS.findall(text)), len(EN_WORDS.findall(text))
    return "de" if de > en else "en" if en > de else "?"


def words(text):
    return len(re.findall(r"\w+", text))


def ctx_request(c, value="", selection=None, instruction=None):
    return {"context": {"app": {"name": c["app"], "id": c["app"].lower()},
                        "field": {"role": c["role"], "value": value, "selection": list(selection) if selection else None, "secure": False},
                        "thread": [{"author": a, "text": t, "from_me": False} for a, t in c["thread"]],
                        "recipients": [c["thread"][0][0]], "subject": c["subject"]},
            "instruction": instruction, "provider": None, "max_tokens": None}


def refined(c, base, kind):
    # flow.rs refined_request with an empty field and no selection: value = base, selection = (0, len(base))
    return ctx_request(c, value=base, selection=(0, len(base)), instruction=REFINE[kind])


def part_refine(a, d):
    rows = []
    for rep in range(a.reps):
        for c in CONTEXTS:
            base = draft(d, ctx_request(c, instruction=c.get("instruction")))
            if base.get("error") or not base.get("text", "").strip():
                rows.append({"ctx": c["id"], "rep": rep, "kind": "base", "error": base.get("error") or "empty draft", **base})
                continue
            b = base["text"].strip()
            rows.append({"ctx": c["id"], "rep": rep, "kind": "base", "text": b, "words": words(b), "style": style(b, c["lang"]),
                         "lang_ok": lang_of(b) == c["lang"], "total_ms": base["total_ms"], "locality": base["locality"]})
            for kind in REFINE:
                r = draft(d, refined(c, b, kind))
                t = (r.get("text") or "").strip()
                row = {"ctx": c["id"], "rep": rep, "kind": kind, "text": t, "words": words(t), "style": style(t, c["lang"]),
                       "lang_ok": lang_of(t) == c["lang"], "total_ms": r.get("total_ms"), "error": r.get("error") or r.get("done_error")}
                bw, bs = words(b), style(b, c["lang"])
                row["base_words"], row["base_style"] = bw, bs
                row["pass"] = bool(t) and row["lang_ok"] and {
                    "shorter": row["words"] < bw,
                    "longer": row["words"] > bw,
                    "formal": row["style"] > bs or (row["style"] == bs and bs >= 3),
                    "casual": row["style"] < bs,
                }[kind]
                # A rewrite must not answer the thread from scratch or echo the instruction.
                row["echo"] = "Rewrite the selected draft" in t
                rows.append(row)
                log(c["id"], kind, "pass" if row["pass"] else "FAIL", f"{bw}->{row['words']} words, style {bs}->{row['style']}")
    out = {"rows": rows}
    summary = {}
    for kind in REFINE:
        for lang in ("en", "de"):
            rs = [r for r in rows if r["kind"] == kind and r["ctx"].startswith(lang)]
            summary[f"{kind}-{lang}"] = f"{sum(1 for r in rs if r.get('pass'))}/{len(rs)}"
    out["summary"] = summary
    return out


# ---------------------------------------------------------------------------------------------
# triage + ask during sync

TRIAGE_QS = [
    {"q": "When will the bike wheel be ready?", "gold": r"wednesday"},
    {"q": "Where is the new noodle place that opened?", "gold": r"garnet"},
    {"q": "What time does the Saturday barbecue start?", "gold": r"17|5\s?pm"},
    {"q": "What are we reading next month in the book club?", "gold": r"lighthouse"},
]


def part_triage(a, d):
    corpus = os.path.join(a.work, "triage-corpus")
    if not os.path.isdir(corpus):
        subprocess.run([sys.executable, os.path.join(QA, "triage", "gen_corpus.py"), "--out", corpus, "--n", str(a.triage_n)], check=True)
    truth = [json.loads(l) for l in open(os.path.join(corpus, "truth.jsonl"), encoding="utf-8")]
    t0 = time.time()
    d.call("sources.add", {"kind": "files", "name": "Mail", "settings": {"roots": [os.path.join(corpus, "mail")]}})
    probes = [r for r in truth if r.get("probe") and r["class"] != "normal"][:6]
    first = {}
    timeline, during = [], []
    asked = False
    deadline = t0 + a.triage_timeout
    while time.time() < deadline:
        st = d.call("ingest.status")
        src = st["sources"][0]
        tr = st.get("triage") or {}
        timeline.append({"s": round(time.time() - t0, 1), "items": src["items"], "queue": st["queue_depth"], "triage": tr})
        for r in probes:
            if r["probe"] not in first and d.call("search", {"query": r["probe"], "limit": 5}):
                first[r["probe"]] = round(time.time() - t0, 1)
        if not asked and time.time() - t0 > 20:
            asked = True
            for q in TRIAGE_QS:
                tq = time.time() - t0
                res = ask(d, q["q"], timeout=600)
                res.update(asked_at_s=round(tq, 1), triage_at_start=(d.call("ingest.status").get("triage") or {}),
                           correct=bool(re.search(q["gold"], res.get("answer", ""), re.I)))
                during.append(res)
                log("during sync:", q["q"], res.get("first_token_ms"), res.get("total_ms"), res.get("correct"))
        if src["items"] >= len(truth) and tr.get("pending") == 0 and tr.get("state") != "running" and asked:
            break
        time.sleep(5)
    t_triaged = round(time.time() - t0, 1)
    idle = []
    for q in TRIAGE_QS:
        res = ask(d, q["q"], timeout=600)
        res["correct"] = bool(re.search(q["gold"], res.get("answer", ""), re.I))
        idle.append(res)
        log("idle:", q["q"], res.get("first_token_ms"), res.get("total_ms"), res.get("correct"))
    # Labels as stored: export everything and read items.jsonl metadata.
    exp = os.path.join(a.work, "export")
    shutil.rmtree(exp, ignore_errors=True)
    os.makedirs(exp)
    labels = {}
    try:
        d.call("export.run", {"folder": exp}, timeout=1800)
        for root, _, files in os.walk(exp):
            if "items.jsonl" in files:
                for line in open(os.path.join(root, "items.jsonl"), encoding="utf-8"):
                    it = json.loads(line)
                    m = it.get("metadata") or {}
                    labels[it["key"].split("/")[-1]] = {"sensitive": m.get("sensitive"), "rule": m.get("sensitive_rule"),
                                                        "junk": m.get("junk"), "triage": m.get("triage")}
    except (RpcError, TimeoutError) as e:
        log("export failed", e)
    rows = []
    for r in truth:
        if r["class"] == "normal" and not r.get("probe"):
            continue
        name = r["path"].split("/")[-1]
        lab = labels.get(name, {})
        found = None
        if r.get("probe"):
            hits = d.call("search", {"query": r["probe"], "limit": 10})
            found = any(name in json.dumps(h) for h in hits)
        rows.append({**r, **{k: lab.get(k) for k in ("sensitive", "rule", "junk")},
                     "triage_state": (lab.get("triage") or {}).get("state"), "model_sensitive": (lab.get("triage") or {}).get("sensitive"),
                     "model_junk": (lab.get("triage") or {}).get("junk"), "reason": (lab.get("triage") or {}).get("reason"), "searchable": found})
    normal_all = [v for k, v in labels.items() if k.startswith("n")]
    out = {"items": len(truth), "first_search_s": first, "t_triaged_s": t_triaged, "timeline": timeline[-40:] + timeline[:10],
           "ask_during": during, "ask_idle": idle, "rows": rows,
           "normal_sensitive_rate": round(sum(1 for v in normal_all if v.get("sensitive")) / max(1, len(normal_all)), 3),
           "normal_junk_rate": round(sum(1 for v in normal_all if v.get("junk")) / max(1, len(normal_all)), 3),
           "normal_triage_states": {s: sum(1 for v in normal_all if (v.get("triage") or {}).get("state") == s) for s in ("done", "pending", "failed", "skipped", "unknown")}}
    return out


# ---------------------------------------------------------------------------------------------
# messaging

def part_messaging(a, d):
    fx = os.path.join(a.work, "msg-fixture")
    gen = os.path.join(QA, "messaging", "realmodel_fixture.py")
    if not os.path.isdir(fx):
        subprocess.run([sys.executable, gen, "--out", fx], check=True)
    qs = [json.loads(l) for l in open(os.path.join(fx, "questions.jsonl"), encoding="utf-8")]
    specs = json.load(open(os.path.join(fx, "sources.json"), encoding="utf-8"))  # [{kind, name, settings}]
    added = []
    for s in specs:
        try:
            added.append(d.call("sources.add", s))
        except RpcError as e:
            added.append({"spec": s, "error": str(e)})
    st = d.wait_idle(timeout=900)
    # wait for embeddings to catch up (queue empty is enough for files; give the embedder time)
    time.sleep(10)
    rows = []
    for q in qs:
        res = ask(d, q["q"], timeout=900)
        ans = res.get("answer", "")
        gold = q.get("gold") or []
        if isinstance(gold, str):
            gold = [gold]
        res["correct"] = bool(gold) and all(re.search(g, ans, re.I) for g in gold)
        cites = res.get("citations") or []
        sup = q.get("support") or q.get("supporting") or []
        if isinstance(sup, str):
            sup = [sup]
        res["cited_support"] = any(any(s.lower() in json.dumps(c, ensure_ascii=False).lower() for s in sup) for c in cites) if sup else None
        cited_n = set(int(n) for n in re.findall(r"\[(\d+)\]", ans))
        res["cited_markers"] = sorted(cited_n)
        res["marker_support"] = any(any(s.lower() in json.dumps(c, ensure_ascii=False).lower() for s in sup) for c in cites if c.get("n") in cited_n) if sup else None
        res["id"] = q.get("id")
        res["gold"] = gold
        res["support"] = sup
        rows.append(res)
        log(q.get("id"), "correct" if res["correct"] else "WRONG", "cited" if res["cited_support"] else "-", ans[:120].replace("\n", " "))
    ctx = d.call("retrieve.context", {"query": qs[0]["q"], "budget_tokens": 2000})
    return {"sources": added, "ingest": st, "rows": rows,
            "context_sensitive": [c.get("sensitive") for c in ctx.get("citations", [])],
            "excluded_sensitive_local": ctx.get("excluded_sensitive")}


# ---------------------------------------------------------------------------------------------

def md(part, res):
    L = [f"## JJ1 real-model: {part}", ""]
    if part == "refine":
        L += ["| refinement-lang | pass |", "|---|---|"] + [f"| {k} | {v} |" for k, v in res["summary"].items()]
        errs = [r for r in res["rows"] if r.get("error")]
        L += ["", f"errors: {len(errs)}"]
    elif part == "triage":
        rows = res["rows"]
        sens = [r for r in rows if r["class"] == "sensitive"]
        L += [f"items {res['items']}, first search {res['first_search_s']}, triaged at {res['t_triaged_s']} s", ""]
        for lang in ("en", "de", "es", "fr"):
            s = [r for r in sens if r["lang"] == lang]
            L.append(f"- sensitive {lang}: {sum(1 for r in s if r.get('sensitive'))}/{len(s)} (model said sensitive: {sum(1 for r in s if r.get('model_sensitive'))})")
        junk = [r for r in rows if r["class"] == "junk"]
        L.append(f"- junk labelled: {sum(1 for r in junk if r.get('junk'))}/{len(junk)}; searchable: {sum(1 for r in junk if r.get('searchable'))}/{sum(1 for r in junk if r.get('probe'))}")
        L.append(f"- normal: sensitive rate {res['normal_sensitive_rate']}, junk rate {res['normal_junk_rate']}, states {res['normal_triage_states']}")
        for k in ("ask_during", "ask_idle"):
            L.append(f"- {k}: " + ", ".join(f"{r.get('first_token_ms')}/{r.get('total_ms')} ms {'ok' if r.get('correct') else 'x'}" for r in res[k]))
    else:
        rows = res["rows"]
        L += [f"correct {sum(1 for r in rows if r.get('correct'))}/{len(rows)}, cited support {sum(1 for r in rows if r.get('cited_support'))}/{len(rows)}", ""]
        L += ["| id | ok | cited | answer |", "|---|---|---|---|"]
        for r in rows:
            L.append(f"| {r.get('id')} | {'✓' if r.get('correct') else '✗'} | {'✓' if r.get('cited_support') else '✗'} | {(r.get('answer') or r.get('error') or '')[:140].replace('|', '/').replace(chr(10), ' ')} |")
    return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--work", default=None)
    ap.add_argument("--part", choices=["refine", "triage", "messaging"], required=True)
    ap.add_argument("--chat-model", default="qwen3-4b-q4km")
    ap.add_argument("--embed-model", default="nomic-embed-text-v1.5-q8")
    ap.add_argument("--models-cache")
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--triage-n", type=int, default=1200)
    ap.add_argument("--triage-timeout", type=int, default=7200)
    a = ap.parse_args()
    a.home, a.out = os.path.abspath(a.home), os.path.abspath(a.out)
    a.work = os.path.abspath(a.work or os.path.join(a.out, "work"))
    os.makedirs(a.out, exist_ok=True)
    os.makedirs(a.work, exist_ok=True)
    d = prepare_home(a)
    d.log = os.path.join(a.out, f"{a.part}-nuggetd.log")
    d.start()
    try:
        install(d, a.chat_model)
        install(d, a.embed_model)
        d.restart()
        log("runtime", wait_runtime(d))
        res = {"refine": part_refine, "triage": part_triage, "messaging": part_messaging}[a.part](a, d)
        res["chat_model"] = a.chat_model
        res["providers"] = d.call("providers.list")
        dump(res, os.path.join(a.out, f"{a.part}.json"))
        open(os.path.join(a.out, f"{a.part}.md"), "w", encoding="utf-8").write(md(a.part, res))
        print(md(a.part, res))
    finally:
        d.stop()
        restore_cache(a)


if __name__ == "__main__":
    main()
