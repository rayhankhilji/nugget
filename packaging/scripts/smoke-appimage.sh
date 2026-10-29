#!/usr/bin/env bash
# Smoke-test a Linux AppImage: launch it under Xvfb (with openbox, so the window can be closed the
# way a user would), check that the main window appears and that the app connects to the nuggetd
# sidecar bundled next to it, then quit through the window manager and stop the daemon.
# Usage: packaging/scripts/smoke-appimage.sh path/to/Nugget.AppImage
#
# Needs: Xvfb, openbox, wmctrl, dbus-run-session (imagemagick's `import` and xdotool optional).
# Everything runs against throwaway HOME/XDG/NUGGET_HOME directories, never the user's own data.
#
# Environment:
#   SMOKE_ARTIFACTS          copy the logs (and a screenshot) here, pass or fail
#   SMOKE_CONNECT_TIMEOUT    seconds to wait for "connected to nuggetd" (default 90)
#   SMOKE_SPAWN_GRACE        seconds to wait for the app to start nuggetd itself (default 30)
set -euo pipefail

usage() { echo "usage: $0 path/to/Nugget.AppImage" >&2; exit 2; }
[ $# -eq 1 ] || usage
appimage="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[ -f "$appimage" ] || { echo "smoke: no such file: $1" >&2; exit 2; }
chmod +x "$appimage"

for tool in Xvfb openbox wmctrl dbus-run-session; do
  command -v "$tool" >/dev/null || { echo "smoke: missing required tool: $tool" >&2; exit 2; }
done

connect_timeout="${SMOKE_CONNECT_TIMEOUT:-90}"
spawn_grace="${SMOKE_SPAWN_GRACE:-30}"
window_timeout=60
quit_timeout=30
daemon_stop_timeout=15

# ---- isolated environment -------------------------------------------------------------------

work="$(mktemp -d "${TMPDIR:-/tmp}/nugget-smoke.XXXXXX")"
export NUGGET_HOME="$work/nugget-home"
export HOME="$work/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_RUNTIME_DIR="$work/runtime"
mkdir -p "$NUGGET_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" \
  "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS

export NUGGET_TRAY=off # without a tray, closing the main window quits the app
export NUGGET_LOG=info
export NO_COLOR=1
export APPIMAGE_EXTRACT_AND_RUN=1 # no FUSE on CI runners
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export LIBGL_ALWAYS_SOFTWARE=1
export GDK_BACKEND=x11

app_log="$work/app.log"
xvfb_log="$work/xvfb.log"
wm_log="$work/openbox.log"
daemon_log_dir="$NUGGET_HOME/data/logs" # crates/nuggetd/src/logging.rs: <data>/logs/nuggetd.<date>.log
screenshot="$work/screenshot.png"

xvfb_pid="" wm_pid="" app_pid="" daemon_pid="" display=""
app_alive_required=0
started_by="app"
diagnosed=0

ms() { date +%s%3N; }
t0=$(ms)
elapsed() { awk -v a="$1" -v b="$(ms)" 'BEGIN { printf "%.1f", (b - a) / 1000 }'; }
log() { echo "smoke [$(elapsed "$t0")s] $*"; }

# Daily files named nuggetd.<YYYY-MM-DD>.log sort by date; the newest is the last.
daemon_log_file() {
  local f last=""
  for f in "$daemon_log_dir"/nuggetd*.log; do [ -f "$f" ] && last="$f"; done
  echo "$last"
}

diagnostics() {
  [ "$diagnosed" = 0 ] || return 0
  diagnosed=1
  echo "================ app log ($app_log) ================"
  cat "$app_log" 2>/dev/null || echo "(none)"
  echo "================ daemon log ================"
  local f
  f="$(daemon_log_file)"
  if [ -n "$f" ]; then cat "$f"; else echo "(none under $daemon_log_dir)"; fi
  echo "================ wmctrl -l ================"
  if [ -n "$display" ]; then DISPLAY="$display" wmctrl -l 2>&1 || true; fi
  echo "================ processes ================"
  pgrep -a -f 'nugget|Nugget|AppRun|WebKit' || true
  echo "================ Xvfb log ================"
  cat "$xvfb_log" 2>/dev/null || true
}

fail() {
  echo "smoke: FAIL: $*" >&2
  [ -z "${GITHUB_ACTIONS:-}" ] || echo "::error::AppImage smoke test: $*"
  diagnostics
  exit 1
}

stop_pid() { # pid [signal]
  local pid="$1" sig="${2:-TERM}"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 0
  kill "-$sig" "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.25; done
  kill -KILL "$pid" 2>/dev/null || true
}

cleanup() {
  local status=$?
  set +e
  [ "$status" = 0 ] || diagnostics
  # The app runs in its own process group (setsid), the daemon in another (it detaches itself).
  if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
    kill -TERM -- "-$app_pid" 2>/dev/null
    sleep 1
    kill -KILL -- "-$app_pid" 2>/dev/null
  fi
  stop_pid "$daemon_pid"
  for pid in $(find_daemons); do stop_pid "$pid"; done
  stop_pid "$wm_pid"
  stop_pid "$xvfb_pid"
  if [ -n "${SMOKE_ARTIFACTS:-}" ]; then
    mkdir -p "$SMOKE_ARTIFACTS"
    cp "$app_log" "$xvfb_log" "$wm_log" "$SMOKE_ARTIFACTS/" 2>/dev/null
    [ -d "$daemon_log_dir" ] && cp "$daemon_log_dir"/nuggetd*.log "$SMOKE_ARTIFACTS/" 2>/dev/null
    [ -f "$screenshot" ] && cp "$screenshot" "$SMOKE_ARTIFACTS/"
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Poll `cmd` every 250 ms for up to `timeout` seconds. While the app is expected to be running,
# stop early if it exits.
wait_until() { # timeout description cmd...
  local timeout="$1" what="$2" deadline
  shift 2
  deadline=$(($(ms) + timeout * 1000))
  while ! "$@"; do
    if [ "$app_alive_required" = 1 ] && ! kill -0 "$app_pid" 2>/dev/null; then
      local st=0
      wait "$app_pid" || st=$?
      app_pid=""
      fail "the app exited (status $st) while waiting for: $what"
    fi
    [ "$(ms)" -lt "$deadline" ] || return 1
    sleep 0.25
  done
}

# nuggetd processes that belong to this run (their NUGGET_HOME is ours).
find_daemons() {
  local pid
  for pid in $(pgrep -x nuggetd 2>/dev/null || true); do
    if tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | grep -qxF "NUGGET_HOME=$NUGGET_HOME"; then
      echo "$pid"
    fi
  done
}

# Window id of a top-level window titled exactly "Nugget".
main_window() {
  DISPLAY="$display" wmctrl -l 2>/dev/null |
    awk '{ id = $1; $1 = $2 = $3 = ""; if (substr($0, 4) == "Nugget") { print id; exit } }'
}

have_window() { [ -n "$(main_window)" ]; }
app_logged() { grep -qF "$1" "$app_log" 2>/dev/null; }
daemon_running() { daemon_pid="$(find_daemons | head -n1)"; [ -n "$daemon_pid" ]; }
daemon_logged() { local f; f="$(daemon_log_file)"; [ -n "$f" ] && grep -qF "$1" "$f"; }
app_exited() { ! kill -0 "$app_pid" 2>/dev/null; }
daemon_exited() { ! kill -0 "$daemon_pid" 2>/dev/null; }

# ---- X server and window manager -------------------------------------------------------------

log "AppImage: $appimage ($(du -h "$appimage" | cut -f1))"
# -displayfd picks a free display number and reports it, so parallel runs cannot collide.
Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp 3>"$work/display" >"$xvfb_log" 2>&1 &
xvfb_pid=$!
wait_until 15 "Xvfb" test -s "$work/display" || fail "Xvfb did not start"
display=":$(tr -dc '0-9' <"$work/display")"
export DISPLAY="$display"
log "Xvfb on $DISPLAY"

openbox --sm-disable >"$wm_log" 2>&1 &
wm_pid=$!
wait_until 15 "openbox" wmctrl -m >/dev/null 2>&1 || fail "openbox did not start"
log "window manager: $(wmctrl -m | sed -n 's/^Name: //p')"

# ---- launch ---------------------------------------------------------------------------------

# Own process group (setsid), so cleanup can take down the AppImage runtime and everything it
# started; the session bus is for tauri-plugin-single-instance.
t_launch=$(ms)
setsid dbus-run-session -- "$appimage" >"$app_log" 2>&1 &
app_pid=$!
app_alive_required=1
log "launched (pid $app_pid)"

wait_until "$window_timeout" "the main window" have_window ||
  fail "no window titled 'Nugget' within ${window_timeout}s"
t_window=$(elapsed "$t_launch")
window_id="$(main_window)"
log "window 'Nugget' is up ($window_id) after ${t_window}s"

# ---- daemon: spawned by the app, connected ---------------------------------------------------

if wait_until "$spawn_grace" "the app to start nuggetd" app_logged "starting nuggetd"; then
  log "the app is starting nuggetd: $(grep -m1 -F 'starting nuggetd' "$app_log" | sed 's/^.*exe=//')"
elif daemon_running; then
  started_by="already running"
else
  fail "the app did not log 'starting nuggetd' within ${spawn_grace}s"
fi

wait_until "$connect_timeout" "connected to nuggetd" app_logged "connected to nuggetd" ||
  fail "the app did not log 'connected to nuggetd' within ${connect_timeout}s"
t_connect=$(elapsed "$t_launch")
log "app connected to nuggetd after ${t_connect}s"

wait_until 15 "a nuggetd process" daemon_running || fail "no nuggetd process with our NUGGET_HOME"
log "nuggetd running (pid $daemon_pid): $(tr '\0' ' ' <"/proc/$daemon_pid/cmdline")"

# Logged once the daemon listens (crates/nuggetd/src/daemon.rs).
wait_until 15 "the daemon's ready line" daemon_logged '"nuggetd ready"' ||
  fail "no 'nuggetd ready' line in the daemon log under $daemon_log_dir"
log "daemon log: $(daemon_log_file)"

if command -v import >/dev/null; then
  if import -display "$DISPLAY" -window root "$screenshot" 2>/dev/null; then
    log "screenshot taken"
  else
    log "screenshot failed (ignored)"
  fi
fi

# ---- quit -----------------------------------------------------------------------------------

# A WM_DELETE_WINDOW close, as clicking the title bar's close button would send.
t_quit=$(ms)
wmctrl -i -c "$window_id" || fail "wmctrl could not close the window"
app_alive_required=0
wait_until "$quit_timeout" "the app to exit" app_exited ||
  fail "the app did not exit within ${quit_timeout}s of closing its window"
app_status=0
wait "$app_pid" || app_status=$?
app_pid=""
[ "$app_status" = 0 ] || fail "the app exited with status $app_status"
t_exit=$(elapsed "$t_quit")
log "app exited cleanly (status 0) ${t_exit}s after the close"

# The daemon is a background service and outlives the app by design.
kill -0 "$daemon_pid" 2>/dev/null || fail "nuggetd exited with the app; it should keep running"
t_term=$(ms)
kill -TERM "$daemon_pid"
wait_until "$daemon_stop_timeout" "nuggetd to stop" daemon_exited ||
  fail "nuggetd did not exit within ${daemon_stop_timeout}s of SIGTERM"
t_stop=$(elapsed "$t_term")
daemon_pid=""
# Buffered log lines are flushed on exit; "stopped" is the last line of a clean shutdown.
daemon_logged '"stopped"' || fail "nuggetd exited without logging 'stopped' (unclean shutdown?)"
log "nuggetd stopped cleanly ${t_stop}s after SIGTERM"

cat <<EOF

PASS: AppImage smoke test ($(basename "$appimage"))
  window 'Nugget' shown      ${t_window}s after launch
  connected to nuggetd       ${t_connect}s after launch (daemon started by: ${started_by})
  app exit after close       ${t_exit}s (status 0)
  nuggetd stop after TERM    ${t_stop}s
  total                      $(elapsed "$t0")s
EOF
