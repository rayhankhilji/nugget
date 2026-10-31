"""The real desktop app (Tauri, WebKitGTK) inside the lab namespace (sinkhole), against the lab's
daemon, for 150 s each: (a) default settings, (b) Updates channel `off`. The build carries a
throwaway updater key (as release builds carry the real one), so the updater is active.
What does the app or its webview send, and is it in the ledger? Docs: privacy.md says nothing
beyond connector syncs and model downloads leaves with the default configuration; release.md
says checks run 60 s after launch on the default Stable channel and are not in the ledger."""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import common  # noqa: E402

NAME = "s65_desktop"
MODE = "sinkhole"
CONFIG = """
[runtime]
enabled = false
[knowledge]
enabled = false
"""
REPO = os.path.dirname(os.path.dirname(common.QA))
APP = os.environ.get("GG8_DESKTOP_APP", os.path.join(REPO, "target/debug/nugget-desktop"))
SECS = os.environ.get("GG8_DESKTOP_SECS", "150")
SCRIPT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "desktop-in-ns.sh")


def run(lab):
    for tag, settings in (("default", None), ("channel-off", {"channel": "off"})):
        cfg = os.path.join(lab.home, f"appcfg-{tag}")
        os.makedirs(os.path.join(cfg, "dev.nugget.desktop"), exist_ok=True)
        if settings:
            with open(os.path.join(cfg, "dev.nugget.desktop", "settings.json"), "w") as f:
                json.dump(settings, f)
        t = time.time()
        lab.mark(f"desktop {tag}")
        lab.ns_run([SCRIPT, APP, lab.home, cfg, os.path.join(lab.out, f"desktop-{tag}"), SECS],
                   timeout=int(SECS) + 120)
        reqs = [r for r in lab.requests(since=t) if "method" in r or r.get("tls_handshake_error")]
        dns = [d.get("qname") for d in lab.dns() if d["t"] >= t]
        summary = [(r.get("host") or r.get("sni"), r.get("method"), r.get("path"),
                    [h for h in r.get("headers", []) if h[0].lower() in ("user-agent", "accept")])
                   for r in reqs]
        with open(os.path.join(lab.out, f"desktop-{tag}-traffic.json"), "w") as f:
            json.dump({"dns": dns, "requests": reqs}, f, indent=1)
        if tag == "default":
            lab.check("desktop.default.traffic", True, {"dns": dns, "requests": summary})
            lab.check("desktop.default.no_egress_beyond_documented", not reqs and not dns,
                      {"dns": dns, "requests": summary}, severity="Medium")
        else:
            lab.check("desktop.channel_off.silent", not reqs and not dns,
                      {"dns": dns, "requests": summary}, severity="High")
    rows = lab.ledger()
    lab.check("desktop.ledger", True, rows[:10])
