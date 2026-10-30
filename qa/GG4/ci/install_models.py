#!/usr/bin/env python3
"""Install the chat and embedding models through the daemon (real clock), into --models-home,
so the simulated-clock runs share them."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import Daemon, Home  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--home", required=True)
ap.add_argument("--models-home", required=True)
ap.add_argument("--chat", required=True)
ap.add_argument("--embed", required=True)
a = ap.parse_args()
h = Home(a.home, runtime=True, chat_model=a.chat, embed_model=a.embed, models_home=a.models_home)
with Daemon(h, sim=None) as d:
    print(json.dumps([{k: m[k] for k in ("id", "installed")} for m in d.call("models.list", {})]))
    d.install_models([a.chat, a.embed])
    print("runtime:", json.dumps(d.wait_runtime_ready(1800)))
    r = d.call("providers.list", {})
    print("providers:", json.dumps(r))
