#!/usr/bin/env bash
# Regenerate the desktop icon set from packaging/icons/nugget.svg with the Tauri CLI pinned in
# apps/desktop. Only the desktop sizes are kept (no mobile or Store tiles).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

cd "$root/apps/desktop"
pnpm exec tauri icon "$root/packaging/icons/nugget.svg" --output "$out" >/dev/null
icons="$root/apps/desktop/src-tauri/icons"
for f in 32x32.png 64x64.png 128x128.png 128x128@2x.png icon.png icon.icns icon.ico; do
  cp "$out/$f" "$icons/$f"
done
cp "$root/packaging/icons/nugget.svg" "$icons/icon.svg"
echo "icons written to $icons"
