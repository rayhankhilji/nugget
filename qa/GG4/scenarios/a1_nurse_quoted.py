#!/usr/bin/env python3
"""Copy persona/world to OUT and RFC 5322-quote the nurse's display name ("Paula Grant, RN"),
to show that the missing clinic card is caused by the unquoted comma in From.

    python3 a1_nurse_quoted.py OUT && python3 a1_replay.py OUT --out RUN --judge off --days 6-6
"""
import shutil
import sys
from pathlib import Path

WORLD = Path(__file__).resolve().parents[1] / "persona" / "world"
out = Path(sys.argv[1])
if out.exists():
    shutil.rmtree(out)
shutil.copytree(WORLD, out)
f = out / "files/Mail/Inbox/20261029-1015-s6-nurse.eml"
t = f.read_bytes().replace(b"From: Paula Grant, RN <", b'From: "Paula Grant, RN" <')
f.write_bytes(t)
print(t.decode().splitlines()[0])
