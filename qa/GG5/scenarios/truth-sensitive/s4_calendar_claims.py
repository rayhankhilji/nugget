#!/usr/bin/env python3
"""truth-sensitive S4: what the calendar extractor asserts as "attended", and whether a vault
footnote's nugget://item URL resolves.

Inputs (written here, dated by filename as the harness does):
- Calendar/2026-12-03-harbor-launch.ics   a FUTURE event (today is 2026-10-30), both PARTSTAT=ACCEPTED
- Calendar/2026-09-10-offsite.ics          STATUS:CANCELLED, attendee accepted before cancellation
- plus the phase1 Calendar/ items (a weekly RRULE stand-up among them)
Captures every claim text + quote, the vault page of the launch event, and MCP resources/read of
each footnote URL found in the vault (include_sensitive client).

Usage: python3 qa/GG5/scenarios/truth-sensitive/s4_calendar_claims.py <workdir>
"""
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
sys.path.insert(0, str(HERE))
import nugget_qa  # noqa: E402
from s1_sensitive import BIN  # noqa: E402

def ics(uid, start, end, summary, extra, organizer, attendees):
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Nugget QA//GG5//EN", "BEGIN:VEVENT",
             f"UID:{uid}", "DTSTAMP:20261001T090000Z", f"DTSTART:{start}", f"DTEND:{end}",
             f"SUMMARY:{summary}", *extra, f"ORGANIZER;CN=\"{organizer[0]}\":mailto:{organizer[1]}"]
    for n, e, st in attendees:
        lines.append(f"ATTENDEE;CN=\"{n}\";PARTSTAT={st}:mailto:{e}")
    lines += ["END:VEVENT", "END:VCALENDAR", ""]
    return "\r\n".join(lines)

MARA = ("Mara Okafor", "mara.okafor@brightwell.example")
ROB = ("Robert Chen", "robert.chen@brightwell.example")
FILES = {
    "Calendar/2026-12-03-harbor-launch.ics": ics("launch@qa.example", "20261203T090000Z", "20261203T170000Z",
        "Harbor launch day", ["LOCATION:Brightwell HQ, Room Fathom"], MARA,
        [(*MARA, "ACCEPTED"), (*ROB, "ACCEPTED")]),
    "Calendar/2026-09-10-offsite.ics": ics("offsite@qa.example", "20260910T090000Z", "20260910T170000Z",
        "Team offsite", ["STATUS:CANCELLED", "LOCATION:Gull Point Lodge"], ROB,
        [(*ROB, "ACCEPTED"), (*MARA, "ACCEPTED")]),
}


def mcp_read(home, cid, token, uris):
    env = dict(os.environ, NUGGET_MCP_TOKEN=token)
    p = subprocess.Popen([BIN, "--home", str(home), "mcp", "--client", cid], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
    def rt(obj, want=None):
        p.stdin.write(json.dumps(obj) + "\n"); p.stdin.flush()
        if want is None:
            return None
        while True:
            m = json.loads(p.stdout.readline())
            if m.get("id") == want:
                return m
    rt({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "qa", "version": "0"}}}, 0)
    rt({"jsonrpc": "2.0", "method": "notifications/initialized"})
    out = {}
    for i, u in enumerate(uris, 1):
        r = rt({"jsonrpc": "2.0", "id": i, "method": "resources/read", "params": {"uri": u}}, i)
        out[u] = json.dumps(r.get("result") or r.get("error"))[:300]
    p.terminate()
    return out


def main():
    work = Path(sys.argv[1]).resolve()
    if work.exists():
        shutil.rmtree(work)
    home, out = work / "home", work / "out"
    out.mkdir(parents=True); home.mkdir(parents=True)
    d = nugget_qa.Daemon(BIN, home, out)
    d.write_config(None, None, "\n[mcp]\nhttp = false\n")
    d.start()
    rep = {}
    try:
        corpus = home / "corpus"
        for rel, text in FILES.items():
            f = corpus / rel
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(text)
        shutil.copytree(nugget_qa.CORPUS / "phase1/Calendar", corpus / "Calendar", dirs_exist_ok=True)
        nugget_qa.set_item_dates(corpus)
        d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(corpus)]}})
        d.settle("s4", 600, quiet_s=8)
        snap = d.snapshot("s4")
        seen, claims = set(), []
        for e in snap["details"].values():
            for c in e.get("claims", []):
                if c["id"] not in seen:
                    seen.add(c["id"])
                    claims.append((c["id"], c["status"], c["text"], [ev.get("quote") for ev in c["evidence"]]))
        rep["claims"] = sorted(claims)
        rep["launch_page"] = nugget_qa.find_page(d.vault, "Harbor launch day").read_text()
        op = nugget_qa.find_page(d.vault, "Team offsite")
        rep["offsite_page"] = op.read_text() if op else None
        uris = sorted({m for p in d.vault.glob("*/*.md") for m in re.findall(r"\((nugget://item/[^)]+)\)", p.read_text())})
        c = d.call("mcp.clients.create", {"name": "qa", "scopes": {"include_sensitive": True}})
        rep["resources_read"] = mcp_read(home, c["client"]["id"], c["token"], uris[:3])
    finally:
        d.stop()
    (out / "s4.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False))
    print(json.dumps(rep, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
