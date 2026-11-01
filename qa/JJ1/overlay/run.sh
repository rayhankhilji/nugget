#!/usr/bin/env bash
# JJ1 overlay QA under X11 (Xvfb + openbox + private session bus), like
# apps/desktop/e2e-native/run-linux.sh, but running overlay_scenarios.mjs. Args go to it.
#   qa/JJ1/overlay/run.sh --out /tmp/ov [--only s1,s4]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
display="${NUGGET_E2E_DISPLAY:-:98}"
Xvfb "$display" -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 100); do [ -e "/tmp/.X11-unix/X${display#:}" ] && break; sleep 0.1; done
export DISPLAY="$display" XDG_SESSION_TYPE=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
unset WAYLAND_DISPLAY
dbus-run-session -- bash -c '
  openbox >/dev/null 2>&1 &
  wm=$!
  sleep 1
  node "$0/overlay_scenarios.mjs" "$@"
  status=$?
  kill "$wm" 2>/dev/null
  exit "$status"
' "$here" "$@"
