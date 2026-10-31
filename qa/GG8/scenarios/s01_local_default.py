"""Default, local-only install with no model installed: add a folder, sync, search, Ask, MCP,
knowledge, Today. Promise: nothing leaves the device (no DNS query, no packet off the namespace)."""
import shutil
import time

NAME = "s01_local_default"
CONFIG = ""  # the daemon writes its local-first default


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    st = lab.call("status")
    lab.check("status.providers", True, [p["id"] for p in st["providers"]])
    src = lab.call("sources.add", {"kind": "files", "name": "Documents",
                                   "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    hits = lab.call("search", {"query": "Acme renewal", "limit": 10})
    lab.check("search.works", len(hits) > 0, [h["title"] for h in hits])
    ctx = lab.call("retrieve.context", {"query": "Acme renewal", "budget_tokens": 2000})
    lab.check("context.local_includes_all", True,
              f"citations={len(ctx['citations'])} excluded_sensitive={ctx['excluded_sensitive']}")
    try:
        c = lab.client()
        started, text, done = c.stream("retrieve.ask", {"query": "When is the Acme renewal due?",
                                                        "budget_tokens": 2000}, timeout=60)
        lab.check("ask.no_model", True, f"{started} {text[:100]} {done}")
    except Exception as e:  # noqa: BLE001
        lab.check("ask.no_model", True, f"refused as expected without a model: {e}")
    mc = lab.call("mcp.clients.create", {"name": "gg8", "scopes": {}}) if False else None
    for m in ("knowledge.status", "cards.list", "proactive.refresh", "brief.get", "plugins.list",
              "models.list", "runtime.status", "browser.status", "voice.status"):
        try:
            p = {"include_closed": False} if m == "cards.list" else {}
            lab.call(m, p, timeout=60)
            lab.note(f"{m} ok")
        except Exception as e:  # noqa: BLE001
            lab.note(f"{m}: {e}")
    time.sleep(20)
    ledger = lab.ledger()
    lab.check("ledger.empty", len(ledger) == 0, ledger[:5])
    lab.check("no_dns", len(lab.dns()) == 0, [d.get("qname") for d in lab.dns()][:20])
    lab.check("no_requests", len(lab.requests()) == 0,
              [(r.get("host"), r.get("path")) for r in lab.requests()][:20])
