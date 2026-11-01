#!/usr/bin/env bash
# JJ1 II4: tray fallback under Xvfb (no StatusNotifierItem host) and NUGGET_TRAY handling.
# Each case gets its own Xvfb + openbox + session bus and a temp NUGGET_HOME / XDG dirs.
#   qa/JJ1/platform/tray.sh [app]   -> TSV on stdout: case, alive_after_12s, main_window_visible, log line, extra
set -uo pipefail
APP="${1:-$(cd "$(dirname "$0")/../../.." && pwd)/target/debug/nugget-desktop}"
run_case() { # name, env assignments..., -- , args...
  local name="$1"; shift
  local envs=(); while [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
  local home; home="$(mktemp -d)"
  local disp=":9$((RANDOM % 5 + 1))"
  Xvfb "$disp" -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 & local xv=$!
  sleep 0.8
  DISPLAY="$disp" XDG_SESSION_TYPE=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
  NUGGET_HOME="$home" XDG_CONFIG_HOME="$home/xdg-config" XDG_DATA_HOME="$home/xdg-data" NUGGET_KEYCHAIN=off \
  dbus-run-session -- env "${envs[@]}" bash -c '
    openbox >/dev/null 2>&1 & sleep 0.8
    "$0" "$@" >"$NUGGET_HOME/app.log" 2>&1 & app=$!
    sleep 12
    alive=$(kill -0 $app 2>/dev/null && echo yes || echo no)
    vis=$(xdotool search --onlyvisible --name "^Nugget$" 2>/dev/null | head -1)
    extra=""
    if [ "${QA_CLOSE:-}" = 1 ] && [ -n "$vis" ]; then
      xdotool windowactivate --sync "$vis" key --clearmodifiers alt+F4; sleep 4
      extra="after_close_alive=$(kill -0 $app 2>/dev/null && echo yes || echo no)"
    fi
    line=$(grep -ao "no tray icon[^\"]*\|tray icon unavailable[^\"]*\|reason[^,]*" "$NUGGET_HOME/app.log" | head -2 | tr "\n" " ")
    panic=$(grep -c "panicked" "$NUGGET_HOME/app.log")
    printf "%s\talive=%s\twindow=%s\tpanics=%s\t%s\t%s\n" "$QA_NAME" "$alive" "${vis:+visible}" "$panic" "$line" "$extra"
    kill $app 2>/dev/null; sleep 1; kill -9 $app 2>/dev/null
    pkill -f "nuggetd --home $NUGGET_HOME" 2>/dev/null
  ' "$APP" "$@"
  kill $xv 2>/dev/null; wait $xv 2>/dev/null
  rm -rf "$home"
}
run_case default QA_NAME=default -- 
run_case tray_on QA_NAME=NUGGET_TRAY=on NUGGET_TRAY=on --
run_case tray_off_min QA_NAME="NUGGET_TRAY=off --minimized" NUGGET_TRAY=off -- --minimized
run_case default_min QA_NAME="no host, --minimized" -- --minimized
run_case close QA_NAME="no host, close window" QA_CLOSE=1 --
