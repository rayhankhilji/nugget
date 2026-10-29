#!/usr/bin/env bash
# Run the native overlay test on Linux under X11: Xvfb, the openbox window manager (it gives
# focus back to the target when the overlay hides, as desktop window managers do) and a private
# session bus (AT-SPI is D-Bus activated on it). Arguments go to run.mjs.
#
#   apps/desktop/e2e-native/run-linux.sh --app target/debug/nugget-desktop --out /tmp/overlay
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
display="${NUGGET_E2E_DISPLAY:-:99}"

Xvfb "$display" -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 100); do
  [ -e "/tmp/.X11-unix/X${display#:}" ] && break
  sleep 0.1
done
export DISPLAY="$display"
unset WAYLAND_DISPLAY
export XDG_SESSION_TYPE=x11
# The overlay's WebKitGTK must not try GPU paths Xvfb lacks.
export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1

# Not `exec`, so the trap stops Xvfb; openbox is stopped with the session.
dbus-run-session -- bash -c '
  openbox >/dev/null 2>&1 &
  wm=$!
  sleep 1
  node "$0/run.mjs" "$@"
  status=$?
  kill "$wm" 2>/dev/null
  exit "$status"
' "$here" "$@"
