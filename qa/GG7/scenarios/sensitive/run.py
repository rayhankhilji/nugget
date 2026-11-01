#!/usr/bin/env python3
"""Index items.py + qa/GG7/corpus with nuggetd (no models), read `sensitive` per item over IPC
(search.faceted), print a per-language table. run.py [--nuggetd P] [--home D] [--default]
Without --default: `assume_unflagged_safe = true`, i.e. the rule layer only."""
import argparse, email, email.policy, re, sys, time
from email.message import EmailMessage
from pathlib import Path
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "harness"))
sys.dont_write_bytecode = True
import gg7  # noqa: E402
from items import ITEMS, SENSITIVE  # noqa: E402

p = argparse.ArgumentParser()
p.add_argument("--nuggetd", default=str(HERE.parents[3] / "target/debug/nuggetd"))
p.add_argument("--home", default="/tmp/claude-0/gg7s")
p.add_argument("--default", action="store_true")
# With a chat model: the shipped default plus the local triage model's verdicts.
p.add_argument("--chat-model")
p.add_argument("--embed-model", default="nomic-embed-text-v1.5-q8")
a = p.parse_args()
a.out, a.no_knowledge = a.home, True
a.no_models = not a.chat_model
if a.no_models:
    a.chat_model, a.embed_model = None, None
else:
    a.default = True
home, sset = Path(a.home), Path(a.home) / "set"
for lang, rows in ITEMS.items():
    (sset / lang).mkdir(parents=True, exist_ok=True)
    for i, (sit, frm, subj, body) in enumerate(rows):
        m = EmailMessage()
        m["From"], m["To"], m["Subject"] = frm, "Sam Okafor <sam.okafor@example.net>", subj
        m["Date"], m["Message-ID"] = "Tue, 27 Oct 2026 09:14:00 +0000", f"<{lang}-{sit}@gg7s.example>"
        m.set_content(body + "\n", charset="utf-8", cte="8bit")
        (sset / lang / f"{i:02d}-{sit}.eml").write_bytes(m.as_bytes())
if not a.default:
    base = gg7.config_toml
    gg7.config_toml = lambda a: base(a) + "\n[ingest]\nassume_unflagged_safe = true\n"
Path(a.home).mkdir(parents=True, exist_ok=True)
proc, ipc = gg7.start_daemon(a)
try:
    if not a.no_models:
        gg7.install_models(ipc, [a.chat_model, a.embed_model])
        gg7.wait_runtime(ipc)
    corpus = gg7.CORPUS
    src = ipc.call("sources.add", {"kind": "files", "name": "s", "settings": {"roots": [str(sset), str(corpus)]}})
    ipc.call("sources.sync", {"id": src["id"]})
    files = sorted(sset.rglob("*.eml")) + sorted(f for f in corpus.rglob("*") if f.suffix in (".eml", ".md", ".ics") and f.name != "SPEC.md")
    for _ in range(1800):
        info = [s for s in ipc.call("sources.list") if s["id"] == src["id"]][0]
        if info["items"] >= len(files) and not ipc.call("ingest.status").get("queue_depth"):
            break
        time.sleep(1)
    if not a.no_models:
        print("triage:", gg7.wait_triage(ipc))

    def queries(f):
        raw = f.read_bytes()
        if f.suffix == ".eml":
            m = email.message_from_bytes(raw, policy=email.policy.default)
            b = m.get_body(preferencelist=("plain",))
            text = str(m["Subject"] or "") + "\n" + (b.get_content() if b else "")
        else:
            text = raw.decode("utf-8", "replace")
        yield from [l for l in text.splitlines() if l.strip()][:3]
        yield from sorted(set(re.findall(r"\w{6,}", text)), key=len, reverse=True)[:6]

    def sensitive(f):
        for q in queries(f):
            hits = ipc.call("search.faceted", {"query": q[:200], "limit": 100})["hits"]
            for h in hits:
                if h["item"]["key"].endswith(f.parent.name + "/" + f.name):
                    return h["sensitive"]
        return None

    table, fp, unread = {}, [], []
    for f in files:
        s = sensitive(f)
        if s is None:
            unread.append(f)
        elif f.is_relative_to(sset):
            lang, sit = f.parent.name, f.stem.split("-", 1)[1]
            t = table.setdefault(lang, [0, 0, []])
            t[0 if sit in SENSITIVE else 1] += s
            if (sit in SENSITIVE) != s:
                t[2].append(("MISS " if sit in SENSITIVE else "FP ") + sit)
        elif s:
            fp.append(str(f.relative_to(corpus)))
    print("mode:", f"rules + triage by {a.chat_model}" if not a.no_models
          else "default (unknown = sensitive)" if a.default else "rule layer (assume_unflagged_safe)")
    print(f"{'lang':6} sens/5 ord-wrong/2  misses")
    for lang in ITEMS:
        t = table.get(lang, [0, 0, ["(not read)"]])
        print(f"{lang:6} {t[0]}/5    {t[1]}/2          {', '.join(t[2])}")
    ncorp = len([f for f in files if not f.is_relative_to(sset)])
    print(f"GG7 corpus marked sensitive: {len(fp)}/{ncorp} {fp}")
    print("not found over IPC:", [str(f) for f in unread])
finally:
    proc.terminate()
    proc.wait(30)
