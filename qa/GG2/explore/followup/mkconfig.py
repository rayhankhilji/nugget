#!/usr/bin/env python3
"""Write a config variant for the follow-up exploration daemon.

usage: mkconfig.py <home> <variant> <fake_port>
variants:
  heuristic  Ollama entry removed; fake server only, locality=cloud -> no local rewrite model,
             so rewrite_query uses heuristic_rewrite.
  model      Ollama entry removed; fake server local (127.0.0.1) on triage/light/synthesis ->
             rewrite_query asks the fake server.
  default    the default Ollama entry kept first (nothing listens on 11434), fake server appended
             for synthesis only -> what a default-config user with a second local server sees.
  default_rt Ollama entry first (down), fake server on triage/light/synthesis after it.
Common: [mcp] http=false, knowledge/vault off, runtime off (no models can run here).
"""
import re
import sys

home, variant, port = sys.argv[1], sys.argv[2], sys.argv[3]
path = f"{home}/config/config.toml"
s = open(path + ".orig" if __import__("os").path.exists(path + ".orig") else path).read()
open(path + ".orig", "w").write(s)
s = s.replace("http = true", "http = false", 1)
s = s.replace("[knowledge]\nenabled = true", "[knowledge]\nenabled = false", 1)
s = s.replace("[vault]\nenabled = true", "[vault]\nenabled = false", 1)
s = s.replace("[runtime]\nenabled = true", "[runtime]\nenabled = false", 1)
fake = f'''
[[providers]]
id = "fake"
kind = "openai_compatible"
base_url = "http://127.0.0.1:{port}/v1"
model = "fake"
max_retries = 0
'''
if variant == "heuristic":
    s = re.sub(r'\[\[providers\]\]\nid = "local"\n(.*\n){4}', "", s, count=1)
    s += fake + 'tiers = ["synthesis"]\nlocality = "cloud"\n'
elif variant == "model":
    s = re.sub(r'\[\[providers\]\]\nid = "local"\n(.*\n){4}', "", s, count=1)
    s += fake + 'tiers = ["triage", "light", "synthesis"]\n'
elif variant == "default":
    s += fake + 'tiers = ["synthesis"]\n'
elif variant == "default_rt":
    # Default Ollama entry first, then a local server on every chat tier: the routing order of
    # a default-config user whose models run in the on-device runtime (local-chat comes after
    # configured non-cloud providers, crates/nuggetd/src/local.rs "Routing order").
    s += fake + 'tiers = ["triage", "light", "synthesis"]\n'
else:
    raise SystemExit("unknown variant")
open(path, "w").write(s)
print(s[s.find("[[providers]]"):])
