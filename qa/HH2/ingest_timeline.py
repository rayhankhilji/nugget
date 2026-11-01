#!/usr/bin/env python3
"""HH2: how long a new folder takes to become searchable, and what Ask feels like meanwhile.

Drives a real nuggetd over IPC with the GG2 harness (qa/GG2/harness/ask_qa.py from the QA branch,
imported at run time, not copied) and the config GG2 used for its 4B run (default config, chosen
manifest models, idle_stop_minutes = 0, knowledge off). From the moment the corpus is added as a
`files` source it records, every --poll seconds:

  timeline       items / chunks / queue / state of the source, and `ingest.status.triage`
                 (pending, done, failed, skipped, state) when the daemon reports it.
  first search   per probe (exact-word `search` queries from search-probes.txt whose target files
                 are derived from the corpus text): the first time its target is in the top 10.
  t_indexed      all corpus items stored (the source's `items`).
  t_triaged      fully triaged: all items stored AND (no `triage` field OR triage.pending == 0 and
                 triage.state != "running"). Capped by --triage-timeout; a timeout is reported.
  asks           --ask-delay seconds after the source is added, the --ask-ids questions are asked
                 one by one through ask_qa.ask (retrieve.ask like the desktop Chat); after
                 t_triaged the same questions again (idle baseline).
  recall         after settling, every probe in search-probes.txt: found or not, and rank.
  zero chunks    IPC has no per-item chunk count; `items - chunks` of the source is a lower bound
                 on items without chunks (exact when every item is one chunk, as in this corpus).

Writes result.json and summary.md into --out. Stdlib only.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib.util
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
GG2_REF = "1f2b7867722291eae20d46884c02b86ba24c73f0"
GG2_FILES = {
    "ask_qa.py": "qa/GG2/harness/ask_qa.py",
    "gen_corpus.py": "qa/GG2/harness/gen_corpus.py",
    "score.py": "qa/GG2/harness/score.py",
    "search-probes.txt": "qa/GG2/repro/search-probes.txt",
}
# Ordinary items (kept by triage in every GG2 run) plus the two the 4B made unsearchable.
DEFAULT_FIRST_PROBES = ["boiler engineer WarmFlow", "Corvid pre-boarding call Lena paperwork",
                        "Kestrel Mobile monthly statement", "City Power electricity bill",
                        "Northgate Auto car service"]
DEFAULT_ASK_IDS = "L01,L05,L12,L15,L19,L25"

T0 = 0.0  # monotonic time of sources.add


def log(*a):
    print(time.strftime("%H:%M:%S"), f"[{rel():7.1f}s]" if T0 else "", *a, flush=True)


def rel(t=None):
    return round((time.monotonic() if t is None else t) - T0, 1)


# ---- GG2 harness ---------------------------------------------------------------------------------

def ensure_gg2(d, ref):
    """Extract the GG2 harness from the QA commit into `d` (unless already there)."""
    os.makedirs(d, exist_ok=True)
    missing = [n for n in GG2_FILES if not os.path.exists(os.path.join(d, n))]
    if not missing:
        return
    git = ["git", "-C", REPO]
    if subprocess.run(git + ["cat-file", "-e", f"{ref}^{{commit}}"], capture_output=True).returncode != 0:
        subprocess.run(git + ["fetch", "--no-tags", "origin", "qa/ask"], check=True)
    for n in missing:
        data = subprocess.run(git + ["show", f"{ref}:{GG2_FILES[n]}"], capture_output=True, check=True).stdout
        with open(os.path.join(d, n), "wb") as f:
            f.write(data)


def load_ask_qa(d):
    spec = importlib.util.spec_from_file_location("gg2_ask_qa", os.path.join(d, "ask_qa.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---- corpus, probes, questions -------------------------------------------------------------------

def corpus_files(root):
    out = {}
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            with open(p, encoding="utf-8", errors="replace") as fh:
                out[os.path.relpath(p, root).replace(os.sep, "/")] = fh.read().lower()
    return out


def probe_targets(probes, files):
    """A probe's targets: the corpus files containing every word of it."""
    out = {}
    for q in probes:
        words = re.findall(r"[a-z0-9]+", q.lower())
        out[q] = sorted(k for k, v in files.items() if all(re.search(r"\b" + re.escape(w), v) for w in words))
    return out


def item_path(key):
    # Files-source keys are "<root index>:<path relative to the root>".
    return key.split(":", 1)[1] if re.match(r"^\d+:", key) else key


def rank_of(hits, targets):
    for i, h in enumerate(hits):
        if item_path(h["item"]["key"]) in targets:
            return i + 1, item_path(h["item"]["key"])
    return None, None


def load_questions(path, ids):
    convs = [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]
    meta = convs.pop(0).get("meta", {})
    by_id = {c["id"]: c for c in convs}
    qs = []
    for i in ids:
        if i not in by_id:
            raise SystemExit(f"question {i} not in {path}")
        t = by_id[i]["turns"][0]
        qs.append({"id": i, "q": t["q"], "support": t["support"], "must": t["must"]})
    return meta, qs


# ---- models cache --------------------------------------------------------------------------------

def move_children(src, dst):
    """Move every entry of `src` into `dst` (same filesystem: renames). Existing entries in `dst`
    are replaced, so the newest copy wins."""
    if not os.path.isdir(src):
        return []
    os.makedirs(dst, exist_ok=True)
    moved = []
    for n in os.listdir(src):
        s, d = os.path.join(src, n), os.path.join(dst, n)
        if os.path.lexists(d):
            shutil.rmtree(d) if os.path.isdir(d) and not os.path.islink(d) else os.remove(d)
        shutil.move(s, d)
        moved.append(n)
    return moved


# ---- measurement ---------------------------------------------------------------------------------

class Run:
    def __init__(self, a, aq, expected, source_id, probes_first, targets):
        self.a, self.aq = a, aq
        self.expected = expected
        self.source_id = source_id
        self.timeline = []
        self.last = None
        self.t_indexed = None
        self.t_half_indexed = None
        self.t_triaged = None
        self.t_queue_empty = None
        self.triaged = asyncio.Event()  # set when fully triaged, or when the timeout gives up
        self.timed_out = False
        self.stop = asyncio.Event()
        self.first_found = {q: None for q in probes_first}
        self.targets = targets
        self.search_ms = []
        self.search_errors = 0

    def src(self, st):
        for s in st.get("sources") or []:
            if s.get("id") == self.source_id:
                return s
        return (st.get("sources") or [{}])[0]

    def is_triaged(self, row):
        if (row["items"] or 0) < self.expected:
            return False
        tr = row.get("triage")
        if tr is None:
            return True
        if self.a.no_models and tr.get("state") == "off":
            return True  # no model: triage cannot run, nothing will change
        return tr.get("pending") == 0 and tr.get("state") != "running"

    async def poller(self, rpc):
        while not self.stop.is_set():
            t_call = time.monotonic()
            try:
                st, _, at = await rpc.call("ingest.status", timeout=60)
            except Exception as e:  # keep polling; a slow status call is itself data
                self.timeline.append({"t": rel(t_call), "error": str(e) or type(e).__name__})
                await asyncio.sleep(self.a.poll)
                continue
            s = self.src(st)
            row = {"t": rel(at), "status_ms": round((at - t_call) * 1000), "items": s.get("items"),
                   "chunks": s.get("chunks"), "queue": st.get("queue_depth"), "state": s.get("state")}
            if "triage" in st and st["triage"] is not None:
                row["triage"] = st["triage"]
            key = {k: v for k, v in row.items() if k not in ("t", "status_ms")}
            if key != self.last:
                tr = row.get("triage") or {}
                log(f"items={row['items']} chunks={row['chunks']} queue={row['queue']} state={row['state']}"
                    + (f" triage={tr.get('state')} pending={tr.get('pending')} done={tr.get('done')}"
                       f" failed={tr.get('failed')} skipped={tr.get('skipped')}" if tr else ""))
            self.last = key
            self.timeline.append(row)
            if self.t_half_indexed is None and (row["items"] or 0) >= self.expected / 2:
                self.t_half_indexed = row["t"]
            if self.t_indexed is None and (row["items"] or 0) >= self.expected:
                self.t_indexed = row["t"]
                log("all items indexed")
            if self.t_queue_empty is None and row["queue"] == 0 and (row["items"] or 0) >= self.expected:
                self.t_queue_empty = row["t"]
            if not self.triaged.is_set() and self.is_triaged(row):
                self.t_triaged = row["t"]
                self.triaged.set()
                log("fully triaged")
            await asyncio.sleep(max(0.0, self.a.poll - (time.monotonic() - t_call)))

    async def prober(self, rpc):
        while not self.stop.is_set() and any(v is None for v in self.first_found.values()):
            t_round = time.monotonic()
            for q, v in self.first_found.items():
                if v is not None:
                    continue
                t_call = time.monotonic()
                try:
                    hits, _, at = await rpc.call("search", {"query": q, "limit": 10}, timeout=120)
                except Exception:
                    self.search_errors += 1
                    continue
                self.search_ms.append(round((at - t_call) * 1000))
                r, key = rank_of(hits, self.targets[q])
                if r is not None:
                    self.first_found[q] = {"t": rel(at), "rank": r, "key": key}
                    log(f"first found {q!r} -> {key} (rank {r})")
            if self.triaged.is_set():
                break  # one last round after triage; never-found probes stay None
            await asyncio.sleep(max(0.0, self.a.poll - (time.monotonic() - t_round)))

    def snapshot(self):
        r = self.timeline[-1] if self.timeline else {}
        return {k: r.get(k) for k in ("t", "items", "chunks", "queue", "triage")}

    async def ask_all(self, rpc, qs, phase):
        out = []
        for q in qs:
            before = self.snapshot()
            t_start = rel()
            rec = await self.aq.ask(rpc, q["q"], [], self.a.ask_timeout)
            cited = [item_path(c["item"]["key"]) for c in rec.get("citations") or []]
            ans = rec.get("answer") or ""
            out.append({"id": q["id"], "phase": phase, "t_start": t_start, "query": q["q"],
                        "first_token_ms": rec.get("first_token_ms"), "total_ms": rec.get("total_ms"),
                        "started_ms": rec.get("started_ms"), "error": rec.get("error") or rec.get("done_error"),
                        "support_cited": any(c in q["support"] for c in cited), "cited": cited,
                        "must_ok": bool(ans) and all(any(re.search(r, ans, re.I) for r in alts) for alts in q["must"]),
                        "answer": ans[:400], "during": before, "after": self.snapshot()})
            log(f"ask[{phase}] {q['id']} ttft={rec.get('first_token_ms')} total={rec.get('total_ms')} "
                f"cited_support={out[-1]['support_cited']} err={out[-1]['error']}")
        return out

    async def asker(self, rpc, qs):
        await asyncio.sleep(max(0.0, self.a.ask_delay - rel()))
        during = await self.ask_all(rpc, qs, "during_sync")
        await self.triaged.wait()
        idle = await self.ask_all(rpc, qs, "idle")
        return during, idle, self.timed_out


def pct(xs, p):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    return xs[min(len(xs) - 1, max(0, round(p / 100 * (len(xs) - 1))))]


def ask_stats(recs):
    ttft = [r["first_token_ms"] for r in recs]
    tot = [r["total_ms"] for r in recs]
    ok = [x for x in ttft if x is not None]
    return {"n": len(recs), "answered": len(ok), "errors": sum(1 for r in recs if r["error"]),
            "ttft_p50_ms": statistics.median(ok) if ok else None, "ttft_max_ms": max(ok) if ok else None,
            "total_p50_ms": statistics.median([x for x in tot if x is not None]) if any(x is not None for x in tot) else None,
            "support_cited": sum(1 for r in recs if r["support_cited"]),
            "must_ok": sum(1 for r in recs if r["must_ok"])}


def hardware():
    try:
        out = subprocess.run(["lscpu"], capture_output=True, text=True, timeout=10).stdout
    except Exception:
        return {}
    h = {}
    for line in out.splitlines():
        k, _, v = line.partition(":")
        if k.strip() in ("Model name", "CPU(s)", "Thread(s) per core", "Core(s) per socket"):
            h[k.strip()] = v.strip()
    return h


async def run(a):
    global T0
    os.makedirs(a.out, exist_ok=True)
    gg2 = os.path.abspath(a.gg2_dir or os.path.join(a.out, "gg2"))
    ensure_gg2(gg2, a.gg2_ref)
    aq = load_ask_qa(gg2)
    aq.NUGGETD = a.nuggetd
    probes_file = a.probes or os.path.join(gg2, "search-probes.txt")
    probes = [l.strip() for l in open(probes_file, encoding="utf-8") if l.strip() and not l.startswith("#")]
    files = corpus_files(a.corpus)
    expected = len(files)
    targets = probe_targets(probes, files)
    first_probes = [p.strip() for p in a.first_probes.split(",") if p.strip()] if a.first_probes else DEFAULT_FIRST_PROBES
    for p in first_probes:
        if p not in targets:
            targets.update(probe_targets([p], files))
    for q, t in targets.items():
        if not t:
            raise SystemExit(f"probe {q!r} matches no corpus file")
    meta, qs = load_questions(a.questions, [i.strip() for i in a.ask_ids.split(",") if i.strip()])

    home = os.path.abspath(a.home)
    if os.path.exists(os.path.join(home, "data", "nugget.db")):
        raise SystemExit(f"{home} is not a fresh home (data/nugget.db exists)")
    logf = open(os.path.join(a.out, "nuggetd.log"), "ab")
    cfg = os.path.join(home, "config", "config.toml")
    if not os.path.exists(cfg):
        proc = aq.start_daemon(a.nuggetd, home, logf)
        try:
            await aq.wait_sock(home, proc)
        finally:
            aq.stop_daemon(proc)
    # The GG2 4B run's config: default config, models chosen, idle stop off, knowledge off.
    text = aq.edit_config(cfg, a.chat_model, a.embed_model, False, False)
    problems = [s for s in (f'chat_model = "{a.chat_model}"', f'embedding_model = "{a.embed_model}"',
                            "idle_stop_minutes = 0", "[knowledge]\nenabled = false") if s not in text]
    if problems:
        log("WARNING: config template changed; not applied:", problems)
    with open(os.path.join(a.out, "config.toml"), "w") as f:
        f.write(text)

    models_dir = os.path.join(home, "data", "models")
    if a.models_cache:
        moved = move_children(a.models_cache, models_dir)
        log(f"models cache: moved {moved} into {models_dir}")
    result = {"label": a.label, "nuggetd": a.nuggetd, "chat_model": None if a.no_models else a.chat_model,
              "embed_model": None if a.no_models else a.embed_model, "no_models": a.no_models,
              "corpus_items": expected, "corpus_today": meta.get("today"), "poll_s": a.poll,
              "ask_delay_s": a.ask_delay, "triage_timeout_s": a.triage_timeout, "hardware": hardware(),
              "config_problems": problems}
    proc = aq.start_daemon(a.nuggetd, home, logf)
    try:
        rpc = await aq.wait_sock(home, proc)
        st, _, _ = await rpc.call("status", timeout=30)
        result["daemon_version"] = st.get("version")
        t = time.time()
        if not a.no_models:
            await aq.install(rpc, a.embed_model)
            await aq.install(rpc, a.chat_model)
        result["install_s"] = round(time.time() - t)
        sock = aq.sock_path(home)
        rpc_poll, rpc_search, rpc_ask = [await aq.Rpc.connect(sock) for _ in range(3)]

        T0 = time.monotonic()
        info, _, at = await rpc.call("sources.add", {"kind": "files", "name": "Dana's files",
                                                     "settings": {"roots": [os.path.abspath(a.corpus)]}}, timeout=120)
        result["sources_add_ms"] = round((at - T0) * 1000)
        log("source added:", info.get("id"))
        r = Run(a, aq, expected, info.get("id"), first_probes, targets)
        tasks = [asyncio.create_task(r.poller(rpc_poll)), asyncio.create_task(r.prober(rpc_search))]
        ask_task = asyncio.create_task(r.asker(rpc_ask, qs))
        try:
            await asyncio.wait_for(r.triaged.wait(), max(1.0, a.triage_timeout - rel()))
        except asyncio.TimeoutError:
            log(f"NOT fully triaged after {a.triage_timeout:.0f}s")
            # Give up waiting: the asker's idle phase and the prober go on, flagged as not idle.
            r.timed_out = True
            r.triaged.set()
        await tasks[1]
        during, idle, idle_after_timeout = await ask_task
        # Recall once settled: every probe, rank in the top 10.
        recall = {}
        for q in probes:
            hits, _, _ = await rpc_search.call("search", {"query": q, "limit": 10}, timeout=120)
            rk, key = rank_of(hits, targets[q])
            recall[q] = {"targets": targets[q], "found": rk is not None, "rank": rk, "key": key,
                         "top": [item_path(h["item"]["key"]) for h in hits[:5]]}
            log(f"recall {q!r}: rank={rk} top={recall[q]['top'][:3]}")
        r.stop.set()
        await tasks[0]
        final, _, _ = await rpc.call("ingest.status", timeout=60)
        status, _, _ = await rpc.call("status", timeout=30)
        try:
            result["runtime"], _, _ = await rpc.call("runtime.status", timeout=30)
        except Exception as e:
            result["runtime"] = str(e)
        s = r.src(final)
        timed_out = r.t_triaged is None
        result.update({
            "t_first_search_s": min((v["t"] for v in r.first_found.values() if v), default=None),
            "first_search": r.first_found,
            "t_half_indexed_s": r.t_half_indexed,
            "t_indexed_s": r.t_indexed,
            "t_queue_empty_s": r.t_queue_empty,
            "t_triaged_s": r.t_triaged,
            "triage_timed_out": timed_out,
            "final": {"items": s.get("items"), "chunks": s.get("chunks"), "queue": final.get("queue_depth"),
                      "state": s.get("state"), "triage": final.get("triage"),
                      "triage_provider": final.get("triage_provider"),
                      "embedding_provider": final.get("embedding_provider"),
                      "freshness_p50_ms": final.get("freshness_p50_ms"),
                      "freshness_p95_ms": final.get("freshness_p95_ms"),
                      "status_items": status.get("items"), "status_chunks": status.get("chunks"),
                      "zero_chunk_items_lower_bound": max(0, (s.get("items") or 0) - (s.get("chunks") or 0))},
            "search_during_sync": {"calls": len(r.search_ms), "errors": r.search_errors,
                                   "p50_ms": pct(r.search_ms, 50), "p95_ms": pct(r.search_ms, 95)},
            "asks": {"during_sync": ask_stats(during), "idle": ask_stats(idle),
                     "idle_after_timeout": idle_after_timeout, "records": during + idle},
            "recall": recall,
            "recall_found": sum(1 for v in recall.values() if v["found"]),
            "timeline": r.timeline,
        })
        result["ingest_status_final"] = final
    finally:
        aq.stop_daemon(proc)
        if a.models_cache and os.path.isdir(models_dir):
            log("models cache: moved back", move_children(models_dir, a.models_cache))
    with open(os.path.join(a.out, "result.json"), "w") as f:
        json.dump(result, f, indent=1)
    with open(os.path.join(a.out, "summary.md"), "w") as f:
        f.write(summary_md(result))
    log("wrote", os.path.join(a.out, "result.json"))


def fmt_s(v):
    return "never" if v is None else f"{v:.0f} s" if v >= 10 else f"{v:.1f} s"


def fmt_ms(v):
    return "–" if v is None else f"{v:.0f} ms" if v < 1000 else f"{v / 1000:.1f} s"


def summary_md(r):
    ad, ai = r["asks"]["during_sync"], r["asks"]["idle"]
    f = r["final"]
    tr = f.get("triage")
    lines = [f"# HH2 ingest timeline: {r['label']}", "",
             f"nuggetd `{r.get('daemon_version')}`, chat model `{r['chat_model']}`, {r['corpus_items']} items, "
             f"CPU {r['hardware'].get('Model name', '?')} ({r['hardware'].get('CPU(s)', '?')} CPUs).", "",
             "| Metric | Value |", "|---|---|",
             f"| First probe found by `search` | {fmt_s(r['t_first_search_s'])} |",
             f"| Half the items stored | {fmt_s(r['t_half_indexed_s'])} |",
             f"| All items stored (t_indexed) | {fmt_s(r['t_indexed_s'])} |",
             f"| Fully triaged (t_triaged) | {fmt_s(r['t_triaged_s'])}"
             + (f" (timed out after {r['triage_timeout_s']:.0f} s)" if r["triage_timed_out"] else "") + " |",
             f"| Ask during sync: TTFT p50 / max | {fmt_ms(ad['ttft_p50_ms'])} / {fmt_ms(ad['ttft_max_ms'])} ({ad['answered']}/{ad['n']} answered) |",
             f"| Ask idle: TTFT p50 / max | {fmt_ms(ai['ttft_p50_ms'])} / {fmt_ms(ai['ttft_max_ms'])} ({ai['answered']}/{ai['n']} answered)"
             + (" (not idle: triage timed out)" if r["asks"]["idle_after_timeout"] else "") + " |",
             f"| Ask total p50 during / idle | {fmt_ms(ad['total_p50_ms'])} / {fmt_ms(ai['total_p50_ms'])} |",
             f"| `search` latency during sync p50 / p95 | {fmt_ms(r['search_during_sync']['p50_ms'])} / {fmt_ms(r['search_during_sync']['p95_ms'])} |",
             f"| Probes found after settling | {r['recall_found']}/{len(r['recall'])} |",
             f"| Items / chunks (source) | {f['items']} / {f['chunks']} |",
             f"| Items without chunks (lower bound) | {f['zero_chunk_items_lower_bound']} |",
             f"| Triage counters | {json.dumps(tr) if tr else 'absent (old build)'} |",
             "", "## First found by `search` (seconds after the source was added)", "",
             "| Probe | First found | Rank | Item |", "|---|---|---|---|"]
    for q, v in r["first_search"].items():
        lines.append(f"| {q} | {fmt_s(v['t'] if v else None)} | {v['rank'] if v else '–'} | {v['key'] if v else '–'} |")
    lines += ["", "## Recall after settling", "", "| Probe | Found | Rank | Targets |", "|---|---|---|---|"]
    for q, v in r["recall"].items():
        lines.append(f"| {q} | {'yes' if v['found'] else 'no'} | {v['rank'] or '–'} | {len(v['targets'])} |")
    lines += ["", "## Asks", "", "| Phase | Id | Start | TTFT | Total | Support cited | Gold | Items stored at start | Error |",
              "|---|---|---|---|---|---|---|---|---|"]
    for x in r["asks"]["records"]:
        lines.append(f"| {x['phase']} | {x['id']} | {fmt_s(x['t_start'])} | {fmt_ms(x['first_token_ms'])} | "
                     f"{fmt_ms(x['total_ms'])} | {'yes' if x['support_cited'] else 'no'} | {'yes' if x['must_ok'] else 'no'} | "
                     f"{(x['during'] or {}).get('items')} | {x['error'] or ''} |")
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--home", required=True, help="a fresh, empty Nugget home")
    ap.add_argument("--corpus", required=True, help="GG2 corpus (gen_corpus.py --out)")
    ap.add_argument("--questions", required=True, help="GG2 questions.jsonl (gen_corpus.py --questions)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", default="run")
    ap.add_argument("--chat-model", default="qwen3-4b-q4km")
    ap.add_argument("--embed-model", default="nomic-embed-text-v1.5-q8")
    ap.add_argument("--no-models", action="store_true", help="text-only: install no model")
    ap.add_argument("--models-cache", default="", help="shared models dir: moved into <home>/data/models "
                    "before the daemon starts and back afterwards")
    ap.add_argument("--gg2-dir", default="", help="dir with the GG2 harness files (default: <out>/gg2, "
                    "extracted from --gg2-ref with git show)")
    ap.add_argument("--gg2-ref", default=GG2_REF)
    ap.add_argument("--probes", default="", help="search probes (default: GG2 repro/search-probes.txt)")
    ap.add_argument("--first-probes", default="", help="comma-separated probes timed during sync")
    ap.add_argument("--ask-ids", default=DEFAULT_ASK_IDS)
    ap.add_argument("--ask-delay", type=float, default=60, help="seconds after sources.add before asking")
    ap.add_argument("--ask-timeout", type=float, default=600)
    ap.add_argument("--poll", type=float, default=2)
    ap.add_argument("--triage-timeout", type=float, default=75 * 60, help="seconds after sources.add")
    a = ap.parse_args()
    a.nuggetd = os.path.abspath(a.nuggetd)
    if a.models_cache:
        a.models_cache = os.path.abspath(a.models_cache)
    asyncio.run(run(a))


if __name__ == "__main__":
    main()
