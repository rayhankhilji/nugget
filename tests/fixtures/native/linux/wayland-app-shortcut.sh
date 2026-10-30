#!/usr/bin/env bash
# Double Tap's Wayland shortcut in the installed app, end to end. Run inside wayland-session.sh
# (kwin or gnome) after installing Nugget's .deb (a debug build, so NUGGET_E2E_PROBE works):
#
#   NUGGET_WAYLAND_COMPOSITOR=kwin tests/fixtures/native/linux/wayland-session.sh \
#       tests/fixtures/native/linux/wayland-app-shortcut.sh [--out DIR] [--app PATH]
#
# 1. Starts the installed nugget-desktop natively on Wayland with NUGGET_E2E_PROBE=<file>, and
#    waits for its Double Tap listener (probe `listener`): the GlobalShortcuts portal session is
#    created for app id dev.nugget.desktop (the installed desktop file).
# 2. Accepts the desktop's consent dialog for the new shortcut over AT-SPI
#    (accept-shortcut-dialog.py), unless the session pre-seeded the consent
#    (NUGGET_WAYLAND_CONSENT=preseed) and no dialog appears.
# 3. Presses Ctrl+Alt+Space on the compositor's virtual keyboard (virtual-keyboard.py: KWin fake
#    input, Mutter RemoteDesktop) and requires the probe's `trigger` event: the key press went
#    compositor -> desktop shortcut handling -> portal Activated -> Nugget's Double Tap.
#
# A Markdown table row goes to $NUGGET_APP_SUMMARY when set. Exits non-zero when the trigger did
# not fire.
set -uo pipefail

out="${NUGGET_WAYLAND_LOGS:-$(mktemp -d)}/app"
app=""
while [ $# -gt 0 ]; do
    case "$1" in
    --out) out="$2"; shift 2 ;;
    --app) app="$2"; shift 2 ;;
    *) echo "wayland-app-shortcut: unknown argument $1" >&2; exit 2 ;;
    esac
done
compositor="${NUGGET_WAYLAND_COMPOSITOR:?run inside wayland-session.sh}"
keys="${NUGGET_WAYLAND_KEYS:?run inside wayland-session.sh}"
accept="${NUGGET_WAYLAND_ACCEPT_DIALOG:?run inside wayland-session.sh}"
consent_mode="${NUGGET_WAYLAND_CONSENT:-dialog}"
if [ -z "$app" ]; then
    # The binary the installed desktop file launches.
    desktop=/usr/share/applications/dev.nugget.desktop.desktop
    if [ -f "$desktop" ]; then
        app="$(sed -n 's/^Exec=\([^ ]*\).*/\1/p' "$desktop" | head -n 1)"
        [ "${app#/}" = "$app" ] && app="$(command -v "$app")"
    fi
    [ -n "$app" ] || app="$(command -v nugget-desktop)"
fi
[ -x "$app" ] || { echo "wayland-app-shortcut: no installed nugget-desktop ($app)" >&2; exit 1; }
mkdir -p "$out"
probe="$out/probe.jsonl"
rm -f "$probe"
home="$(mktemp -d)"

log() { echo "wayland-app-shortcut: $*"; }
probe_count() {
    if [ -f "$probe" ]; then grep -c "\"event\":\"$1\"" "$probe"; else echo 0; fi
}

registered="?"
session="no"
consent="none"
activation="virtual keyboard ($([ "$compositor" = kwin ] && echo 'KWin org_kde_kwin_fake_input' || echo 'Mutter RemoteDesktop'))"
fired="no"
fire_ms="-"
row() {
    if [ -n "${NUGGET_APP_SUMMARY:-}" ]; then
        echo "| $compositor | $registered | $session | $consent | $activation | $fired | $fire_ms |" >>"$NUGGET_APP_SUMMARY"
    fi
}

log "starting $app (probe $probe)"
NUGGET_HOME="$home" NUGGET_KEYCHAIN=off NUGGET_E2E_PROBE="$probe" \
    NUGGET_LOG="info,nugget_desktop=debug,nugget_hotkey=debug" RUST_LOG="nugget_hotkey=debug" \
    GDK_BACKEND=wayland WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
    "$app" >"$out/nugget-desktop.log" 2>&1 &
app_pid=$!
trap 'kill "$app_pid" 2>/dev/null; sleep 0.5; kill -9 "$app_pid" 2>/dev/null; rm -rf "$home"' EXIT

# 1. The listener (the portal session).
for _ in $(seq 1 450); do
    [ "$(probe_count listener)" -gt 0 ] && break
    if ! kill -0 "$app_pid" 2>/dev/null; then
        log "nugget-desktop exited early"
        tail -n 80 "$out/nugget-desktop.log"
        row
        exit 1
    fi
    sleep 0.2
done
listener="$(grep -m1 '"event":"listener"' "$probe" 2>/dev/null)"
log "probe: ${listener:-no listener event in 90 s}"
if ! echo "$listener" | grep -q '"listening":true'; then
    log "the Double Tap listener did not start"
    tail -n 80 "$out/nugget-desktop.log"
    row
    exit 1
fi
session="created"
# The portal names the app from its host registration (the installed desktop file).
if grep -q "host portal registration failed" "$out/nugget-desktop.log"; then
    registered="failed (see log)"
else
    registered="dev.nugget.desktop"
fi

# 2. Consent.
if python3 "$accept" --timeout 25 --dump >"$out/consent.log" 2>&1; then
    consent="dialog accepted over AT-SPI"
    rc=0
else
    rc=$?
fi
cat "$out/consent.log"
if [ "$rc" = 3 ]; then
    if [ "$consent_mode" = preseed ]; then
        consent="PRE-SEEDED (returning-user kglobalshortcutsrc, no dialog)"
    else
        consent="no dialog appeared"
    fi
elif [ "$rc" != 0 ]; then
    consent="dialog not accepted (rc $rc)"
fi
log "consent: $consent"
sleep 1.5

# 3. The key press.
for attempt in 1 2 3; do
    before="$(probe_count trigger)"
    t0="$(date +%s%N)"
    if ! python3 "$keys" ctrl+alt+space >>"$out/keys.log" 2>&1; then
        log "the virtual keyboard failed:"
        cat "$out/keys.log"
        break
    fi
    for _ in $(seq 1 30); do
        [ "$(probe_count trigger)" -gt "$before" ] && break
        sleep 0.1
    done
    if [ "$(probe_count trigger)" -gt "$before" ]; then
        t1="$(date +%s%N)"
        fired="yes (attempt $attempt)"
        fire_ms="$(((t1 - t0) / 1000000)) (press to probe, upper bound)"
        break
    fi
    log "attempt $attempt: Ctrl+Alt+Space did not fire Double Tap"
    sleep 1
done
cat "$out/keys.log"
log "fired: $fired; probe:"
cat "$probe"
row
case "$fired" in
yes*) exit 0 ;;
*)
    tail -n 60 "$out/nugget-desktop.log"
    exit 1
    ;;
esac
