#!/usr/bin/env bash
# Markdown report of a bundle build: installer sizes, the two binaries, and where the nuggetd
# sidecar sits inside each installer. Best effort: a listing that fails is reported, not fatal.
# Usage: packaging/scripts/bundle-report.sh <target-triple> <installers-dir> <tauri-bundle-dir>
set -uo pipefail

target="$1"
out="$2"
bundle="$3"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
case "$target" in
  *windows*) ext=".exe" ;;
  *) ext="" ;;
esac
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

size_of() { wc -c < "$1" | tr -d ' '; }
mib() { awk -v b="$1" 'BEGIN { printf "%.1f MiB", b / 1048576 }'; }
listing() {
  echo
  echo "<details><summary>$1</summary>"
  echo
  echo '```'
  shift
  "$@" 2>&1 || echo "(listing failed)"
  echo '```'
  echo "</details>"
}

echo "## Bundles for $target"
echo
echo "| File | Size | Bytes |"
echo "|---|---|---|"
for f in "$out"/*; do
  [ -f "$f" ] || continue
  b=$(size_of "$f")
  echo "| \`$(basename "$f")\` | $(mib "$b") | $b |"
done
for bin in "nugget-desktop$ext" "nuggetd$ext"; do
  f="$root/target/$target/release/$bin"
  if [ -f "$f" ]; then
    b=$(size_of "$f")
    echo "| binary \`$bin\` (uncompressed) | $(mib "$b") | $b |"
  fi
done

echo
echo "### Sidecar layout"
case "$target" in
  *apple-darwin)
    app="$bundle/macos/Nugget.app"
    echo
    echo "\`Nugget.app\`: $(du -sk "$app" | awk '{ printf "%.1f MiB", $1 / 1024 }') on disk"
    listing "Nugget.app/Contents/MacOS" ls -l "$app/Contents/MacOS"
    listing "Nugget.app (all files)" find "$app" -type f
    listing "codesign (ad-hoc on a dry run)" codesign -dv "$app"
    ;;
  *windows*)
    for f in "$out"/*-setup.exe; do
      [ -f "$f" ] && listing "NSIS: $(basename "$f")" 7z l "$f"
    done
    for f in "$out"/*.msi; do
      [ -f "$f" ] || continue
      dest="$(cygpath -w "$scratch/msi" 2>/dev/null || echo "$scratch/msi")"
      msi="$(cygpath -w "$f" 2>/dev/null || echo "$f")"
      # An administrative install unpacks the MSI's files without installing anything.
      pwsh -NoProfile -Command "Start-Process msiexec -Wait -ArgumentList '/a', '\"$msi\"', '/qn', 'TARGETDIR=\"$dest\"'" >/dev/null 2>&1
      listing "MSI: $(basename "$f") (administrative install)" find "$scratch/msi" -type f -not -name '*.msi'
    done
    ;;
  *linux*)
    for f in "$out"/*.deb; do
      [ -f "$f" ] && listing "deb: $(basename "$f")" dpkg-deb -c "$f"
    done
    for f in "$out"/*.rpm; do
      [ -f "$f" ] && listing "rpm: $(basename "$f")" rpm -qplv "$f"
    done
    for f in "$out"/*.AppImage; do
      [ -f "$f" ] || continue
      chmod +x "$f"
      (cd "$scratch" && "$f" --appimage-extract >/dev/null 2>&1)
      listing "AppImage: $(basename "$f") usr/bin" ls -l "$scratch/squashfs-root/usr/bin"
      listing "AppImage: $(basename "$f") (all files)" find "$scratch/squashfs-root" -type f
    done
    ;;
esac
