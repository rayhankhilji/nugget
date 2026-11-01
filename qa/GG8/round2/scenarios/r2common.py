"""Round 2 helpers: seed the real models (downloaded once into /tmp/gg8-models by the daemon's own
models.install) into a lab home, and wait for background triage."""
import json
import os
import shutil
import subprocess
import time

MODELS_HOME = os.environ.get("GG8_MODELS_HOME", "/tmp/gg8-models")
R2 = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUNTIME = """
[runtime]
chat_model = "qwen3-1.7b-q8"
embedding_model = "nomic-embed-text-v1.5-q8"
"""


def seed(lab, corpus=True, chats=True, private_chats=True):
    os.makedirs(lab.data, exist_ok=True)
    src = os.path.join(MODELS_HOME, "data", "models")
    subprocess.run(["cp", "-al", src, os.path.join(lab.data, "models")], check=True)
    lab.start_daemon()
    if corpus:
        shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", f"{lab.home}/Documents")
        lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [f"{lab.home}/Documents"]}})
    if chats:
        shutil.copytree(f"{R2}/fixtures/whatsapp", f"{lab.home}/wa")
        shutil.copytree(f"{R2}/fixtures/telegram", f"{lab.home}/tg")
        lab.call("sources.add", {"kind": "whatsapp", "name": "WhatsApp", "settings": {"folder": f"{lab.home}/wa", "private": private_chats}})
        lab.call("sources.add", {"kind": "telegram", "name": "Telegram", "settings": {"path": f"{lab.home}/tg", "private": private_chats}})


def wait_runtime(lab, timeout=600):
    end = time.time() + timeout
    while time.time() < end:
        rs = lab.call("runtime.status")
        if all(r["state"] == "ready" for r in rs if r["role"] in ("chat", "embedding")):
            return rs
        time.sleep(5)
    return lab.call("runtime.status")


def sensitivity(lab, query="Acme renewal dinner deck Lisbon", limit=50):
    fac = lab.call("search.faceted", {"query": query, "limit": limit, "offset": 0, "filters": {}})
    return {h.get("title"): h.get("sensitive") for h in fac.get("hits") or []}


def wait_triage(lab, normal_titles, timeout=900):
    """Wait until every title in normal_titles is no longer flagged sensitive."""
    end = time.time() + timeout
    flags = {}
    while time.time() < end:
        flags = sensitivity(lab)
        if all(flags.get(t) is False for t in normal_titles):
            return True, flags
        time.sleep(10)
    return False, flags


def bodies(lab, since, host="api.anthropic.com"):
    return [r for r in lab.requests(since=since) if r.get("host") == host and "method" in r]


def canaries(text):
    out = set()
    i = 0
    while (i := text.find("GG8CANARY-", i)) >= 0:
        j = i
        while j < len(text) and (text[j].isalnum() or text[j] in "-_"):
            j += 1
        out.add(text[i:j])
        i = j
    return sorted(out)


def save(lab, name, obj):
    with open(os.path.join(lab.out, name), "w") as f:
        json.dump(obj, f, indent=1)
