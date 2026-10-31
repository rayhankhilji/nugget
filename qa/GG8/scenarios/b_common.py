"""Shared helpers for the GG8 area-B scenarios (what is sent to a cloud provider, per feature)."""
import glob
import json
import os
import re
import shutil
import time

FIX = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "fixtures")
CLOUD_HOSTS = ("api.anthropic.com", "api.openai.com", "api.cloudllm.test")
# Strings from the sensitive fixtures that must never reach a cloud model.
SECRETS = ["GG8CANARY-SENS", "4111 1111 1111 1111", "4111111111111111", "512-34-6789",
           "AKIAIOSFODNN7GG8TEST", "sk-ant-api03-GG8fake", "GB82 WEST", "wJalrXUtnFEMI"]


def canary_files(corpus_dir):
    """canary -> file name, from the fixture corpus."""
    out = {}
    for p in glob.glob(os.path.join(corpus_dir, "**", "*"), recursive=True):
        if os.path.isfile(p):
            for c in re.findall(r"GG8CANARY-[A-Z]+-\d+", open(p, errors="replace").read()):
                out[c] = os.path.relpath(p, corpus_dir)
    return out


def add_corpus(lab, src="corpus", name="Documents", wait=True, dest=None):
    dest = dest or f"{lab.home}/{name}"
    shutil.copytree(os.path.join(FIX, src), dest, dirs_exist_ok=True)
    info = lab.call("sources.add", {"kind": "files", "name": name, "settings": {"roots": [dest]}})
    if wait:
        lab.wait_idle(seconds=5, timeout=180)
    return info, dest


def cloud_reqs(lab, since, hosts=CLOUD_HOSTS):
    return [r for r in lab.requests(since=since) if r.get("host") in hosts and "method" in r]


def body(reqs):
    return "\n".join(r.get("body", "") for r in reqs)


def no_secrets(lab, label, reqs, severity="High"):
    b = body(reqs)
    leaked = [s for s in SECRETS if s in b]
    return lab.check(f"{label}.no_sensitive_content", not leaked,
                     f"{len(reqs)} requests; leaked: {leaked}", severity=None if not leaked else severity)


def keys_only_to_own_host(lab, label, keymap):
    """keymap: canary key string -> the only host allowed to receive it."""
    bad = []
    for r in lab.requests():
        if "method" not in r:
            continue
        blob = json.dumps(r.get("headers")) + (r.get("body") or "") + r.get("path", "")
        for k, host in keymap.items():
            if k in blob and r.get("host") != host:
                bad.append((k, r.get("host"), r.get("path")))
    return lab.check(f"{label}.keys_only_to_own_host", not bad, bad)


def ledger_covers(lab, label, since, feature=None, corpus_dir=None, hosts=CLOUD_HOSTS):
    """Every cloud request since `since` has a ledger row (host, feature) and the row's items
    include every fixture whose canary is in that request's body."""
    time.sleep(1.5)
    reqs = cloud_reqs(lab, since, hosts)
    rows = lab.ledger()
    since_iso = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(since - 1))
    rows = [r for r in rows if r["at"] >= since_iso]
    cmap = canary_files(corpus_dir) if corpus_dir else {}
    # match each request to a row with same host, nearest bytes
    unmatched = []
    used = set()
    item_problems = []
    for q in reqs:
        cands = [(i, r) for i, r in enumerate(rows) if r["host"] == q["host"] and i not in used
                 and (feature is None or r["feature"] == feature)]
        if not cands:
            unmatched.append((q["host"], q["path"], q.get("bytes_in")))
            continue
        i, row = min(cands, key=lambda ir: abs(ir[1].get("bytes_sent", 0) - q.get("bytes_in", 0)))
        used.add(i)
        keys = {it["key"] for it in row.get("items", [])}
        for c in set(re.findall(r"GG8CANARY-[A-Z]+-\d+", q.get("body", ""))):
            f = cmap.get(c)
            if f and not any(k.endswith(f) for k in keys):
                item_problems.append({"canary": c, "file": f, "row_items": sorted(keys),
                                      "feature": row["feature"]})
    lab.check(f"{label}.ledger_rows", not unmatched,
              f"{len(reqs)} requests, {len(rows)} rows (features {sorted({r['feature'] for r in rows})}); unmatched {unmatched}")
    if corpus_dir:
        lab.check(f"{label}.ledger_items_accurate", not item_problems, item_problems[:5])
    return rows, reqs


def save_requests(lab, label, reqs):
    with open(os.path.join(lab.out, f"bodies-{label}.json"), "w") as f:
        json.dump([{"host": r["host"], "path": r["path"], "headers": r["headers"],
                    "body": r.get("body")} for r in reqs], f, indent=1)
