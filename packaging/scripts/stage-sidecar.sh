#!/usr/bin/env bash
# Build nuggetd for a target and stage it where Tauri's `externalBin` expects it:
#   apps/desktop/src-tauri/binaries/nuggetd-<target-triple>[.exe]
# Usage: packaging/scripts/stage-sidecar.sh [target-triple]   (default: the host triple)
# For a reproducible build, source packaging/scripts/repro-env.sh first: with SOURCE_DATE_EPOCH
# set, the staged binary's mtime is clamped to it (it would otherwise leak into the bundles).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$target" in
  *windows*) ext=".exe" ;;
  *) ext="" ;;
esac

cd "$root"
cargo build --locked --release -p nuggetd --target "$target"

dest="apps/desktop/src-tauri/binaries"
mkdir -p "$dest"
cp "target/$target/release/nuggetd$ext" "$dest/nuggetd-$target$ext"
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  py="$(command -v python3 || command -v python)"
  "$py" packaging/scripts/clamp_mtimes.py "$dest/nuggetd-$target$ext"
fi
echo "staged $dest/nuggetd-$target$ext"
