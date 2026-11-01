#!/usr/bin/env python3
"""Start the daemon's own chat llama-server outside the daemon, for qa/HH2/triage_prompts.py.

  serve_chat.py --harness DIR --nuggetd BIN --home DIR --chat-model ID --models-cache DIR --port N

Installs the chat model through the daemon (JJ1's realmodel.py helpers, models from the cache),
waits for the chat runtime, records the chat llama-server's command line and environment, stops
the daemon, then starts the same server on 127.0.0.1:PORT without the per-process API key and
waits for /health. Prints the server's pid. The models stay in HOME until the caller moves them.
"""
import argparse
import os
import subprocess
import sys
import time
import types
import urllib.request


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--harness", required=True)
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--chat-model", required=True)
    ap.add_argument("--models-cache")
    ap.add_argument("--port", type=int, default=18080)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()
    sys.path.insert(0, a.harness)
    import realmodel  # noqa: E402

    os.makedirs(a.log and os.path.dirname(a.log) or ".", exist_ok=True)
    ns = types.SimpleNamespace(home=os.path.abspath(a.home), nuggetd=a.nuggetd, out=os.path.dirname(a.log),
                               part="serve", chat_model=a.chat_model, embed_model="nomic-embed-text-v1.5-q8",
                               models_cache=a.models_cache)
    d = realmodel.prepare_home(ns)
    d.start()
    try:
        realmodel.install(d, a.chat_model)
        realmodel.install(d, ns.embed_model)
        d.restart()
        print("runtime", realmodel.wait_runtime(d), flush=True)
        argv = env = None
        for pid in os.listdir("/proc"):
            if not pid.isdigit():
                continue
            try:
                cmd = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
            except OSError:
                continue
            args = [c.decode() for c in cmd if c]
            if args and os.path.basename(args[0]).startswith("llama-server") \
                    and "--embedding" not in args and "--reranking" not in args:
                argv = args
                raw = open(f"/proc/{pid}/environ", "rb").read().split(b"\0")
                env = dict(e.decode().split("=", 1) for e in raw if b"=" in e)
                break
        if argv is None:
            raise SystemExit("chat llama-server not found")
    finally:
        d.stop()
    # Same server, plain TCP on PORT, no API key.
    out, skip = [], 0
    for i, x in enumerate(argv):
        if skip:
            skip -= 1
            continue
        if x in ("--host", "--port", "--api-key", "--api-key-file"):
            skip = 1
            continue
        out.append(x)
    out += ["--host", "127.0.0.1", "--port", str(a.port)]
    env = {k: v for k, v in env.items() if "API_KEY" not in k}
    print("serving:", " ".join(out), flush=True)
    p = subprocess.Popen(out, env=env, stdout=open(a.log, "w"), stderr=subprocess.STDOUT, start_new_session=True)
    t = time.time()
    while time.time() - t < 600:
        try:
            if urllib.request.urlopen(f"http://127.0.0.1:{a.port}/health", timeout=5).status == 200:
                print("pid", p.pid, flush=True)
                return
        except Exception:  # noqa: BLE001
            pass
        if p.poll() is not None:
            raise SystemExit(f"llama-server exited {p.returncode}; see {a.log}")
        time.sleep(2)
    raise SystemExit("llama-server not healthy")


if __name__ == "__main__":
    main()
