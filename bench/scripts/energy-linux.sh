#!/usr/bin/env bash
# Run a command inside a private X11 desktop session for `nugget-bench energy` on Linux: Xvfb,
# the openbox window manager (it honours `xdotool windowactivate`), and a private session bus on
# which AT-SPI is D-Bus activated (the shell's read-ahead and the `a11y_poll` scenario read the
# frontmost app through it). Modelled on apps/desktop/e2e-native/run-linux.sh.
#
#   bench/scripts/energy-linux.sh target/release/nugget-bench energy --energy-scenarios all
#
# Needs: xvfb openbox xdotool dbus-x11 at-spi2-core python3-gi gir1.2-gtk-3.0 (and the Tauri
# runtime libraries for the shell). The display defaults to :98 (NUGGET_BENCH_DISPLAY).
set -euo pipefail
if [ "$#" -eq 0 ]; then
  echo "usage: $0 COMMAND [ARGS...]" >&2
  exit 2
fi
display="${NUGGET_BENCH_DISPLAY:-:98}"

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
# The shell's WebKitGTK must not try GPU paths Xvfb lacks.
export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
# GTK apps (the shell, the text windows) expose themselves to AT-SPI.
export GTK_MODULES=gail:atk-bridge NO_AT_BRIDGE=0

# Not `exec`, so the trap stops Xvfb; openbox is stopped with the session.
# shellcheck disable=SC2016 # expanded by the inner shell
dbus-run-session -- bash -c '
  openbox >/dev/null 2>&1 &
  wm=$!
  sleep 1
  # Start the accessibility bus now rather than on the first AT-SPI call.
  gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >/dev/null 2>&1 || true
  "$@"
  status=$?
  kill "$wm" 2>/dev/null
  exit "$status"
' energy-linux "$@"
