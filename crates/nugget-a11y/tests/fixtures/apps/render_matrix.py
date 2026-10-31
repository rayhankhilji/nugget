#!/usr/bin/env python3
"""Render the nugget-a11y app matrix (JSON Lines from tests/apps_matrix.rs) as Markdown.

    render_matrix.py results.jsonl [more.jsonl ...] --stdout
    render_matrix.py results.jsonl --summary >> "$GITHUB_STEP_SUMMARY"
    render_matrix.py results*.jsonl --update docs/book/src/guide/double-tap.md --marker guide
    render_matrix.py --self-test

One table per OS; rows are app x fixture; columns detect, read, thread P/R, secure, insert
(passed / total and the failing payload ids), undo, read time median. Marks: pass, fail, and
"—" for not run. Failures are listed under each table with their diff excerpt. Python 3
standard library only.
"""

import argparse
import json
import sys

PASS = "✅"
FAIL = "❌"
NONE = "—"
OS_NAMES = {"macos": "macOS", "windows": "Windows", "linux": "Linux"}
OS_ORDER = ["macos", "windows", "linux"]
PAYLOADS = ["ascii", "newlines", "emoji", "rtl", "combining", "long"]


def load(paths):
    recs = []
    for p in paths:
        with open(p, encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    recs.append(json.loads(line))
                except json.JSONDecodeError as e:
                    print(f"{p}:{n}: skipped bad line ({e})", file=sys.stderr)
    return recs


def mark(v):
    if v is True:
        return PASS
    if v is False:
        return FAIL
    return NONE


def esc(s):
    """Text safe inside a Markdown table cell."""
    s = "" if s is None else str(s)
    s = s.replace("\\", "\\\\").replace("|", "\\|").replace("\n", "\\n").replace("\r", "\\r")
    return s.replace("<", "&lt;").replace(">", "&gt;")


def short(s, n=160):
    s = "" if s is None else str(s)
    return s if len(s) <= n else s[:n] + "…"


def pct(x):
    return NONE if x is None else f"{round(x * 100)}%"


def thread_cell(t):
    if not isinstance(t, dict):
        return NONE
    p, r = t.get("precision"), t.get("recall")
    if p is None and r is None:
        return NONE
    ok = p == 1 and r == 1
    if r is None:
        ok = p == 1
    extra = []
    if t.get("from_me_ok") is False:
        extra.append("from_me ❌")
    if t.get("recipients_ok") is not None:
        extra.append("to " + mark(t.get("recipients_ok")))
    if t.get("subject_ok") is not None:
        extra.append("subj " + mark(t.get("subject_ok")))
    s = f"{PASS if ok else FAIL} {pct(p)}/{pct(r)}"
    return s + (" (" + ", ".join(extra) + ")" if extra else "")


def mail_cell(t):
    """Recipients/subject only (for contexts with no messages in the truth)."""
    if not isinstance(t, dict):
        return NONE
    parts = []
    if t.get("recipients_ok") is not None:
        parts.append("to " + mark(t.get("recipients_ok")))
    if t.get("subject_ok") is not None:
        parts.append("subj " + mark(t.get("subject_ok")))
    return ", ".join(parts) if parts else NONE


def insert_cell(ins):
    if not ins:
        return NONE
    ok = [i for i in ins if i.get("exact") is True]
    ran = {i.get("payload") for i in ins}
    bad = [i.get("payload", "?") for i in ins if i.get("exact") is not True]
    bad += [p for p in PAYLOADS if p not in ran]
    s = f"{PASS if not bad else FAIL} {len(ok)}/{len(PAYLOADS)}"
    methods = sorted({i.get("method") for i in ins if i.get("method")})
    if methods:
        s += " " + "/".join(methods)
    if bad:
        s += " ✗ " + ", ".join(bad)
    return s


def undo_cell(ins):
    tried = [i for i in (ins or []) if i.get("undo_one_step") is not None]
    if not tried:
        return NONE
    ok = [i for i in tried if i.get("undo_one_step") is True]
    s = f"{PASS if len(ok) == len(PAYLOADS) else FAIL} {len(ok)}/{len(PAYLOADS)}"
    bad = [i.get("payload", "?") for i in tried if i.get("undo_one_step") is not True]
    if bad:
        s += " ✗ " + ", ".join(bad)
    return s


def ms_cell(r):
    m = r.get("read_ms")
    if not isinstance(m, dict) or m.get("median") is None:
        return NONE
    return f"{m['median']:.0f} ms"


def app_label(r):
    s = r.get("app", "?")
    if r.get("app_version"):
        s += f" {short(r['app_version'], 40)}"
    if r.get("force_a11y"):
        s += " (forced a11y)"
    return s


def failures(r):
    """(what, detail) for every failing measurement of a record."""
    out = []
    if not r.get("available"):
        return out
    if r.get("error"):
        out.append(("error", esc(short(r["error"], 400))))
    if r.get("detect") is False:
        out.append(("detect", "no focused, non-secure field"))
    if r.get("read_ok") is False:
        d = r.get("read_diff") or {}
        out.append(("read", f"expected `{esc(short(d.get('expected'), 80))}`, got "
                            f"`{esc(short(d.get('got'), 80))}`"))
    t = r.get("thread")
    if isinstance(t, dict):
        if (t.get("precision") not in (None, 1)) or (t.get("recall") not in (None, 1)):
            got = t.get("got_texts")
            out.append(("thread", f"P {pct(t.get('precision'))} R {pct(t.get('recall'))}"
                        + (f"; reported {esc(short(json.dumps(got, ensure_ascii=False), 200))}"
                           if got else "")))
        for k in ("from_me_ok", "recipients_ok", "subject_ok"):
            if t.get(k) is False:
                extra = ""
                if k == "recipients_ok":
                    extra = f": got {esc(json.dumps(t.get('got_recipients'), ensure_ascii=False))}"
                if k == "subject_ok":
                    extra = f": got {esc(json.dumps(t.get('got_subject'), ensure_ascii=False))}"
                out.append((k.replace("_ok", ""), "wrong" + extra))
    sec = r.get("secure")
    if isinstance(sec, dict) and sec.get("error"):
        out.append(("secure", esc(short(sec["error"], 200))))
    if r.get("secure_ok") is False:
        s = r.get("secure") or {}
        bad = [k for k in ("secure", "value_empty", "insert_refused", "password_len_unchanged")
               if s.get(k) is not True]
        out.append(("secure", "not " + ", ".join(bad) if bad else "failed"))
    for i in r.get("inserts") or []:
        pid = i.get("payload", "?")
        if i.get("exact") is not True:
            d = i.get("diff") or {}
            det = f"method {i.get('method')}, verified {i.get('verified')}"
            if i.get("error"):
                det += f", error: {esc(short(i['error'], 160))}"
            if d:
                det += (f"; first difference at char {d.get('index')}: expected "
                        f"`{esc(d.get('expected'))}`, got `{esc(d.get('got'))}`")
            out.append((f"insert {pid}", det))
        if i.get("undo_one_step") is False:
            out.append((f"undo {pid}", f"after one undo: `{esc(short(i.get('undo_text'), 80))}`"))
    return out


def render(recs, heading_level=3):
    if not recs:
        return "_No app matrix results._\n"
    h = "#" * heading_level
    # One table per runner (windows-latest and windows-2022 apart), else per OS.
    groups = {}
    for r in recs:
        key = (r.get("os", "?"), r.get("runner") or "")
        groups.setdefault(key, []).append(r)

    def order(k):
        os_id, runner = k
        return (OS_ORDER.index(os_id) if os_id in OS_ORDER else len(OS_ORDER), os_id, runner)

    lines = []
    for os_id, runner in sorted(groups, key=order):
        rows = groups[(os_id, runner)]
        versions = sorted({r.get("os_version") for r in rows if r.get("os_version")})
        title = OS_NAMES.get(os_id, os_id) + (f" `{esc(runner)}`" if runner else "")
        lines.append(f"{h} {title}" + (f" ({esc(', '.join(versions))})" if versions else ""))
        lines.append("")
        lines.append("| app | fixture | detect | read | thread P/R | secure | insert | undo "
                     "| read (median) |")
        lines.append("|---|---|---|---|---|---|---|---|---|")
        notes = []
        for r in rows:
            fixture = r.get("fixture", "?")
            if not r.get("available"):
                lines.append(f"| {esc(r.get('app'))} | {esc(fixture)} | not available "
                             f"| {NONE} | {NONE} | {NONE} | {NONE} | {NONE} | {NONE} |")
                continue
            t = r.get("thread")
            tcell = thread_cell(t)
            if tcell == NONE:
                tcell = mail_cell(t)
            detect = mark(r.get("detect"))
            if r.get("role"):
                detect += f" {esc(r['role'])}"
            read = mark(r.get("read_ok"))
            if r.get("oracle") == "a11y":
                read += "ᵃ"
            fails = failures(r)
            fixture_cell = esc(fixture)
            if fails:
                notes.append((r, fails))
                fixture_cell += f" [{len(notes)}]"
            lines.append(
                f"| {esc(app_label(r))} | {fixture_cell} | {detect} | {read} | {tcell} "
                f"| {mark(r.get('secure_ok'))} | {insert_cell(r.get('inserts'))} "
                f"| {undo_cell(r.get('inserts'))} | {ms_cell(r)} |")
        lines.append("")
        if any(r.get("oracle") == "a11y" for r in rows):
            lines.append("ᵃ the app's text was read back through accessibility itself "
                         "(no independent oracle).")
            lines.append("")
        if notes:
            lines.append("Failures:")
            lines.append("")
            for n, (r, fails) in enumerate(notes, 1):
                lines.append(f"{n}. **{esc(r.get('app'))} / {esc(r.get('fixture'))}**")
                for what, det in fails:
                    lines.append(f"   - {what}: {det}")
            lines.append("")
        missing = [r for r in rows if not r.get("available")]
        if missing:
            lines.append("Not available: " + "; ".join(
                f"{esc(r.get('app'))} ({esc(short(r.get('error'), 120))})" for r in missing))
            lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def update(path, marker, body):
    start = f"<!-- a11y-matrix:{marker}:start -->"
    end = f"<!-- a11y-matrix:{marker}:end -->"
    with open(path, encoding="utf-8") as f:
        text = f.read()
    i = text.find(start)
    j = text.find(end, i + len(start)) if i >= 0 else -1
    if i < 0 or j < 0:
        raise SystemExit(f"{path}: markers {start} / {end} not found")
    new = text[: i + len(start)] + "\n\n" + body.rstrip() + "\n\n" + text[j:]
    with open(path, "w", encoding="utf-8") as f:
        f.write(new)


SAMPLE = [
    {"os": "linux", "os_version": "Ubuntu 24.04", "app": "firefox", "app_version": "131.0",
     "fixture": "quill", "kind": "rich", "context": "chat", "available": True, "detect": True,
     "role": "text_area", "read_ok": True,
     "thread": {"precision": 1.0, "recall": 0.75, "from_me_ok": True, "recipients_ok": None,
                "subject_ok": None, "got_texts": ["Hi | there"]},
     "secure_ok": True,
     "inserts": [
         {"payload": "ascii", "method": "accessibility", "verified": True, "exact": True,
          "undo_one_step": True},
         {"payload": "emoji", "method": "clipboard", "verified": False, "exact": False,
          "undo_one_step": False, "undo_text": "Draft: x",
          "diff": {"index": 14, "expected": "Thanks 👍🏽 see", "got": "Thanks 👍 see"}}],
     "read_ms": {"median": 42.0, "max": 80.0}, "oracle": "page", "force_a11y": False},
    {"os": "linux", "app": "gedit", "fixture": "native", "kind": "native", "available": True,
     "detect": True, "role": "text_area", "read_ok": True, "thread": None, "secure_ok": None,
     "inserts": [{"payload": "ascii", "method": "accessibility", "verified": True,
                  "exact": True, "undo_one_step": True}],
     "read_ms": {"median": 10.0, "max": 12.0}, "oracle": "a11y", "force_a11y": False},
    {"os": "linux", "app": "edge", "fixture": "*", "available": False,
     "error": "not installed"},
    {"os": "macos", "app": "safari", "fixture": "mail-compose", "context": "mail",
     "available": True, "detect": False, "error": "no focused field",
     "thread": {"precision": None, "recall": None, "recipients_ok": False, "subject_ok": True,
                "got_recipients": []},
     "inserts": [], "read_ms": None, "oracle": "page"},
]


def self_test():
    md = render(SAMPLE)
    assert "### Linux (Ubuntu 24.04)" in md, md
    assert "### macOS" in md and md.index("### macOS") < md.index("### Linux"), md
    assert "| firefox 131.0 | quill [1] | ✅ text_area | ✅ | ❌ 100%/75% |" in md, md
    assert "❌ 1/6 accessibility/clipboard ✗ emoji, newlines, rtl, combining, long" in md, md
    assert "❌ 1/6 ✗ emoji" in md, md
    assert "| edge | * | not available |" in md, md
    assert "first difference at char 14" in md, md
    assert "ᵃ" in md and "| gedit | native | ✅ text_area | ✅ᵃ |" in md, md
    assert "Hi \\| there" in md, md
    assert "to ❌, subj ✅" in md, md
    assert "error: no focused field" in md, md
    assert render([]) == "_No app matrix results._\n"
    import os
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "doc.md")
        with open(p, "w", encoding="utf-8") as f:
            f.write("a\n<!-- a11y-matrix:x:start -->\nold\n<!-- a11y-matrix:x:end -->\nb\n")
        update(p, "x", "NEW")
        with open(p, encoding="utf-8") as f:
            t = f.read()
        assert t == "a\n<!-- a11y-matrix:x:start -->\n\nNEW\n\n<!-- a11y-matrix:x:end -->\nb\n", t
        update(p, "x", "NEWER")
        with open(p, "w", encoding="utf-8") as f:
            f.write("<!-- a11y-matrix:y:end -->\n<!-- a11y-matrix:y:start -->\nz\n"
                    "<!-- a11y-matrix:y:end -->\n")
        update(p, "y", "Y")
        with open(p, encoding="utf-8") as f:
            assert f.read() == ("<!-- a11y-matrix:y:end -->\n<!-- a11y-matrix:y:start -->\n\nY\n\n"
                                "<!-- a11y-matrix:y:end -->\n")
        update(p, "y", "NEWER")
        with open(p, encoding="utf-8") as f:
            assert "NEW\n" not in f.read().replace("NEWER", "")
        try:
            update(p, "missing", "x")
        except SystemExit:
            pass
        else:
            raise AssertionError("missing markers must fail")
        j = os.path.join(d, "r.jsonl")
        with open(j, "w", encoding="utf-8") as f:
            f.write("\n".join(json.dumps(r) for r in SAMPLE) + "\nnot json\n")
        assert len(load([j])) == len(SAMPLE)
    print("render_matrix.py self-test: ok")


def main():
    # Windows consoles default to a legacy code page that cannot encode the marks.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("files", nargs="*", help="JSON Lines result files")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--stdout", action="store_true", help="print the Markdown (default)")
    mode.add_argument("--summary", action="store_true",
                      help="Markdown for $GITHUB_STEP_SUMMARY (with a title)")
    mode.add_argument("--update", metavar="FILE",
                      help="replace the text between the a11y-matrix markers in FILE")
    mode.add_argument("--self-test", action="store_true", help="run the built-in checks")
    ap.add_argument("--marker", help="marker NAME for --update")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return
    if not a.files:
        ap.error("no result files given")
    recs = load(a.files)
    if a.update:
        if not a.marker:
            ap.error("--update needs --marker NAME")
        update(a.update, a.marker, render(recs, heading_level=4))
        return
    if a.summary:
        sys.stdout.write("## A11y app matrix\n\n" + render(recs))
        return
    sys.stdout.write(render(recs))


if __name__ == "__main__":
    main()
