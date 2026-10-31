#!/usr/bin/env python3
"""GG1 area E: check `inserted_field` (the field after insertion) and read-ahead outcomes.

Usage: join_check.py [--scenarios FILE] drafts.jsonl [...]

Scenario `expect` keys used here (ignored by check.py):
  inserted_required_regex / inserted_forbidden_regex: regexes on result.inserted_field
  read_ahead: expected read-ahead outcome ("done" or a skip reason such as "refused"),
              checked only on records that carry a read_ahead block (--read-ahead on).
Standard library only.
"""
import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("drafts", nargs="+")
    ap.add_argument("--scenarios", default=os.path.join(HERE, "..", "..", "scenarios", "E-insert.json"))
    a = ap.parse_args()
    with open(a.scenarios, encoding="utf-8") as fh:
        scs = {s["id"]: s for s in json.load(fh)["scenarios"]}
    fails = total = 0
    for path in a.drafts:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                if not line.strip():
                    continue
                rec = json.loads(line)
                sc = scs.get(rec["scenario"])
                if sc is None:
                    continue
                e = sc.get("expect") or {}
                r = rec["result"]
                probs = []
                ins = r.get("inserted_field")
                if ins is not None:
                    for pat in e.get("inserted_required_regex", []):
                        if not re.search(pat, ins):
                            probs.append(f"inserted lacks {pat!r}")
                    for pat in e.get("inserted_forbidden_regex", []):
                        m = re.search(pat, ins)
                        if m:
                            probs.append(f"inserted has {pat!r}: {m.group(0)!r}")
                ra = r.get("read_ahead")
                if ra is not None and "read_ahead" in e:
                    got = "done" if ra.get("outcome") == "done" else (ra.get("reason") or ra.get("outcome"))
                    if got != e["read_ahead"]:
                        probs.append(f"read_ahead {got} (want {e['read_ahead']})")
                total += 1
                if probs:
                    fails += 1
                    print(f"FAIL {rec.get('label')} {rec['scenario']}: " + "; ".join(probs))
                    if ins is not None:
                        print(f"     inserted_field={ins!r}")
    print(f"\n{total - fails}/{total} records pass the insertion checks")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
