#!/usr/bin/env bash
# Runs inside the lab namespace (lab.ns_run): an X server, a session bus and the real desktop app
# (debug build of nugget-desktop) against the lab's daemon, for SECS seconds, then a screenshot.
#   desktop-in-ns.sh APP NUGGET_HOME XDG_CONFIG_HOME OUT_PREFIX SECS
set -uo pipefail
app=$1 home=$2 cfg=$3 out=$4 secs=$5
display=:97
Xvfb "$display" -screen 0 1600x1000x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
sleep 2
export DISPLAY=$display XDG_SESSION_TYPE=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 NUGGET_HOME=$home XDG_CONFIG_HOME=$cfg
export XDG_RUNTIME_DIR=$(mktemp -d) NO_AT_BRIDGE=1 RUST_LOG=info
unset WAYLAND_DISPLAY HTTPS_PROXY HTTP_PROXY https_proxy http_proxy ALL_PROXY
dbus-run-session -- bash -c "
  '$app' > '$out.log' 2>&1 &
  pid=\$!
  sleep $secs
  import -window root '$out.png' 2>/dev/null || true
  kill \$pid; sleep 2; kill -9 \$pid 2>/dev/null
  true"
kill "$xvfb" 2>/dev/null
exit 0
