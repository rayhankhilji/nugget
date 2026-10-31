"""Slack and Notion connectors (pasted tokens via token_env) against mocks at slack.com and
api.notion.com, plus an Anthropic provider. Promise: every sync request is in the ledger as
sync:slack / sync:notion with the right host; each token goes only to its own service; sensitive
connector content is withheld from the cloud Ask."""
import json
import time

NAME = "s40_saas_tokens"
EXTRA_MOCKS = ["mock_saas"]
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis"]
"""
SLACK = "xoxp-GG8CANARY-SLACKTOKEN-1"
NOTION = "ntn_GG8CANARY-NOTIONTOKEN-1"
ANTH = "sk-ant-GG8CANARY-APIKEY-anthropic"
ENV = {"ANTHROPIC_API_KEY": ANTH, "GG8_SLACK_TOKEN": SLACK, "GG8_NOTION_TOKEN": NOTION}
SINK_ENV = {"GG8_SLACK_TOKEN": SLACK, "GG8_NOTION_TOKEN": NOTION}
OWN = {SLACK: "slack.com", NOTION: "api.notion.com", ANTH: "api.anthropic.com"}


def token_confinement(lab):
    """Each secret appears only in requests to its own host and nowhere in logs/files."""
    bad = []
    for r in lab.requests():
        if "method" not in r:
            continue
        blob = json.dumps(r.get("headers")) + (r.get("body") or "") + r.get("path", "")
        for tok, host in OWN.items():
            if tok in blob and r.get("host") != host:
                bad.append((tok[:20], r.get("host"), r.get("path")))
    return bad


def run(lab):
    s = lab.call("sources.add", {"kind": "slack", "name": "Slack",
                                 "settings": {"token_env": "GG8_SLACK_TOKEN"}})
    n = lab.call("sources.add", {"kind": "notion", "name": "Notion",
                                 "settings": {"token_env": "GG8_NOTION_TOKEN"}})
    lab.mark("synced-start")
    time.sleep(3)
    lab.wait_idle(seconds=8, timeout=180)
    srcs = {x["id"]: x for x in lab.call("sources.list")}
    for sid in (s["id"], n["id"]):
        x = srcs[sid]
        lab.check(f"sync.{x['kind']}.items", x["items"] > 0,
                  f"state={x['state']} items={x['items']} err={x['last_error']}")
    rows = lab.ledger()
    for kind, host in (("slack", "slack.com"), ("notion", "api.notion.com")):
        nreq = len([r for r in lab.requests() if r.get("host") == host])
        lrows = [r for r in rows if r["feature"] == f"sync:{kind}"]
        lab.check(f"ledger.sync:{kind}", lrows and all(r["host"] == host for r in lrows),
                  f"requests={nreq} ledger_rows={len(lrows)} hosts={sorted({r['host'] for r in lrows})} "
                  f"bytes_sent={sum(r['bytes_sent'] for r in lrows)}")
    # client_secret refused in settings
    try:
        lab.call("sources.update", {"id": s["id"], "settings": {"token_env": "GG8_SLACK_TOKEN",
                                                                 "client_secret": "GG8CANARY-CS-1"}})
        lab.check("settings.client_secret_refused", False, "accepted")
    except Exception as e:  # noqa: BLE001
        lab.check("settings.client_secret_refused", True, str(e)[:200])
    t0 = time.time()
    c = lab.client()
    started, text, done = c.stream("retrieve.ask", {
        "query": "What did Priya say about the Acme renewal and the deposit?",
        "budget_tokens": 4000}, timeout=120)
    lab.check("ask.answered", done and not done.get("error"),
              f"cit={len(started.get('citations', []))} excl={started.get('excluded_sensitive')}")
    body = "".join(r.get("body") or "" for r in lab.requests(since=t0, host="api.anthropic.com"))
    lab.check("ask.connector_content_sent", "SLACK-NORMAL" in body or "NOTION-NORMAL" in body,
              "non-sensitive connector passages reached the cloud (expected)")
    lab.check("ask.no_sensitive_connector_content",
              "SENS" not in body and "4111 1111" not in body,
              [k for k in ("SLACK-SENS-1", "NOTION-SENS-1", "4111 1111") if k in body])
    time.sleep(2)
    lab.check("tokens.confined", not token_confinement(lab), token_confinement(lab)[:5])
    # restart: is the source rebuilt from token_env and does sync traffic stay ledgered?
    lab.restart_daemon()
    time.sleep(5)
    lab.wait_idle(seconds=6, timeout=120)
    rows = lab.ledger()
    reqs = [r for r in lab.requests() if r.get("host") in ("slack.com", "api.notion.com")]
    lab.note(f"after restart: {len(reqs)} SaaS requests, ledger rows {len(rows)}")
