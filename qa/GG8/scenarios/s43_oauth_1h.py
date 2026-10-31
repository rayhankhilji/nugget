"""As s42 with realistic 1-hour access tokens. OAuth sign-in for gmail, outlook, linear and github (device flow), and a CalDAV server whose
calendar-home-set points at another host. Promise: sources.auth.begin contacts nothing (or only the
documented provider host for the device flow) before the user acts; after the user completes the
Google redirect, the code exchange and the refresh go only to Google, are ledgered as sync:gmail, and
tokens/client secrets go nowhere else. CalDAV credentials are not sent to the foreign host."""
import json
import time
from urllib.parse import parse_qs, urlparse

NAME = "s43_oauth_1h"
EXTRA_MOCKS = ["mock_oauth", "mock_caldav"]
SINK_ENV = {"GG8_OAUTH_EXPIRES": "3600", "GG8_CALDAV_EVIL_HOME": "1", "GG8_CALDAV_PASSWORD": "GG8CANARY-CALDAVPW-2"}
CONFIG = ""
CS = "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e"


def run(lab):
    begun = {}
    for kind, settings in (
            ("gmail", {"client_id": "gg8-client.apps.googleusercontent.com"}),
            ("outlook", {"client_id": "00000000-0000-0000-0000-0000000000a1"}),
            ("linear", {"client_id": "gg8linearclient"}),
            ("github", {"client_id": "Iv1.gg8githubclient"})):
        s = lab.call("sources.add", {"kind": kind, "name": kind, "settings": settings})
        if kind == "gmail":
            lab.call("sources.secret.set", {"id": s["id"], "secret": CS})
        t0 = time.time()
        try:
            b = lab.call("sources.auth.begin", {"id": s["id"]})
        except Exception as e:  # noqa: BLE001
            lab.check(f"{kind}.auth_begin", False, str(e)[:200])
            continue
        time.sleep(4)
        reqs = [(r.get("host"), r.get("method"), r.get("path")) for r in lab.requests(since=t0) if "method" in r]
        begun[kind] = (s, b)
        u = urlparse(b["url"])
        allowed = {"github": {"github.com"}}.get(kind, set())
        lab.check(f"{kind}.auth_begin_contacts", all(h in allowed for h, _, _ in reqs),
                  f"url_host={u.netloc} requests={reqs}")
        lab.check(f"{kind}.auth_url_has_no_secret", CS not in b["url"], u.netloc)
    # Complete the Google redirect as the browser would.
    if "gmail" in begun:
        s, b = begun["gmail"]
        q = {k: v[0] for k, v in parse_qs(urlparse(b["url"]).query).items()}
        redirect = q.get("redirect_uri")
        t0 = time.time()
        r = lab.ns_run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                        f"{redirect}?code=GG8CANARY-AUTHCODE&state={q.get('state')}&scope=x"],
                       capture_output=True, text=True, timeout=30)
        lab.note(f"redirect -> {r.stdout} {redirect}")
        time.sleep(10)
        lab.wait_idle(seconds=5, timeout=90)
        tok = lab.requests(since=t0, host="oauth2.googleapis.com")
        lab.check("gmail.code_exchanged", any("GG8CANARY-AUTHCODE" in (x.get("body") or "") for x in tok),
                  [(x["method"], x["path"]) for x in tok])
        rows = lab.ledger()
        grows = [x for x in rows if x["host"] == "oauth2.googleapis.com"]
        lab.check("ledger.token_exchange", grows and all(x["feature"] == "sync:gmail" for x in grows),
                  [(x["feature"], x["host"]) for x in grows])
        # wait for the 60 s token to expire and a sync to refresh it
        time.sleep(75)
        lab.call("sources.sync", {"id": s["id"]})
        time.sleep(10)
        tok2 = [x for x in lab.requests(host="oauth2.googleapis.com") if "refresh_token" in (x.get("body") or "")]
        lab.note(f"refresh requests: {len(tok2)}")
        rows = lab.ledger()
        lab.check("ledger.google_hosts_all_ledgered",
                  {x.get("host") for x in lab.requests() if "google" in (x.get("host") or "")}
                  <= {x["host"] for x in rows}, sorted({x["host"] for x in rows}))
    # Token / secret confinement
    leaks = []
    for x in lab.requests():
        if "method" not in x:
            continue
        blob = json.dumps(x.get("headers")) + (x.get("body") or "") + x.get("path", "")
        for canary, hosts in (("GG8CANARY-ACCESS-google", ("gmail.googleapis.com", "www.googleapis.com", "oauth2.googleapis.com")),
                              ("GG8CANARY-REFRESH-google", ("oauth2.googleapis.com",)),
                              ("CLIENTSECRET-google", ("oauth2.googleapis.com",)),
                              ("GG8CANARY-AUTHCODE", ("oauth2.googleapis.com",))):
            if canary in blob and x.get("host") not in hosts:
                leaks.append((canary, x.get("host"), x.get("path")))
    lab.check("oauth.tokens_confined", not leaks, leaks)
    hits = [c for c in lab.canary_hits() if not c["where"].startswith("request")]
    lab.check("oauth.tokens_not_in_logs_or_files", not hits, hits)
    # CalDAV whose home set points at a foreign host
    t0 = time.time()
    cal = lab.call("sources.add", {"kind": "caldav", "name": "cal", "settings": {
        "url": "https://caldav.gg8-synthetic.test/", "username": "ada@gg8-synthetic.test"}})
    lab.call("sources.secret.set", {"id": cal["id"], "secret": "GG8CANARY-CALDAVPW-2"})
    lab.call("sources.sync", {"id": cal["id"]})
    time.sleep(8)
    evil = lab.requests(since=t0, host="collector.gg8-evil.test")
    x = next(s for s in lab.call("sources.list") if s["id"] == cal["id"])
    lab.check("caldav.no_credentials_to_foreign_home", not evil,
              f"foreign requests={len(evil)} state={x['state']} err={x['last_error']}")
