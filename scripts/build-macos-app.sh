#!/usr/bin/env bash
# Build Nugget.app on this Mac from source, in one command. Works on Intel and Apple Silicon.
#
#   scripts/build-macos-app.sh            build for this Mac (x86_64 or arm64)
#   scripts/build-macos-app.sh --target aarch64-apple-darwin   build for another Mac
#
# What it does:
#   1. builds nuggetd and stages it as the app's sidecar (packaging/scripts/stage-sidecar.sh);
#   2. builds the frontend and the app with the frontend embedded (`pnpm tauri build`, only the
#      .app bundle), unsigned: no Developer ID, no notarization, no updater key;
#   3. signs the bundle ad hoc (`codesign --force --deep -s -`), so macOS has a stable app to
#      attach Accessibility, Input Monitoring and Full Disk Access to (Nugget.app, not Terminal);
#   4. prints where Nugget.app is and how to move it to /Applications.
#
# Local builds trust the repository's development model-manifest key, so managed model downloads
# work (NUGGET_TRUST_DEV_MANIFEST_KEY=1; set it to 0 to build without). Release builds never do
# that: .github/workflows/release.yml builds its own bundles and does not use this script.
set -euo pipefail

die() {
  echo "build-macos-app: $*" >&2
  exit 1
}

usage() {
  echo "usage: $0 [--target x86_64-apple-darwin|aarch64-apple-darwin]" >&2
  exit 2
}

[ "$(uname -s)" = Darwin ] || die "this script builds the macOS app and runs on macOS only"

target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      [ $# -ge 2 ] || usage
      target="$2"
      shift 2
      ;;
    --target=*)
      target="${1#--target=}"
      shift
      ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

if [ -z "$target" ]; then
  case "$(uname -m)" in
    x86_64) target="x86_64-apple-darwin" ;;
    arm64) target="aarch64-apple-darwin" ;;
    *) die "unknown Mac architecture $(uname -m); pass --target" ;;
  esac
fi
case "$target" in
  x86_64-apple-darwin | aarch64-apple-darwin) ;;
  *) die "unsupported target $target (x86_64-apple-darwin or aarch64-apple-darwin)" ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# ---- tools ------------------------------------------------------------------------------------

xcode-select -p >/dev/null 2>&1 || die "no Xcode command line tools: run xcode-select --install"
command -v cargo >/dev/null 2>&1 || die "no cargo: install Rust from https://rustup.rs"
command -v node >/dev/null 2>&1 || die "no node: install Node.js 22 or newer"
command -v pnpm >/dev/null 2>&1 ||
  die "no pnpm: run npm install -g pnpm@10.28.0 (Node 25 and newer no longer ship corepack)"
command -v codesign >/dev/null 2>&1 || die "no codesign: run xcode-select --install"

if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "$target"; then
  echo "build-macos-app: adding the Rust target $target"
  rustup target add "$target"
fi

export NUGGET_TRUST_DEV_MANIFEST_KEY="${NUGGET_TRUST_DEV_MANIFEST_KEY:-1}"
if [ "$NUGGET_TRUST_DEV_MANIFEST_KEY" = 0 ]; then unset NUGGET_TRUST_DEV_MANIFEST_KEY; fi

# ---- build ------------------------------------------------------------------------------------

echo "build-macos-app: building nuggetd for $target"
packaging/scripts/stage-sidecar.sh "$target"

work="$(mktemp -d "${TMPDIR:-/tmp}/nugget-build.XXXXXX")"
trap 'rm -rf "$work"' EXIT
# Ad-hoc signing while bundling, as release.yml's dry runs do without an Apple certificate.
echo '{"bundle":{"macOS":{"signingIdentity":"-"}}}' >"$work/signing.json"

echo "build-macos-app: building Nugget.app"
(
  cd apps/desktop
  pnpm install --frozen-lockfile
  pnpm tauri build --target "$target" --bundles app \
    --config src-tauri/tauri.bundle.conf.json --config "$work/signing.json"
)

app="$root/target/$target/release/bundle/macos/Nugget.app"
[ -d "$app" ] || die "no Nugget.app at $app"
[ -x "$app/Contents/MacOS/nuggetd" ] || die "Nugget.app has no nuggetd sidecar"

# ---- sign -------------------------------------------------------------------------------------

# Ad hoc: no Apple Developer ID needed. One signature over the app and the sidecar inside it, so
# macOS attributes the permissions to Nugget.app.
echo "build-macos-app: signing Nugget.app ad hoc"
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app" || die "codesign does not verify $app"

cat <<EOF

Built: $app

Install it (replaces an older copy in Applications):

  rm -rf /Applications/Nugget.app
  ditto "$app" /Applications/Nugget.app
  open /Applications/Nugget.app

Open Nugget from Applications (or Launchpad, Spotlight), not from Terminal: an app started from
Terminal gets its privacy permissions under Terminal's name. Nugget shows what it needs and opens
the right System Settings page (Accessibility, Input Monitoring, and Full Disk Access if you add
a Messages source).

Each build is signed afresh, so after a rebuild macOS may show Nugget as on but not let it in.
Reset its permissions, open Nugget again and turn them back on:

  tccutil reset Accessibility dev.nugget.desktop
  tccutil reset ListenEvent dev.nugget.desktop
  tccutil reset SystemPolicyAllFiles dev.nugget.desktop
EOF
