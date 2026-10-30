"""Default local-only install: a folder, MCP over loopback HTTP, then 5 minutes idle and a restart.
Promise: no DNS query and no packet leaves the device; the MCP listener is reachable only on
loopback; MCP hides sensitive items by default; MCP calls make no ledger rows."""
import json
import shutil
import socket
import subprocess
import time

import lab as L

NAME = "s10_local_idle_mcp"
CONFIG = ""
IDLE = 300


def mcp_http(lab, token, body):
    r = lab.ns_run(["curl", "-s", "-m", "20", "-X", "POST", "http://127.0.0.1:7319/mcp",
                    "-H", f"Authorization: Bearer {token}", "-H", "Content-Type: application/json",
                    "-H", "Accept: application/json, text/event-stream", "-d", json.dumps(body)],
                   capture_output=True, text=True)
    return r.stdout


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    created = lab.call("mcp.clients.create", {"name": "gg8-http", "scopes": {}})
    token = created["token"]
    init = mcp_http(lab, token, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "gg8", "version": "0"}}})
    lab.check("mcp.http.initialize", "result" in init, init[:200])
    out = mcp_http(lab, token, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "search", "arguments": {"query": "Acme renewal", "limit": 20}}})
    lab.check("mcp.search.returns", "GG8CANARY-NORMAL-1" in out or "acme-renewal" in out, out[:300])
    sens = [c for c in ("SENS-1", "SENS-2", "SENS-3", "SENS-4") if f"GG8CANARY-{c}" in out]
    lab.check("mcp.search.hides_sensitive", not sens, sens)
    out2 = mcp_http(lab, token, {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "get_context", "arguments": {"query": "Acme renewal card payment", "budget_tokens": 4000}}})
    sens2 = [c for c in ("SENS-1", "SENS-2", "SENS-3", "SENS-4") if f"GG8CANARY-{c}" in out2]
    lab.check("mcp.get_context.hides_sensitive", not sens2, sens2)
    # Exposure: listening sockets inside the namespace, and reachability from the host side.
    ss = lab.ns_run(["ss", "-H", "-ltnu"], capture_output=True, text=True).stdout
    lab.note("listening: " + ss)
    exposed = [ln for ln in ss.splitlines() if not any(a in ln for a in ("127.0.0.1:", "[::1]:", "127.0.0.53"))]
    lab.check("listeners.loopback_only", not exposed, exposed)
    ns_ip = L.SINK_IP.rsplit(".", 1)[0] + "." + str(int(L.SINK_IP.rsplit(".", 1)[1]) - 8)
    reach = []
    for ip in (ns_ip, L.LAN_IP.rsplit(".", 1)[0] + ".2"):
        s = socket.socket()
        s.settimeout(2)
        try:
            s.connect((ip, 7319))
            reach.append(ip)
        except OSError:
            pass
        finally:
            s.close()
    lab.check("mcp.not_reachable_off_device", not reach, reach)
    t_idle = time.time()
    lab.mark("idle")
    time.sleep(IDLE)
    lab.restart_daemon()
    time.sleep(60)
    lab.check("ledger.empty", lab.ledger() == [], lab.ledger()[:3])
    lab.check("no_dns", not lab.dns(), sorted({d.get("qname") for d in lab.dns()}))
    lab.check("no_requests", not lab.requests(), [(r.get("host"), r.get("path")) for r in lab.requests()][:10])
    flows = lab.flows()
    lab.check("no_packets_off_namespace", not flows, flows[:10])
