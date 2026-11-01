#!/usr/bin/env python3
"""II1 privacy: imported chats (private by default) never reach a cloud model.

A fake OpenAI-compatible server on 127.0.0.1 is configured as the only chat provider with
`locality = "cloud"`; it records every request body. The messaging fixture
(realmodel_fixture.py) is imported as whatsapp/telegram/imessage sources. Then, for each case:
  A. private on (the default): retrieve.ask and draft.begin about the chats. Expected: the cloud
     request contains no chat text and excluded_sensitive > 0.
  B. the same sources with `private: false` (sources.update). Expected: chat passages may be sent
     (the user opted in), except messages holding PII (a card number planted in the fixture? no: we
     only check that the opt-out takes effect).
Canaries are distinctive strings from the chats (booking reference, flight time, door code).

Usage: cloud_exclusion.py --home DIR [--nuggetd BIN] --out results.json
"""
import argparse
import http.server
import json
import os
import shutil
import subprocess
import sys
import threading

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "harness"))
from nq import Daemon, RpcError, dump  # noqa: E402

# Strings that appear in the chats but not in the questions asked below.
CANARIES = ["KX7Q2P", "18:40", "6:40", "Terminal 2", "TP 1350"]
BODIES = []


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"object":"list","data":[{"id":"cloud-x","object":"model"}]}')

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n).decode("utf-8", "replace")
        BODIES.append(body)
        try:
            stream = json.loads(body).get("stream")
        except ValueError:
            stream = False
        self.send_response(200)
        if stream:
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for t in ["I could not find that ", "in what I was given."]:
                self.wfile.write(("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": t}}]}) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.send_header("content-type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                                         "usage": {"prompt_tokens": 1, "completion_tokens": 1}}).encode())


def leaks(bodies):
    return sorted({c for b in bodies for c in CANARIES if c in b})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--home", required=True)
    ap.add_argument("--nuggetd")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_address[1]}/v1"
    shutil.rmtree(a.home, ignore_errors=True)
    fx = os.path.join(a.home, "fixture")
    subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "realmodel_fixture.py"), "--out", fx], check=True)
    os.makedirs(os.path.join(a.home, "config"), exist_ok=True)
    with open(os.path.join(a.home, "config", "config.toml"), "w") as f:
        f.write(f"""[runtime]\nenabled = false\n\n[mcp]\nhttp = false\n\n[knowledge]\nenabled = false\n\n[[providers]]\nid = "cloudx"\nkind = "openai_compatible"\nbase_url = "{url}"\nmodel = "cloud-x"\nlocality = "cloud"\ntiers = ["triage", "light", "synthesis"]\n""")
    res = {"cases": {}}
    q = "When does Sam's flight from Lisbon land, and what is the cabin booking reference?"
    dctx = {"context": {"app": {"name": "Signal", "id": "signal"}, "field": {"role": "text_field", "value": "", "secure": False},
                        "thread": [{"author": "Sam Rivera", "text": "Can you remind me of the cabin booking reference and when my flight lands?", "from_me": False}],
                        "recipients": ["Sam Rivera"], "subject": None}, "instruction": None, "provider": None, "max_tokens": None}
    with Daemon(a.home, nuggetd=a.nuggetd) as d:
        ids = []
        for s in json.load(open(os.path.join(fx, "sources.json"))):
            ids.append(d.call("sources.add", s)["id"])
        d.wait_idle(timeout=180)
        for case in ("A_private_default", "B_private_off"):
            if case == "B_private_off":
                # Without a local triage model nothing is "judged", and unjudged items count as
                # sensitive (HH2). Opt out of that too, so `private: false` alone decides.
                d.stop()
                with open(d.config_path(), "a") as f:
                    f.write("\n[ingest]\nassume_unflagged_safe = true\n")
                d.start()
                for sid in ids:
                    cur = next(x for x in d.call("sources.list") if x["id"] == sid)
                    d.call("sources.update", {"id": sid, "settings": {**cur["settings"], "private": False}})
                    d.call("sources.sync", {"id": sid})
                d.wait_idle(timeout=180)
            BODIES.clear()
            out = {}
            try:
                r = d.ask(q, timeout=120)
                out["ask"] = {k: r.get(k) for k in ("provider", "locality", "excluded_sensitive", "answer")}
                out["ask"]["citations"] = [(c["title"], c["sensitive"]) for c in r.get("citations") or []]
            except (RpcError, TimeoutError) as e:
                out["ask"] = {"error": str(e)}
            ask_bodies = list(BODIES)
            BODIES.clear()
            try:
                st = d.call("draft.begin", dctx, timeout=120)
                text, done, _ = d.c.stream(st["stream_id"], 120)
                out["draft"] = {"provider": st.get("provider"), "locality": st.get("locality"), "excluded_sensitive": st.get("excluded_sensitive"),
                                "citations": st.get("citations"), "text": text, "error": done.get("error")}
            except (RpcError, TimeoutError) as e:
                out["draft"] = {"error": str(e)}
            draft_bodies = list(BODIES)
            out["ask_requests"], out["ask_leaked_canaries"] = len(ask_bodies), leaks(ask_bodies)
            out["draft_requests"], out["draft_leaked_canaries"] = len(draft_bodies), leaks(draft_bodies)
            ctx = d.call("retrieve.context", {"query": q, "budget_tokens": 2000})
            out["context_sensitive_flags"] = [c["sensitive"] for c in ctx["citations"]]
            out["egress_recent"] = d.call("egress.recent", {"limit": 10})
            res["cases"][case] = out
            print(case, json.dumps({k: v for k, v in out.items() if k != "egress_recent"}, ensure_ascii=False)[:900])
    srv.shutdown()
    A = res["cases"]["A_private_default"]
    res["pass_private_default_no_leak"] = not A["ask_leaked_canaries"] and not A["draft_leaked_canaries"]
    dump(res, a.out)
    print("private default: no chat text sent to the cloud provider:", res["pass_private_default_no_leak"])


if __name__ == "__main__":
    main()
