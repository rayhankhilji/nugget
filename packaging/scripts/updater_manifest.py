#!/usr/bin/env python3
"""Release helpers for Nugget's signed updates (standard library only).

  payloads DIR                list the files in DIR the updater can install (to be signed)
  manifest DIR ...            write the Tauri updater manifest (latest.json) for a release
  promote NEW [CURRENT]       print NEW if it should replace a channel feed holding CURRENT
  checksums DIR               write DIR/SHA256SUMS over every file in DIR

Tauri asks a feed for `{os}-{arch}-{installer}` first, then `{os}-{arch}`; the manifest lists
both, with the plain key pointing at the default installer for that platform.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

# File name pattern (as `tauri build` names bundles; the macOS tarball is made by the release
# workflow) -> (os-arch, installer, is the default for os-arch).
PATTERNS: list[tuple[re.Pattern[str], str, str, bool]] = [
    (re.compile(r"_aarch64\.app\.tar\.gz$"), "darwin-aarch64", "app", True),
    (re.compile(r"_x64\.app\.tar\.gz$"), "darwin-x86_64", "app", True),
    (re.compile(r"_x64-setup\.exe$"), "windows-x86_64", "nsis", True),
    (re.compile(r"_x64_[A-Za-z-]+\.msi$"), "windows-x86_64", "msi", False),
    (re.compile(r"_amd64\.AppImage$"), "linux-x86_64", "appimage", True),
    (re.compile(r"_aarch64\.AppImage$"), "linux-aarch64", "appimage", True),
    (re.compile(r"_amd64\.deb$"), "linux-x86_64", "deb", False),
    (re.compile(r"_arm64\.deb$"), "linux-aarch64", "deb", False),
    (re.compile(r"\.x86_64\.rpm$"), "linux-x86_64", "rpm", False),
    (re.compile(r"\.aarch64\.rpm$"), "linux-aarch64", "rpm", False),
]


def classify(name: str) -> tuple[str, str, bool] | None:
    for pattern, platform, installer, default in PATTERNS:
        if pattern.search(name):
            return platform, installer, default
    return None


def payloads(dist: Path) -> list[Path]:
    return sorted(p for p in dist.iterdir() if p.is_file() and classify(p.name))


def build_manifest(dist: Path, version: str, notes: str, pub_date: str, base_url: str) -> dict:
    platforms: dict[str, dict[str, str]] = {}
    for path in payloads(dist):
        platform, installer, default = classify(path.name)  # type: ignore[misc]
        sig = path.with_name(path.name + ".sig")
        if not sig.is_file():
            raise SystemExit(f"missing signature for {path.name}")
        entry = {
            "signature": sig.read_text(encoding="utf-8").strip(),
            "url": f"{base_url.rstrip('/')}/{path.name}",
        }
        platforms[f"{platform}-{installer}"] = entry
        if default:
            platforms[platform] = entry
    if not platforms:
        raise SystemExit(f"no updater payloads in {dist}")
    return {
        "version": version.removeprefix("v"),
        "notes": notes,
        "pub_date": pub_date,
        "platforms": dict(sorted(platforms.items())),
    }


def _semver_key(version: str) -> tuple:
    """Semantic Versioning 2.0 precedence (build metadata ignored)."""
    core, _, pre = version.removeprefix("v").split("+", 1)[0].partition("-")
    nums = tuple(int(x) for x in core.split("."))
    if not pre:
        return (nums, 1, ())
    ids = tuple((0, int(i), "") if i.isdigit() else (1, 0, i) for i in pre.split("."))
    return (nums, 0, ids)


def newer(a: str, b: str) -> bool:
    return _semver_key(a) > _semver_key(b)


def checksums(dist: Path) -> Path:
    out = dist / "SHA256SUMS"
    lines = []
    for p in sorted(dist.iterdir()):
        if p.is_file() and p.name != out.name:
            lines.append(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n")
    out.write_text("".join(lines), encoding="utf-8")
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("payloads")
    p.add_argument("dist", type=Path)
    m = sub.add_parser("manifest")
    m.add_argument("dist", type=Path)
    m.add_argument("--version", required=True)
    m.add_argument("--notes-file", type=Path, required=True)
    m.add_argument("--pub-date", required=True, help="RFC 3339")
    m.add_argument("--base-url", required=True, help="download URL of the release's assets")
    m.add_argument("--out", type=Path, required=True)
    pr = sub.add_parser("promote")
    pr.add_argument("new", type=Path)
    pr.add_argument("current", type=Path, nargs="?")
    c = sub.add_parser("checksums")
    c.add_argument("dist", type=Path)
    args = ap.parse_args(argv)

    if args.cmd == "payloads":
        for path in payloads(args.dist):
            print(path)
    elif args.cmd == "manifest":
        notes = args.notes_file.read_text(encoding="utf-8").strip()
        manifest = build_manifest(args.dist, args.version, notes, args.pub_date, args.base_url)
        args.out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    elif args.cmd == "promote":
        new = json.loads(args.new.read_text(encoding="utf-8"))["version"]
        if args.current and args.current.is_file():
            current = json.loads(args.current.read_text(encoding="utf-8"))["version"]
            if not newer(new, current):
                print(f"keeping {current}: {new} is not newer", file=sys.stderr)
                return 0
        print(new)
    elif args.cmd == "checksums":
        print(checksums(args.dist))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
