#!/usr/bin/env bash
# Release rehearsal: everything before the end-to-end tests, with throwaway keys.
#
#   prepare.sh keys              generate the throwaway keys (private parts stay in $KEYS)
#   prepare.sh manifest          append a release signature to manifests/default.json.sig
#                                (job workspace only; never committed)
#   prepare.sh sidecar [--wrong] build and stage nuggetd trusting only the throwaway manifest
#                                key, as a real release does; --wrong first builds a second
#                                nuggetd trusting a different key (for the rejection test)
#   prepare.sh build old|new     build the app at the old or new version with the throwaway
#                                updater key and the localhost feed baked in
#   prepare.sh sign              sign the new build's update payloads and the feed exactly as
#                                release.yml's `sign` and `channels` jobs do, and lay out the
#                                feed variants to serve (good, wrong feed key, wrong payload key),
#                                on the beta channel, where a prerelease goes
#
# Environment: TARGET (default host), BUNDLES (tauri --bundles; default appimage on Linux,
# nsis on Windows), REHEARSAL_DIR (default $RUNNER_TEMP/rehearsal). Run from anywhere.
set -euo pipefail
# shellcheck source=packaging/scripts/rehearsal/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

cd "$REHEARSAL_ROOT"
case "$TARGET" in
  *windows*) : "${BUNDLES:=nsis}" ;;
  *linux*) : "${BUNDLES:=appimage}" ;;
  *) rdie "the rehearsal covers Linux and Windows only, not $TARGET" ;;
esac

sign_manifest() { cargo run --locked -q -p nugget-models --example sign-manifest -- "$@"; }

# Runs the locked Tauri CLI from apps/desktop (pnpm install --frozen-lockfile done by the job).
tauri() { (cd "$REHEARSAL_ROOT/apps/desktop" && pnpm exec tauri "$@"); }

# Sign FILE with the updater key named KEY (updater | updater-wrong): `tauri signer sign`
# with the key in TAURI_SIGNING_PRIVATE_KEY, as release.yml does with its secrets.
updater_sign() { # key file
  local pw key
  pw="$(cat "$KEYS/$1.password")"
  key="$(cat "$KEYS/$1.key")"
  mask "$pw"
  mask "$(tr -d '\r\n' <<<"$key")"
  TAURI_SIGNING_PRIVATE_KEY="$key" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$pw" \
    tauri signer sign "$2" >/dev/null
  test -s "$2.sig" || rdie "tauri signer sign wrote no $2.sig"
}

# Both versions must be marked "-rehearsal", and NEW must be strictly newer than OLD by the same
# SemVer rule the release pipeline uses (updater_manifest.newer).
check_versions() {
  case "$OLD_VERSION/$NEW_VERSION" in
    *-rehearsal*/*-rehearsal*) ;;
    *) rdie "both versions must contain -rehearsal: $OLD_VERSION, $NEW_VERSION" ;;
  esac
  "$PY" -I - "$REHEARSAL_ROOT/packaging/scripts" "$OLD_VERSION" "$NEW_VERSION" <<'PYEOF' ||
import sys
sys.path.insert(0, sys.argv[1])
from updater_manifest import newer
sys.exit(0 if newer(sys.argv[3], sys.argv[2]) else 1)
PYEOF
    rdie "$NEW_VERSION is not newer than $OLD_VERSION"
}

cmd_keys() {
  check_versions
  [ ! -e "$KEYS" ] || rdie "$KEYS already exists; keys are made once per run"
  mkdir -p "$KEYS" "$PUB"
  chmod 700 "$KEYS"
  local name pw out
  for name in updater updater-wrong; do
    pw="$(openssl rand -hex 32)"
    mask "$pw"
    (umask 077 && printf '%s' "$pw" >"$KEYS/$name.password")
    tauri signer generate --ci -w "$KEYS/$name.key" -p "$pw" >/dev/null
    chmod 600 "$KEYS/$name.key"
    cp "$KEYS/$name.key.pub" "$PUB/$name.key.pub"
    rlog "updater key '$name': public key $(cat "$PUB/$name.key.pub")"
  done
  for name in manifest-release manifest-wrong; do
    out="$(sign_manifest --generate-key "$KEYS/$name.key")"
    chmod 600 "$KEYS/$name.key"
    sed -n 's/^public key: //p' <<<"$out" | tr -d '\r\n' >"$PUB/$name.pub"
    [ "$(wc -c <"$PUB/$name.pub")" -eq 64 ] || rdie "no public key from sign-manifest --generate-key"
    rlog "manifest key '$name': public key $(cat "$PUB/$name.pub")"
  done
  record "Throwaway keys generated in the job" PASS \
    "updater + wrong updater (minisign), manifest release + wrong manifest (ed25519); private keys only in the job's temp dir"
}

cmd_manifest() {
  local pub wrong
  pub="$(cat "$PUB/manifest-release.pub")"
  wrong="$(cat "$PUB/manifest-wrong.pub")"
  sign_manifest --append --key-file "$KEYS/manifest-release.key" manifests/default.json
  mkdir -p "$EVIDENCE/manifest"
  cp manifests/default.json.sig "$EVIDENCE/manifest/default.json.sig"
  sha256sum manifests/default.json >"$EVIDENCE/manifest/default.json.sha256"
  if sign_manifest --verify manifests/default.json --pubkey "$pub"; then
    record "Manifest: release signature appended with the throwaway key" PASS \
      "default.json.sig now has $(grep -c . manifests/default.json.sig) signatures; verifies with $pub"
  else
    record "Manifest: release signature appended with the throwaway key" FAIL "does not verify"
  fi
  if sign_manifest --verify manifests/default.json --pubkey "$wrong" 2>/dev/null; then
    record "Manifest: signature does not verify with the wrong key" FAIL "verified with $wrong"
  else
    record "Manifest: signature does not verify with the wrong key" PASS "refused by sign-manifest --verify"
  fi
}

cmd_sidecar() {
  local pub
  pub="$(cat "$PUB/manifest-release.pub")"
  mkdir -p "$REHEARSAL_DIR/bin"
  # A real release: the release key only, never the development key.
  unset NUGGET_TRUST_DEV_MANIFEST_KEY
  if [ "${1:-}" = --wrong ]; then
    # Built first, so the target dir ends with the right build. Only nugget-models and its
    # dependents recompile for a different key.
    NUGGET_MANIFEST_RELEASE_PUBKEY="$(cat "$PUB/manifest-wrong.pub")" \
      cargo build --locked --release -p nuggetd --target "$TARGET"
    cp "target/$TARGET/release/nuggetd$EXE" "$REHEARSAL_DIR/bin/nuggetd-wrong-key$EXE"
  fi
  NUGGET_MANIFEST_RELEASE_PUBKEY="$pub" packaging/scripts/stage-sidecar.sh "$TARGET"
  cp "apps/desktop/src-tauri/binaries/nuggetd-$TARGET$EXE" "$REHEARSAL_DIR/bin/nuggetd-release-key$EXE"
}

set_version() { # version
  "$PY" -I - "$REHEARSAL_ROOT/apps/desktop/src-tauri/tauri.conf.json" "$1" <<'EOF'
import json, sys
path, version = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as f:
    conf = json.load(f)
conf["version"] = version
with open(path, "w", encoding="utf-8") as f:
    json.dump(conf, f, indent=2)
    f.write("\n")
EOF
}

cmd_build() {
  local which="$1" version conf backup out bundle
  check_versions
  case "$which" in
    old) version="$OLD_VERSION" ;;
    new) version="$NEW_VERSION" ;;
    *) rdie "build old|new" ;;
  esac
  conf="apps/desktop/src-tauri/tauri.conf.json"
  backup="$REHEARSAL_DIR/tauri.conf.json.orig"
  [ -f "$backup" ] || cp "$conf" "$backup"
  set_version "$version"
  rlog "building $which ($version, bundles $BUNDLES, feed $FEED_BASE)"
  (
    cd apps/desktop
    export NUGGET_UPDATER_PUBKEY NUGGET_UPDATER_FEED="$FEED_BASE" APPIMAGE_EXTRACT_AND_RUN=1
    NUGGET_UPDATER_PUBKEY="$(cat "$PUB/updater.key.pub")"
    pnpm tauri build --ci --target "$TARGET" --bundles "$BUNDLES" \
      --config src-tauri/tauri.bundle.conf.json
  )
  cp "$backup" "$conf" # the workspace copy only ever changes for the duration of a build
  out="$REHEARSAL_DIR/build/$which"
  bundle="target/$TARGET/release/bundle"
  mkdir -p "$out"
  case "$TARGET" in
    *windows*) cp "$bundle/nsis/Nugget_${version}_x64-setup.exe" "$out/" ;;
    *linux*) cp "$bundle/appimage/Nugget_${version}_amd64.AppImage" "$out/" ;;
  esac
  ls -l "$out"
  record "Build $which installer ($version)" PASS "$(cd "$out" && for f in *; do printf '%s %s; ' "$f" "$(sha256 "$f")"; done)"
}

# Serve root for one feed variant: ROOT/feed/beta.json(.sig) and ROOT/dist/<payloads>. A
# prerelease only ever reaches the beta channel (release.yml `channels`), so that is the feed.
layout() { # variant dist feed.json feed.sig
  local root="$REHEARSAL_DIR/serve/$1"
  rm -rf "$root"
  mkdir -p "$root/feed" "$root/dist"
  local f
  for f in $("$PY" -I packaging/scripts/updater_manifest.py payloads "$2" | tr -d '\r'); do
    ln "$f" "$root/dist/" 2>/dev/null || cp "$f" "$root/dist/"
  done
  cp "$3" "$root/feed/beta.json"
  cp "$4" "$root/feed/beta.json.sig"
  mkdir -p "$EVIDENCE/feeds/$1"
  cp "$3" "$EVIDENCE/feeds/$1/beta.json"
  cp "$4" "$EVIDENCE/feeds/$1/beta.json.sig"
}

cmd_sign() {
  local dist="$REHEARSAL_DIR/dist" notes="$REHEARSAL_DIR/notes.md" f
  rm -rf "$dist"
  mkdir -p "$dist"
  cp "$REHEARSAL_DIR/build/new/"* "$dist/"
  printf 'Nugget %s: RELEASE REHEARSAL, throwaway keys, not a release.\n' "$NEW_VERSION" >"$notes"

  # release.yml `sign`: sign every payload, write latest.json, checksums, sign the feed.
  # (tr: Python on Windows ends lines with CRLF.)
  "$PY" -I packaging/scripts/updater_manifest.py payloads "$dist" | tr -d '\r' >"$REHEARSAL_DIR/payloads.txt"
  [ -s "$REHEARSAL_DIR/payloads.txt" ] || rdie "no updater payloads in $dist"
  while read -r f; do updater_sign updater "$f"; done <"$REHEARSAL_DIR/payloads.txt"
  "$PY" -I packaging/scripts/updater_manifest.py manifest "$dist" \
    --version "v$NEW_VERSION" --notes-file "$notes" \
    --pub-date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --base-url "$DIST_BASE" --out "$dist/latest.json"
  "$PY" -I packaging/scripts/updater_manifest.py checksums "$dist" >/dev/null
  updater_sign updater "$dist/latest.json"
  # release.yml `channels` for a prerelease: beta only. `promote` against the (empty) current
  # feed must accept it; the beta feed is then a byte-for-byte copy of latest.json and its .sig.
  [ "$("$PY" -I packaging/scripts/updater_manifest.py promote "$dist/latest.json" "$REHEARSAL_DIR/no-current-feed.json" | tr -d '\r')" = "$NEW_VERSION" ] ||
    rdie "updater_manifest.py promote refused $NEW_VERSION"
  "$PY" -I packaging/scripts/updater_manifest.py channel "$dist/latest.json" beta >/dev/null
  "$PY" -I packaging/scripts/updater_manifest.py checksums "$dist" >/dev/null
  mkdir -p "$EVIDENCE/release"
  for f in latest.json latest.json.sig beta.json beta.json.sig SHA256SUMS; do cp "$dist/$f" "$EVIDENCE/release/"; done
  for f in "$dist"/*.sig; do cp "$f" "$EVIDENCE/release/"; done
  cat "$dist/SHA256SUMS"

  if "$PY" -I - "$dist/latest.json" "https://localhost:$PORT/" <<'EOF'
import json, sys
m = json.load(open(sys.argv[1], encoding="utf-8"))
urls = [p["url"] for p in m["platforms"].values()]
sys.exit(0 if urls and all(u.startswith(sys.argv[2]) for u in urls) and "rehearsal" in m["version"] else 1)
EOF
  then
    record "Feed is unpublishable (localhost URLs, -rehearsal version)" PASS "version $NEW_VERSION, every payload URL under $DIST_BASE"
  else
    record "Feed is unpublishable (localhost URLs, -rehearsal version)" FAIL "see latest.json"
  fi

  # Variant "good": what `channels` would publish.
  layout good "$dist" "$dist/beta.json" "$dist/beta.json.sig"

  # Variant "wrong-feed-key": the same feed bytes, signed by a key the app does not trust.
  local w="$REHEARSAL_DIR/wrong-feed-key"
  rm -rf "$w"
  mkdir -p "$w"
  cp "$dist/beta.json" "$w/beta.json"
  updater_sign updater-wrong "$w/beta.json"
  layout wrong-feed-key "$dist" "$w/beta.json" "$w/beta.json.sig"

  # Variant "wrong-payload-key": a correctly signed feed whose payload signatures were made
  # with the wrong key (the feed itself verifies; the download must not).
  local p="$REHEARSAL_DIR/wrong-payload-key"
  rm -rf "$p"
  mkdir -p "$p"
  while read -r f; do
    cp "$f" "$p/"
    updater_sign updater-wrong "$p/${f##*[/\\]}"
  done <"$REHEARSAL_DIR/payloads.txt"
  "$PY" -I packaging/scripts/updater_manifest.py manifest "$p" \
    --version "v$NEW_VERSION" --notes-file "$notes" \
    --pub-date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --base-url "$DIST_BASE" --out "$p/latest.json"
  updater_sign updater "$p/latest.json"
  "$PY" -I packaging/scripts/updater_manifest.py channel "$p/latest.json" beta >/dev/null
  layout wrong-payload-key "$p" "$p/beta.json" "$p/beta.json.sig"

  record "Update signed like release.yml (payloads, latest.json + .sig, promote, beta.json + .sig, SHA256SUMS)" PASS \
    "$(wc -l <"$REHEARSAL_DIR/payloads.txt") payload(s); variants: good, wrong-feed-key, wrong-payload-key"
}

case "${1:-}" in
  keys) cmd_keys ;;
  manifest) cmd_manifest ;;
  sidecar) shift; cmd_sidecar "$@" ;;
  build) cmd_build "${2:?build old|new}" ;;
  sign) cmd_sign ;;
  *) echo "usage: $0 keys|manifest|sidecar [--wrong]|build old|new|sign" >&2; exit 2 ;;
esac
