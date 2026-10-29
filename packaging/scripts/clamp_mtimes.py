#!/usr/bin/env python3
"""Clamp file modification times to SOURCE_DATE_EPOCH (reproducible-builds.org convention).

Every file, directory and symlink under the given paths whose mtime is later than the epoch gets
the epoch as its access and modification time; older entries are left alone. Pure Python, so it
behaves the same on Linux, macOS and Windows (GNU and BSD `touch` disagree on `-d @N`).

Usage: clamp_mtimes.py [--epoch N] PATH...   (default epoch: $SOURCE_DATE_EPOCH)
Exits 0 without touching anything when no epoch is given, so build scripts can call it
unconditionally; pass --require to make a missing epoch an error.
"""

from __future__ import annotations

import argparse
import os
import sys


def clamp(path: str, epoch: int) -> int:
    """Clamps one entry (not following symlinks); returns 1 if it was changed."""
    st = os.lstat(path)
    if st.st_mtime <= epoch:
        return 0
    if os.path.islink(path):
        if os.utime not in os.supports_follow_symlinks:
            return 0  # the platform cannot set a link's own times; its target is clamped anyway
        os.utime(path, (epoch, epoch), follow_symlinks=False)
    else:
        os.utime(path, (epoch, epoch))
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--epoch", type=int, help="seconds since 1970 (default: $SOURCE_DATE_EPOCH)")
    ap.add_argument("--require", action="store_true", help="fail when no epoch is set")
    ap.add_argument("paths", nargs="+")
    args = ap.parse_args()

    epoch = args.epoch
    if epoch is None:
        raw = os.environ.get("SOURCE_DATE_EPOCH", "").strip()
        if not raw:
            if args.require:
                print("clamp_mtimes: SOURCE_DATE_EPOCH is not set", file=sys.stderr)
                return 2
            return 0
        if not raw.isdigit():
            print(f"clamp_mtimes: SOURCE_DATE_EPOCH is not an integer: {raw!r}", file=sys.stderr)
            return 2
        epoch = int(raw)

    changed = 0
    for top in args.paths:
        if not os.path.lexists(top):
            print(f"clamp_mtimes: no such path: {top}", file=sys.stderr)
            return 2
        if os.path.isdir(top) and not os.path.islink(top):
            # Bottom-up, so clamping a directory's children does not bump it again afterwards.
            for dirpath, dirnames, filenames in os.walk(top, topdown=False):
                for name in filenames + [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]:
                    changed += clamp(os.path.join(dirpath, name), epoch)
                changed += clamp(dirpath, epoch)
        else:
            changed += clamp(top, epoch)
    print(f"clamp_mtimes: {changed} entr{'y' if changed == 1 else 'ies'} clamped to {epoch}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
