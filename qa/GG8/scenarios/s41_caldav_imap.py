"""CalDAV over https (mock at caldav.gg8-synthetic.test), CalDAV over plain http to a LAN address
(must be refused), and IMAP to a sinkholed host on 993 (implicit TLS) and 143 (STARTTLS), where only
a raw TCP logger listens. Promise: sync requests ledgered as sync:caldav; the password goes only to
the CalDAV host and only inside TLS; no plaintext credential to IMAP; failed attempts visible."""
import base64
import json
import time

NAME = "s41_caldav_imap"
EXTRA_MOCKS = ["mock_caldav"]
PW_CAL = "GG8CANARY-CALDAVPW-1"
PW_IMAP = "GG8CANARY-IMAPPW-1"
SINK_ENV = {"GG8_CALDAV_PASSWORD": PW_CAL}
CONFIG = ""


def wait_state(lab, sid, timeout=60):
    end = time.time() + timeout
    x = None
    while time.time() < end:
        x = next(s for s in lab.call("sources.list") if s["id"] == sid)
        if x["state"] not in ("syncing",) and (x["last_sync"] or x["last_error"]):
            return x
        time.sleep(1)
    return x


def run(lab):
    cal = lab.call("sources.add", {"kind": "caldav", "name": "Work calendar", "settings": {
        "url": "https://caldav.gg8-synthetic.test/", "username": "ada@gg8-synthetic.test"}})
    lab.check("caldav.needs_auth_before_secret", cal["state"] == "needs_auth", cal["state"])
    lab.check("caldav.nothing_before_secret",
              not lab.requests(host="caldav.gg8-synthetic.test"),
              len(lab.requests(host="caldav.gg8-synthetic.test")))
    lab.call("sources.secret.set", {"id": cal["id"], "secret": PW_CAL})
    lab.call("sources.sync", {"id": cal["id"]})
    time.sleep(3)
    lab.wait_idle(seconds=5, timeout=120)
    x = next(s for s in lab.call("sources.list") if s["id"] == cal["id"])
    lab.check("caldav.synced", x["items"] >= 2, f"state={x['state']} items={x['items']} err={x['last_error']}")
    reqs = lab.requests(host="caldav.gg8-synthetic.test")
    rows = [r for r in lab.ledger() if r["feature"] == "sync:caldav"]
    lab.check("ledger.sync:caldav", len(rows) >= 1 and all(r["host"] == "caldav.gg8-synthetic.test" for r in rows),
              f"requests={len(reqs)} rows={len(rows)} methods={[r['method'] for r in reqs]}")
    want = "Basic " + base64.b64encode(f"ada@gg8-synthetic.test:{PW_CAL}".encode()).decode()
    leaks = [(r.get("host"), r.get("path")) for r in lab.requests() if "method" in r
             and want in json.dumps(r.get("headers")) and r.get("host") != "caldav.gg8-synthetic.test"]
    lab.check("caldav.password_confined", not leaks, leaks)
    lab.check("caldav.only_https", all(r["scheme"] == "https" for r in reqs), {r["scheme"] for r in reqs})

    # Plain http to a LAN CalDAV server: documented as refused (https only, http only to loopback).
    try:
        lan = lab.call("sources.add", {"kind": "caldav", "name": "LAN cal", "settings": {
            "url": f"http://{lab.fmt('{lan_ip}')}:8080/", "username": "ada"}})
        lab.call("sources.secret.set", {"id": lan["id"], "secret": PW_CAL})
        lab.call("sources.sync", {"id": lan["id"]})
        time.sleep(5)
        x = next(s for s in lab.call("sources.list") if s["id"] == lan["id"])
        lanreq = [r for r in lab.requests() if r.get("listen", "").startswith(lab.fmt("{lan_ip}"))]
        lab.check("caldav.http_lan_refused", not lanreq,
                  f"add accepted; state={x['state']} err={x['last_error']} lan_requests={len(lanreq)}")
    except Exception as e:  # noqa: BLE001
        lab.check("caldav.http_lan_refused", True, f"sources.add refused: {e}")

    # IMAP, implicit TLS on 993 and STARTTLS on 143, to a host where only a raw logger listens.
    t0 = time.time()
    ids = []
    for tls, port in (("implicit", 993), ("starttls", 143)):
        s = lab.call("sources.add", {"kind": "imap", "name": f"Mail {tls}", "settings": {
            "host": "imap.gg8-synthetic.test", "port": port, "tls": tls,
            "username": "ada@gg8-synthetic.test"}})
        lab.call("sources.secret.set", {"id": s["id"], "secret": PW_IMAP})
        lab.call("sources.sync", {"id": s["id"]})
        ids.append(s["id"])
    time.sleep(25)
    raw = [r for r in lab.requests(since=t0) if r.get("raw_tcp") or r.get("tls_handshake_error")]
    firsts = [(r["listen"], base64.b64decode(r.get("first_bytes_b64", ""))[:16].hex()) for r in raw]
    plain = [r for r in raw if PW_IMAP.encode() in base64.b64decode(r.get("first_bytes_b64", ""))
             or b"LOGIN" in base64.b64decode(r.get("first_bytes_b64", "")).upper()]
    lab.check("imap.attempted", len(raw) >= 1, firsts)
    lab.check("imap.no_plaintext_credentials", not plain, len(plain))
    ok993 = [f for f in firsts if f[0].endswith(":993")]
    lab.check("imap.993_starts_tls", all(h.startswith("16") for _, h in ok993) and ok993, ok993)
    ok143 = [f for f in firsts if f[0].endswith(":143")]
    lab.check("imap.143_sends_nothing_before_greeting", all(h == "" for _, h in ok143) and ok143, ok143)
    srcs = {s["id"]: s for s in lab.call("sources.list")}
    for sid in ids:
        lab.note(f"{sid}: state={srcs[sid]['state']} err={srcs[sid]['last_error']}")
    rows = [r for r in lab.ledger() if r["feature"] == "sync:imap"]
    lab.check("ledger.sync:imap_failed_attempts", len(rows) >= 1,
              f"connections={len(raw)} ledger_rows={len(rows)} {rows[:3]}")
    lab.check("password.not_in_logs", not any(
        c["canary"] in ("GG8CANARY-CALDAVPW-1", "GG8CANARY-IMAPPW-1") and not c["where"].startswith("request")
        for c in lab.canary_hits()), [c for c in lab.canary_hits() if "PW" in c["canary"]])
