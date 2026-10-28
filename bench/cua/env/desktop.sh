#!/usr/bin/env bash
# Bring up a Linux desktop for `bench/cua` (Xvfb, a window manager, a session D-Bus and the
# AT-SPI bus), run the suite on it, and tear everything down.
#
#   bench/cua/env/desktop.sh [nugget-bench args...]
#   bench/cua/env/desktop.sh --cua-agent reference --strict
#   CUA_VNC=1 bench/cua/env/desktop.sh --cua-tasks shop   # watch on vnc://127.0.0.1:5900
#
# Without this script `nugget-bench cua --cua-env xvfb` starts its own Xvfb (and whatever of the
# window manager, D-Bus and AT-SPI it can find). The script exists for full control and for
# keeping a display around: with CUA_HOLD=1 it starts the desktop, prints its DISPLAY and waits.
set -euo pipefail

cd "$(dirname "$0")/../../.."

display_num=${CUA_DISPLAY_NUM:-99}
while [ -e "/tmp/.X11-unix/X${display_num}" ] || [ -e "/tmp/.X${display_num}-lock" ]; do
  display_num=$((display_num + 1))
done
export DISPLAY=":${display_num}"

pids=()
cleanup() {
  for ((i = ${#pids[@]} - 1; i >= 0; i--)); do kill "${pids[$i]}" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

Xvfb "$DISPLAY" -screen 0 1600x1000x24 -nolisten tcp &
pids+=($!)
for _ in $(seq 100); do [ -e "/tmp/.X11-unix/X${display_num}" ] && break; sleep 0.05; done

if command -v dbus-daemon >/dev/null; then
  eval "$(dbus-launch --sh-syntax 2>/dev/null || true)"
  if [ -n "${DBUS_SESSION_BUS_PID:-}" ]; then pids+=("$DBUS_SESSION_BUS_PID"); fi
fi
for launcher in /usr/libexec/at-spi-bus-launcher /usr/lib/at-spi2-core/at-spi-bus-launcher \
  /usr/lib/x86_64-linux-gnu/at-spi-bus-launcher; do
  if [ -x "$launcher" ] && [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    "$launcher" --launch-immediately &
    pids+=($!)
    break
  fi
done
export ACCESSIBILITY_ENABLED=1 NO_AT_BRIDGE=0
for wm in openbox matchbox-window-manager fluxbox icewm; do
  if command -v "$wm" >/dev/null; then "$wm" & pids+=($!); break; fi
done
if [ "${CUA_VNC:-0}" = 1 ] && command -v x11vnc >/dev/null; then
  x11vnc -display "$DISPLAY" -localhost -forever -nopw -quiet &
  pids+=($!)
fi

export NUGGET_CUA_DISPLAY="$DISPLAY"
echo "cua desktop on DISPLAY=$DISPLAY (D-Bus: ${DBUS_SESSION_BUS_ADDRESS:-none})" >&2
if [ "${CUA_HOLD:-0}" = 1 ]; then
  echo "holding; Ctrl-C to stop" >&2
  wait
else
  cargo run --release -p nugget-bench -- cua --cua-env xvfb "$@"
fi
