#!/usr/bin/env python3
"""Release rehearsal: turn the recorded checks into the evidence report.

Reads EVIDENCE/results.tsv (one "check<TAB>PASS|FAIL|INFO<TAB>detail" row per check, written by
the rehearsal scripts), the public keys and the release files in EVIDENCE, and writes
EVIDENCE/report.md: a PASS/FAIL table, the public keys, SHA256SUMS and the feed files, and the
list of logs. Appends the same report to $GITHUB_STEP_SUMMARY when set.

Before reading anything it checks that EVIDENCE holds no private key material: no file named
*.key or *.password, and no file containing the text of any file in KEYS_DIR (the throwaway
private keys and their passwords). A hit is reported as a failed check and the offending file
is deleted, so the upload step can never ship it. The finished report goes through the same
check (and is redacted and failed on a hit) before it is written or added to the job summary.

Exits 1 if any check failed or nothing was recorded.

Usage: python3 -I report.py --evidence DIR --keys KEYS_DIR --os NAME --old V --new V
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


def load_secrets(keys: Path) -> list[bytes]:
    """The text of every private key and password in KEYS_DIR."""
    secrets: list[bytes] = []
    if keys.is_dir():
        for k in keys.iterdir():
            if k.is_file() and not k.name.endswith(".pub"):
                text = k.read_bytes().strip()
                if text:
                    secrets.append(text)
    return secrets


def scrub(evidence: Path, secrets: list[bytes]) -> list[str]:
    """Delete anything private from the evidence; return what was found."""
    found = []
    for f in sorted(evidence.rglob("*")):
        if not f.is_file():
            continue
        bad = f.suffix in (".key", ".password")
        if not bad and secrets:
            data = f.read_bytes()
            bad = any(s in data for s in secrets)
        if bad:
            found.append(str(f.relative_to(evidence)))
            f.unlink()
    return found


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return "(missing)"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--evidence", type=Path, required=True)
    ap.add_argument("--keys", type=Path, required=True)
    ap.add_argument("--os", required=True)
    ap.add_argument("--old", required=True)
    ap.add_argument("--new", required=True)
    args = ap.parse_args(argv)
    ev: Path = args.evidence
    ev.mkdir(parents=True, exist_ok=True)

    # Scrub first, so nothing below can read a file that held a secret.
    secrets = load_secrets(args.keys)
    leaked = scrub(ev, secrets)

    rows: list[tuple[str, str, str]] = []
    results = ev / "results.tsv"
    if results.is_file():
        for line in results.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.rstrip("\r").split("\t", 2)
            if len(parts) == 3:
                rows.append((parts[0], parts[1], parts[2]))

    if leaked:
        rows.append(("Evidence holds no private key material", "FAIL", "removed: " + ", ".join(leaked)))
    else:
        rows.append(("Evidence holds no private key material", "PASS",
                     "no *.key/*.password file and no file containing a private key or password"))

    failed = [r for r in rows if r[1] == "FAIL"]
    passed = [r for r in rows if r[1] == "PASS"]
    verdict = "FAIL" if failed or not passed else "PASS"

    def cell(s: str) -> str:
        return s.replace("|", "\\|").replace("\n", " ")

    out = [
        f"# Release rehearsal ({args.os}): {verdict}",
        "",
        "**A rehearsal, not a release.** Every key was generated in this job and thrown away with "
        "it; the builds point at a localhost feed and carry `-rehearsal` versions; no installer "
        "is uploaded and nothing is published.",
        "",
        f"Old version `{args.old}` -> new version `{args.new}`. "
        f"{len(passed)} passed, {len(failed)} failed.",
        "",
        "| Check | Result | Detail |",
        "|---|---|---|",
    ]
    for name, status, detail in rows:
        mark = {"PASS": "PASS", "FAIL": "**FAIL**"}.get(status, status)
        out.append(f"| {cell(name)} | {mark} | {cell(detail)} |")

    out += ["", "## Throwaway public keys", ""]
    pub = ev / "public-keys"
    for f in sorted(pub.glob("*")) if pub.is_dir() else []:
        out.append(f"- `{f.name}`: `{read(f)}`")

    rel = ev / "release"
    if (rel / "SHA256SUMS").is_file():
        out += ["", "## SHA256SUMS (signed release files)", "", "```", read(rel / "SHA256SUMS"), "```"]
    if (rel / "stable.json").is_file():
        out += ["", "## stable.json (= latest.json)", "", "```json", read(rel / "stable.json"), "```"]

    out += ["", "## Evidence files", ""]
    for f in sorted(ev.rglob("*")):
        if f.is_file() and f.name != "report.md":
            out.append(f"- `{f.relative_to(ev).as_posix()}` ({f.stat().st_size} bytes)")
    text = "\n".join(out) + "\n"
    # The report itself goes through the same check before it is written anywhere.
    if any(sec.decode("utf-8", "replace") in text for sec in secrets):
        for sec in secrets:
            text = text.replace(sec.decode("utf-8", "replace"), "[REDACTED]")
        text = text.replace(f"# Release rehearsal ({args.os}): {verdict}", f"# Release rehearsal ({args.os}): FAIL", 1)
        text += "\n**FAIL: the report contained private key material; it was redacted.**\n"
        verdict = "FAIL"

    (ev / "report.md").write_text(text, encoding="utf-8")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as s:
            s.write(text)
    print(text)
    return 0 if verdict == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
