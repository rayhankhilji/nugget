#!/usr/bin/env python3
"""JJ1-1 before/after: compare two runs of QA JJ1's `realmodel.py --part triage`.

    jj1_compare.py BEFORE_OUT AFTER_OUT [--hardware hardware.txt] [--labels B A] [--out compare.md]
                   [--labels-dir DIR]

Each *_OUT is a realmodel.py --out directory: triage.json (and work/export/**/items.jsonl, the
export realmodel.py read its labels from, when it is still there). Writes a markdown report to
stdout, to --out, and appended to $GITHUB_STEP_SUMMARY when that is set. With --labels-dir it also
writes labels-<label>.jsonl there: one compact line per exported item (name, sensitive,
sensitive_rule, junk, triage), so the upload does not need the whole export. Stdlib only.

What is compared (QA JJ1-1: after 4B triage only 9/22 planted sensitive mails stayed sensitive):

- The 22 planted sensitive mails (triage.json rows with class "sensitive"): final
  `metadata.sensitive`, overall and per language (en/de/es/fr) and kind (health/financial/
  credentials/legal); what the model said (`metadata.triage.sensitive`, row `model_sensitive`) and
  what the rules said (`metadata.sensitive_rule`, row `rule`), and the triage state.
  After 8f606a20 `triage.sensitive` is the daemon's reading of the per-category answer (any
  category true, or a topic that is not a named safe one, means sensitive); the raw categories
  are not stored.
- False positives: normal mails (n*) and junk (j*) that end sensitive, with the cause (rule hit,
  model kept it, or triage not done: pending/unknown/failed/skipped all count as sensitive), and
  the triage state counts. From the export when present; otherwise triage.json's
  normal_sensitive_rate and normal_triage_states.
- Junk labelled, junk still searchable, normal junk rate; full-triage time; first search; Ask
  during the sync and when idle (first token / total / correct).
"""
import argparse
import json
import os
import re
import statistics
import sys

LANGS = ("en", "de", "es", "fr")
KINDS = ("health", "financial", "credentials", "legal")
STATES = ("done", "pending", "failed", "skipped", "unknown")


def load_run(d):
    """triage.json plus the exported labels (name -> metadata subset), either may be None."""
    run = {"dir": d, "res": None, "labels": None}
    p = os.path.join(d, "triage.json")
    if os.path.isfile(p):
        with open(p, encoding="utf-8") as f:
            run["res"] = json.load(f)
    exp = os.path.join(d, "work", "export")
    if os.path.isdir(exp):
        labels = {}
        for root, _, files in os.walk(exp):
            if "items.jsonl" in files:
                with open(os.path.join(root, "items.jsonl"), encoding="utf-8") as f:
                    for line in f:
                        if not line.strip():
                            continue
                        it = json.loads(line)
                        m = it.get("metadata") or {}
                        # Same key as realmodel.py: the file name.
                        labels[str(it.get("key", "")).split("/")[-1]] = {
                            "sensitive": m.get("sensitive"),
                            "rule": m.get("sensitive_rule"),
                            "junk": m.get("junk"),
                            "triage": m.get("triage"),
                        }
        run["labels"] = labels or None
    return run


def frac(n, d):
    return f"{n}/{d}" if d else "–"


def secs(ms):
    return "–" if ms is None else f"{ms / 1000:.1f}"


def state_of(lab):
    t = (lab or {}).get("triage") or {}
    return t.get("state") or "missing"


def cause(lab):
    """Why an item ended sensitive."""
    if not lab.get("sensitive"):
        return None
    if lab.get("rule"):
        return "rule"
    st = state_of(lab)
    if st == "done":
        return "model"
    return f"state {st}"


def counts(items, key):
    c = {}
    for x in items:
        k = key(x)
        if k is not None:
            c[k] = c.get(k, 0) + 1
    return c


def fmt_counts(c, order=None):
    if not c:
        return "–"
    keys = [k for k in (order or ()) if k in c] + sorted(k for k in c if k not in (order or ()))
    return ", ".join(f"{k} {c[k]}" for k in keys)


def ask_stats(rs):
    if not rs:
        return "–"
    ftt = [r.get("first_token_ms") for r in rs if r.get("first_token_ms") is not None]
    tot = [r.get("total_ms") for r in rs if r.get("total_ms") is not None]
    ok = sum(1 for r in rs if r.get("correct"))
    err = sum(1 for r in rs if r.get("error"))
    s = f"{ok}/{len(rs)} correct; first token median {secs(statistics.median(ftt)) if ftt else '–'} s"
    s += f" (max {secs(max(ftt)) if ftt else '–'}); total median {secs(statistics.median(tot)) if tot else '–'} s"
    return s + (f"; {err} errors" if err else "")


def last_timeline(res):
    tl = res.get("timeline") or []
    return max(tl, key=lambda e: e.get("s", 0)) if tl else None


def metrics(run):
    res = run["res"]
    if res is None:
        return None
    rows = res.get("rows") or []
    sens = [r for r in rows if r.get("class") == "sensitive"]
    junk = [r for r in rows if r.get("class") == "junk"]
    nprobe = [r for r in rows if r.get("class") == "normal"]
    m = {"res": res, "sens": sens, "junk": junk}
    m["sens_final"] = sum(1 for r in sens if r.get("sensitive"))
    m["sens_model"] = sum(1 for r in sens if r.get("model_sensitive"))
    m["sens_rule"] = sum(1 for r in sens if r.get("rule"))
    m["sens_states"] = counts(sens, lambda r: r.get("triage_state") or "missing")
    # Cleared by the model: no rule, labelled done, model said not sensitive, final not sensitive.
    m["sens_cleared"] = [r for r in sens if not r.get("sensitive")]
    m["by_lang"] = {l: [r for r in sens if r.get("lang") == l] for l in LANGS}
    m["by_kind"] = {k: [r for r in sens if r.get("kind") == k] for k in KINDS}
    m["junk_labelled"] = sum(1 for r in junk if r.get("junk"))
    m["junk_model"] = sum(1 for r in junk if r.get("model_junk"))
    m["junk_probe"] = [r for r in junk if r.get("probe")]
    m["junk_searchable"] = sum(1 for r in m["junk_probe"] if r.get("searchable"))
    m["normal_probe"] = nprobe
    m["normal_probe_searchable"] = sum(1 for r in nprobe if r.get("searchable"))
    labels = run["labels"]
    if labels:
        normal = [v for k, v in labels.items() if k.startswith("n")]
        jl = [v for k, v in labels.items() if k.startswith("j")]
        m["normal_n"] = len(normal)
        m["normal_sens"] = sum(1 for v in normal if v.get("sensitive"))
        m["normal_causes"] = counts(normal, cause)
        m["normal_states"] = counts(normal, state_of)
        m["normal_model_sens"] = sum(1 for v in normal if ((v.get("triage") or {}).get("sensitive")) is True)
        m["normal_rule"] = sum(1 for v in normal if v.get("rule"))
        m["normal_junk"] = sum(1 for v in normal if v.get("junk"))
        m["junk_sens"] = sum(1 for v in jl if v.get("sensitive"))
        m["junk_causes"] = counts(jl, cause)
        m["junk_states"] = counts(jl, state_of)
        m["exported"] = len(labels)
        m["normal_rules"] = counts(normal, lambda v: v.get("rule") or None)
    else:
        st = res.get("normal_triage_states") or {}
        n = sum(st.values()) if st else None
        m["normal_n"] = n
        m["normal_sens"] = round(res["normal_sensitive_rate"] * n) if n and res.get("normal_sensitive_rate") is not None else None
        m["normal_states"] = {k: v for k, v in st.items() if v}
        m["normal_junk"] = round(res["normal_junk_rate"] * n) if n and res.get("normal_junk_rate") is not None else None
        m["junk_sens"] = sum(1 for r in junk if r.get("sensitive"))
        m["junk_states"] = counts(junk, lambda r: r.get("triage_state") or "missing")
    fs = res.get("first_search_s") or {}
    m["first_search"] = (min(fs.values()), max(fs.values())) if fs else None
    return m


def cpu(hardware):
    lines = []
    if hardware and os.path.isfile(hardware):
        with open(hardware, encoding="utf-8", errors="replace") as f:
            for line in f:
                if re.match(r"\s*(Model name|CPU\(s\)):", line):
                    lines.append(" ".join(line.split()))
                elif line.startswith("Mem:") and len(line.split()) > 1:
                    lines.append(f"RAM: {line.split()[1]} GB (free -g total)")
    return "; ".join(lines) or "unknown"


def yn(v):
    return "–" if v is None else ("✓" if v else "✗")


def render(runs, labels, hardware):
    ms = [metrics(r) for r in runs]
    out = ["## JJ1-1 before/after: sensitive labels after real 4B triage (QA JJ1 harness, Linux)", ""]
    out.append(f"CPU: {cpu(hardware)}")
    out.append("")
    out += ["| | " + " | ".join(labels) + " |", "|---|" + "---|" * len(labels)]

    def row(name, fn):
        out.append(f"| {name} | " + " | ".join("no triage.json" if m is None else fn(m) for m in ms) + " |")

    row("mails / exported items", lambda m: f"{m['res'].get('items')} / {m.get('exported', '–')}")
    row("**planted sensitive that stay sensitive**", lambda m: f"**{frac(m['sens_final'], len(m['sens']))}**")
    for l in LANGS:
        row(f"… {l}", lambda m, l=l: frac(sum(1 for r in m["by_lang"][l] if r.get("sensitive")), len(m["by_lang"][l])))
    for k in KINDS:
        row(f"… {k}", lambda m, k=k: frac(sum(1 for r in m["by_kind"][k] if r.get("sensitive")), len(m["by_kind"][k])))
    row("planted: model said sensitive (`triage.sensitive`)", lambda m: frac(m["sens_model"], len(m["sens"])))
    row("planted: a rule matched (`sensitive_rule`)", lambda m: frac(m["sens_rule"], len(m["sens"])))
    row("planted: triage states", lambda m: fmt_counts(m["sens_states"], STATES))
    row("**normal mails that end sensitive** (false positives)",
        lambda m: f"**{frac(m['normal_sens'], m['normal_n'])}**" if m.get("normal_sens") is not None else "–")
    row("… why (rule / model kept it / triage not done)", lambda m: fmt_counts(m.get("normal_causes") or {}) if "normal_causes" in m else "(no export)")
    row("… normal: rule hits by rule", lambda m: fmt_counts(m.get("normal_rules") or {}) if "normal_rules" in m else "(no export)")
    row("normal: model said sensitive", lambda m: frac(m["normal_model_sens"], m["normal_n"]) if "normal_model_sens" in m else "(no export)")
    row("normal: triage states", lambda m: fmt_counts(m.get("normal_states") or {}, STATES))
    row("normal labelled junk", lambda m: frac(m["normal_junk"], m["normal_n"]) if m.get("normal_junk") is not None else "–")
    row("junk labelled junk", lambda m: f"{frac(m['junk_labelled'], len(m['junk']))} (model {m['junk_model']})")
    row("junk still searchable (probes)", lambda m: frac(m["junk_searchable"], len(m["junk_probe"])))
    row("junk that ends sensitive", lambda m: frac(m["junk_sens"], len(m["junk"])) + (f" ({fmt_counts(m['junk_causes'])})" if m.get("junk_causes") else ""))
    row("junk: triage states", lambda m: fmt_counts(m.get("junk_states") or {}, STATES))
    row("normal probes searchable", lambda m: frac(m["normal_probe_searchable"], len(m["normal_probe"])))
    row("first search (s, min–max of 6 probes)", lambda m: "–" if not m["first_search"] else f"{m['first_search'][0]}–{m['first_search'][1]}")
    row("**full triage (s)** (`t_triaged_s`)", lambda m: f"**{m['res'].get('t_triaged_s')}**")
    row("triage at the last poll", lambda m: "`" + json.dumps((last_timeline(m["res"]) or {}).get("triage"), ensure_ascii=False)[:160] + "`")
    row("Ask during the sync", lambda m: ask_stats(m["res"].get("ask_during")))
    row("Ask after triage (idle)", lambda m: ask_stats(m["res"].get("ask_idle")))
    out.append("")
    out.append("Unknown, pending and failed triage count as sensitive, so a normal mail that ends sensitive"
               " is either a rule hit, the model keeping it (state done), or triage not finished.")
    out.append("")

    # Per planted item.
    if any(ms):
        base = next(m for m in ms if m)
        out.append("<details><summary>The 22 planted sensitive mails: final sensitive / rule / model said / state</summary>\n")
        out.append("| lang | kind | subject | " + " | ".join(labels) + " |")
        out.append("|---|---|---|" + "---|" * len(labels))
        for r in base["sens"]:
            cells = []
            for m in ms:
                x = None if m is None else next((s for s in m["sens"] if s.get("path") == r.get("path")), None)
                cells.append("–" if x is None else f"{yn(x.get('sensitive'))} / {x.get('rule') or '–'} / {yn(x.get('model_sensitive'))} / {x.get('triage_state') or '–'}")
            out.append(f"| {r.get('lang')} | {r.get('kind')} | {str(r.get('subject', '')).replace('|', '/')} | " + " | ".join(cells) + " |")
        out.append("\n</details>\n")
        for lab, m in zip(labels, ms):
            if m and m["sens_cleared"]:
                out.append(f"**{lab}: planted sensitive mails cleared:** " + "; ".join(
                    f"{r.get('lang')} {r.get('kind')} \"{r.get('subject')}\"" for r in m["sens_cleared"]))
                out.append("")
        for k in ("ask_during", "ask_idle"):
            out.append(f"<details><summary>{k}: first token / total ms, correct</summary>\n")
            out.append("| question | " + " | ".join(labels) + " |")
            out.append("|---|" + "---|" * len(labels))
            qs = [q.get("query") for q in (base["res"].get(k) or [])]
            for q in qs:
                cells = []
                for m in ms:
                    x = None if m is None else next((a for a in m["res"].get(k) or [] if a.get("query") == q), None)
                    cells.append("–" if x is None else (f"error: {str(x['error'])[:60]}" if x.get("error") else f"{x.get('first_token_ms')} / {x.get('total_ms')} {yn(x.get('correct'))}"))
                out.append(f"| {q} | " + " | ".join(cells) + " |")
            out.append("\n</details>\n")
    return "\n".join(out) + "\n"


def dump_labels(run, path):
    if not run["labels"]:
        return
    with open(path, "w", encoding="utf-8") as f:
        for k in sorted(run["labels"]):
            f.write(json.dumps({"name": k, **run["labels"][k]}, ensure_ascii=False) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--hardware", default="")
    ap.add_argument("--labels", nargs=2, default=["before", "after"])
    ap.add_argument("--out", default="")
    ap.add_argument("--labels-dir", default="")
    a = ap.parse_args()
    runs = [load_run(a.before), load_run(a.after)]
    md = render(runs, a.labels, a.hardware)
    sys.stdout.write(md)
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(md)
    if a.labels_dir:
        os.makedirs(a.labels_dir, exist_ok=True)
        for tag, run in zip(("before", "after"), runs):
            dump_labels(run, os.path.join(a.labels_dir, f"labels-{tag}.jsonl"))
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(md)


if __name__ == "__main__":
    main()
