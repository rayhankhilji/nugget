"""Round 2, local only (default config, no model installed): first run, folders plus WhatsApp and
Telegram imports, faceted search, timeline, preview, quick-ask-style Ask, draft.check_sensitive,
plugins.catalog, knowledge graph with sensitive filter, MCP, then 5 minutes idle and a restart.
Promise: nothing leaves the device; the messaging importers make no request at all."""
import json
import os
import shutil
import time

NAME = "r01_local_new_features"
CONFIG = ""
R2 = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IDLE = int(os.environ.get("GG8_IDLE_SECS", "300"))


def tryc(lab, m, p=None, timeout=60):
    try:
        r = lab.call(m, p or {}, timeout=timeout)
        lab.note(f"{m}: ok {json.dumps(r)[:200]}")
        return r
    except Exception as e:  # noqa: BLE001
        lab.note(f"{m}: {e}")
        return None


def run(lab):
    docs = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", docs)
    shutil.copytree(f"{R2}/fixtures/whatsapp", f"{lab.home}/wa")
    shutil.copytree(f"{R2}/fixtures/telegram", f"{lab.home}/tg")
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [docs]}})
    wa = tryc(lab, "sources.add", {"kind": "whatsapp", "name": "WhatsApp", "settings": {"folder": f"{lab.home}/wa"}})
    tg = tryc(lab, "sources.add", {"kind": "telegram", "name": "Telegram", "settings": {"path": f"{lab.home}/tg"}})
    lab.check("imports.added", bool(wa) and bool(tg), [bool(wa), bool(tg)])
    lab.wait_idle(seconds=8, timeout=180)
    srcs = lab.call("sources.list")
    lab.note("sources " + json.dumps([(s["id"], s["kind"], s["items"], s["state"], s["last_error"]) for s in srcs]))
    hits = lab.call("search", {"query": "renewal dinner", "limit": 10})
    lab.check("search.finds_chats", any("GG8CANARY" in json.dumps(h) or "dinner" in json.dumps(h).lower() for h in hits),
              [h["title"] for h in hits])
    fac = tryc(lab, "search.faceted", {"query": "Acme renewal", "limit": 20, "offset": 0, "filters": {}})
    if fac:
        hs = fac.get("hits") or []
        lab.note("faceted sensitive flags " + json.dumps([(h.get("title"), h.get("sensitive")) for h in hs]))
        lab.check("faceted.chats_marked_sensitive",
                  all(h.get("sensitive") for h in hs if (h.get("item") or {}).get("source", "").startswith(("whatsapp", "telegram"))),
                  [(h.get("title"), h.get("sensitive")) for h in hs])
    tryc(lab, "draft.check_sensitive", {"text": "card 4111 1111 1111 1111"})
    tryc(lab, "plugins.catalog")
    tryc(lab, "knowledge.graph", {"center": None, "depth": 1, "limit": 50, "kinds": [], "include_sensitive": False})
    try:
        c = lab.client()
        c.stream("retrieve.ask", {"query": "When is the renewal dinner?", "budget_tokens": 2000,
                                  "provider": None, "tier": "synthesis", "utc_offset_minutes": 60}, timeout=60)
    except Exception as e:  # noqa: BLE001
        lab.note(f"ask without a model: {e}")
    for m, p in (("knowledge.status", {}), ("cards.list", {"include_closed": False}), ("proactive.refresh", {}),
                 ("brief.get", {}), ("models.list", {}), ("runtime.status", {}), ("status", {})):
        tryc(lab, m, p)
    lab.mark("idle")
    time.sleep(IDLE)
    lab.restart_daemon()
    time.sleep(60)
    lab.check("ledger.empty", lab.ledger() == [], lab.ledger()[:3])
    lab.check("no_dns", not lab.dns(), sorted({d.get("qname") for d in lab.dns()}))
    lab.check("no_requests", not lab.requests(), [(r.get("host"), r.get("path")) for r in lab.requests()][:10])
    lab.check("no_packets_off_namespace", not lab.flows(), lab.flows()[:10])
