#!/usr/bin/env bash
# Build nuggetd for a target and stage it where Tauri's `externalBin` expects it:
#   apps/desktop/src-tauri/binaries/nuggetd-<target-triple>[.exe]
# Usage: packaging/scripts/stage-sidecar.sh [target-triple]   (default: the host triple)
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
echo "staged $dest/nuggetd-$target$ext"
