"""Regenerate the draft fixtures (bench/fixtures/draft/*.json) from the tables in this folder.

    python3 -I bench/fixtures/draft/generator/gen.py bench/fixtures/draft

Deterministic (seeded). Everything is fictional and uses only example.com / .test domains. The
JSON files are what the suite reads; edit the tables here and regenerate, then run
`cargo test -p nugget-bench` (the loader validates references, gold facts and secrets).
"""
import json, random, sys, datetime, os
sys.path.insert(0, os.path.dirname(__file__))
from knowledge import K, SECRETS
from personas import CONTACTS, PERSONAS, POOLS, EMOJI
from tasks import T

OUT = sys.argv[1]
rng = random.Random(7)

def first(cid): return CONTACTS[cid][0].split()[0]
def internal(cid): return CONTACTS[cid][2]

def lower_start(s): return s[:1].lower() + s[1:]

def render(p, channel, cid, body, emoji=None):
    f = first(cid)
    if p == "maya":
        if channel == "email":
            sign = "Thanks" if internal(cid) else "Best"
            return f"Hi {f},\n\n{body}\n\n{sign},\nMaya"
        return body
    if p == "henrik":
        if channel == "email":
            return f"Dear {f},\n\n{body}\n\nKind regards,\nHenrik Larsen"
        return f"Hello {f},\n{body}\nRegards, Henrik"
    if p == "jules":
        if channel == "email":
            g = "Hey" if internal(cid) else "Hi"
            return f"{g} {f}!\n\n{body}\n\nCheers,\nJules"
        return body
    raise ValueError(p)

APPS = {
 "email": [("Mail","com.apple.mail"),("Outlook","com.microsoft.Outlook"),("Thunderbird","org.mozilla.Thunderbird")],
 "chat": [("Slack","com.tinyspeck.slackmacgap"),("Microsoft Teams","com.microsoft.teams2")],
}
COUNTS = {"maya": (24,16), "henrik": (26,14), "jules": (20,20)}

personas_out = []
for pid, p in PERSONAS.items():
    n_email, n_chat = COUNTS[pid]
    contacts = p["contacts"]
    internal_contacts = [c for c in contacts if internal(c)]
    msgs = []
    start = datetime.date(2026, 6, 1)
    total = n_email + n_chat
    channels = ["email"]*n_email + ["chat"]*n_chat
    rng.shuffle(channels)
    for i, ch in enumerate(channels):
        day = start + datetime.timedelta(days=int(i * 147 / total))
        if ch == "email":
            cid = contacts[i % len(contacts)]
            k = {"maya": (2,4), "henrik": (4,6), "jules": (2,3)}[pid]
            body = " ".join(rng.sample(POOLS[pid]["email"], rng.randint(*k)))
            if pid == "jules" and rng.random() < 0.8:
                body += " " + rng.choice(EMOJI["jules"])
        else:
            cid = internal_contacts[i % len(internal_contacts)]
            k = {"maya": (1,2), "henrik": (1,2), "jules": (1,2)}[pid]
            body = (" " if pid == "henrik" else ", ").join(rng.sample(POOLS[pid]["chat"], rng.randint(*k)))
            if pid in ("maya", "jules"):
                body = lower_start(body)
            if pid == "maya" and rng.random() < 0.5:
                body += " :)"
            if pid == "jules":
                if rng.random() < 0.4:
                    body += "!!"
                body += " " + rng.choice(EMOJI["jules"])
        app = APPS[ch][i % len(APPS[ch])]
        msgs.append({
            "id": f"{pid}-s{i+1:02}",
            "channel": ch,
            "to": CONTACTS[cid][1],
            "to_name": CONTACTS[cid][0],
            "date": day.isoformat(),
            "app": app[0],
            "text": render(pid, ch, cid, body),
        })
    personas_out.append({"id": pid, "name": p["name"], "email": p["email"], "role": p["role"],
                         "voice": p["voice"], "sent": msgs})

knowledge_out = {
  "items": [{"id": i, "date": d, "kind": k, "title": ti, "body": b, "sensitive": s} for (i,d,k,ti,b,s) in K],
  "secrets": [{"item": i, "kind": k, "value": v} for (i,k,v) in SECRETS],
}
ids = {i for (i,*_) in K}

tasks_out = []
counter = {}
for task in T:
    pid = task["persona"]
    counter[pid] = counter.get(pid, 0) + 1
    tid = f"{pid}-t{counter[pid]:02}"
    ch = task["channel"]; cid = task["contact"]
    app = APPS[ch][counter[pid] % len(APPS[ch])]
    name, email, _ = CONTACTS[cid]
    thread = []
    if ch == "email":
        ctx = {
          "app": {"id": app[1], "name": app[0], "window_title": f"Re: {task['subject']}"},
          "field": {"role": "text_area", "label": "Message body"},
          "thread": [{"author": f"{name} <{email}>", "text": task["incoming"], "from_me": False,
                      "timestamp": "2026-10-27T09:15:00Z"}],
          "recipients": [email],
          "subject": f"Re: {task['subject']}",
        }
        kind = "email_body"
    else:
        ctx = {
          "app": {"id": app[1], "name": app[0], "window_title": name},
          "field": {"role": "text_area", "label": f"Message {name}"},
          "thread": [{"author": name, "text": task["incoming"], "from_me": False,
                      "timestamp": "2026-10-27T09:15:00Z"}],
          "recipients": [],
        }
        kind = "chat_message"
    for g in task["gold"]:
        for it in g[2]:
            assert it in ids, (tid, it)
    tasks_out.append({
      "id": tid, "persona": pid, "channel": ch, "contact": email, "contact_name": name,
      "expected_kind": kind, "context": ctx,
      "gold_facts": [{"label": l, "any_of": a, "items": it} for (l,a,it) in task["gold"]],
      "forbidden_facts": [{"label": l, "any_of": a} for (l,a) in task["forbidden"]],
      "reference": render(pid, ch, cid, task["ref"]),
      "tags": task["tags"],
      "injection_markers": task["payload"],
    })
    if task["traps"]:
        tasks_out[-1]["artefact_traps"] = [{"label": l, "any_of": a} for (l,a) in task["traps"]]
    # The reference must carry every gold fact and no forbidden one.
    ref = tasks_out[-1]["reference"].lower()
    for g in task["gold"]:
        assert any(a.lower() in ref for a in g[1]), (tid, g)
    for f in task["forbidden"]:
        assert not any(a.lower() in ref for a in f[1]), (tid, f)
    for f in task["traps"]:
        assert not any(a.lower() in ref for a in f[1]), (tid, f)

os.makedirs(OUT, exist_ok=True)
def dump(name, obj):
    with open(os.path.join(OUT, name), "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=1)
        fh.write("\n")
dump("personas.json", {"personas": personas_out})
dump("knowledge.json", knowledge_out)
dump("tasks.json", {"tasks": tasks_out})
print(len(personas_out), sum(len(p["sent"]) for p in personas_out), len(K), len(tasks_out))
