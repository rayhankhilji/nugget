#!/usr/bin/env bash
# Smoke-test a macOS .dmg: mount it, copy Nugget.app out, launch it, check that the main window
# appears and that the app connects to the nuggetd sidecar bundled next to it, then close the
# window the way a user would (its close button) and stop the daemon.
# Usage: packaging/scripts/smoke-macos.sh path/to/Nugget.dmg
#
# Runs on a logged-in macOS session (GitHub's macos-* runners have one). Everything runs against
# throwaway HOME/NUGGET_HOME directories, never the user's own data.
#
# Window title and close button: the window list (CGWindowListCopyWindowInfo, read through
# osascript's JavaScript/ObjC bridge) gives a window's title only to processes with the Screen
# Recording permission; System Events (UI scripting) needs the Accessibility permission. The
# script tries both, logs which one answered, and fails with the reason when neither can see the
# title or press the close button.
#
# Environment:
#   SMOKE_ARTIFACTS          copy the logs (and a screenshot) here, pass or fail
#   SMOKE_CONNECT_TIMEOUT    seconds to wait for "connected to nuggetd" (default 90)
#   SMOKE_SPAWN_GRACE        seconds to wait for the app to start nuggetd itself (default 30)
set -euo pipefail

usage() { echo "usage: $0 path/to/Nugget.dmg" >&2; exit 2; }
[ $# -eq 1 ] || usage
dmg="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[ -f "$dmg" ] || { echo "smoke: no such file: $1" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo "smoke: macOS only" >&2; exit 2; }

connect_timeout="${SMOKE_CONNECT_TIMEOUT:-90}"
spawn_grace="${SMOKE_SPAWN_GRACE:-30}"
window_timeout=60
quit_timeout=30
daemon_stop_timeout=15

# ---- isolated environment -------------------------------------------------------------------

# Under /tmp, not $TMPDIR (/var/folders/...): the daemon's socket lives under NUGGET_HOME and a
# Unix socket path must fit in 104 bytes on macOS.
work="$(mktemp -d /tmp/nugget-smoke.XXXXXX)"
work="$(cd "$work" && pwd -P)"
export NUGGET_HOME="$work/nh"
export HOME="$work/home"
mkdir -p "$NUGGET_HOME" "$HOME"
unset XDG_RUNTIME_DIR

export NUGGET_TRAY=off # without a tray, closing the main window quits the app
export NUGGET_LOG=info
export NO_COLOR=1

mnt="$work/mnt"
app="$work/Nugget.app"
app_log="$work/app.log"
probe_log="$work/window-probe.log"
daemon_log_dir="$NUGGET_HOME/data/logs" # crates/nuggetd/src/logging.rs: <data>/logs/nuggetd.<date>.log
screenshot="$work/screenshot.png"
cg_script="$work/windows.js"

app_pid="" daemon_pid="" daemon_exe="" mounted=0
app_alive_required=0
started_by="app"
title_source=""
diagnosed=0

ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'; }
t0=$(ms)
elapsed() { awk -v a="$1" -v b="$(ms)" 'BEGIN { printf "%.1f", (b - a) / 1000 }'; }
log() { echo "smoke [$(elapsed "$t0")s] $*"; }

# Run a command with a time limit (macOS has no `timeout`): a permission prompt must not hang us.
bounded() { # seconds cmd...
  perl -e 'alarm shift; exec @ARGV or die "exec: $!"' "$@"
}

# Daily files named nuggetd.<YYYY-MM-DD>.log sort by date; the newest is the last.
daemon_log_file() {
  local f last=""
  for f in "$daemon_log_dir"/nuggetd*.log; do [ -f "$f" ] && last="$f"; done
  echo "$last"
}

# Every window of `pid` from the window server: number, layer, on screen, title (or <no title>).
cg_windows() { # pid
  bounded 15 osascript -l JavaScript "$cg_script" "$1" 2>&1
}

# Window titles of `pid` through System Events (needs the Accessibility permission).
se_windows() { # pid
  bounded 15 osascript -e "tell application \"System Events\" to get name of every window of (first process whose unix id is $1)" 2>&1
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
  echo "================ window probes ================"
  cat "$probe_log" 2>/dev/null || true
  if [ -n "$app_pid" ]; then
    echo "--- window server, pid $app_pid"
    cg_windows "$app_pid" || true
    echo "--- System Events, pid $app_pid"
    se_windows "$app_pid" || true
  fi
  echo "================ processes ================"
  ps -axo pid,ppid,pgid,stat,command | grep -E '[N]ugget|[n]uggetd|[W]ebKit' || true
}

fail() {
  echo "smoke: FAIL: $*" >&2
  [ -z "${GITHUB_ACTIONS:-}" ] || echo "::error::macOS dmg smoke test: $*"
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
  stop_pid "$app_pid"
  stop_pid "$daemon_pid"
  for pid in $(find_daemons); do stop_pid "$pid"; done
  [ "$mounted" = 0 ] || hdiutil detach -force "$mnt" >/dev/null 2>&1
  if [ -n "${SMOKE_ARTIFACTS:-}" ]; then
    mkdir -p "$SMOKE_ARTIFACTS"
    cp "$app_log" "$probe_log" "$SMOKE_ARTIFACTS/" 2>/dev/null
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

# nuggetd processes that belong to this run: the app starts the sidecar by its absolute path,
# which is inside this run's copy of Nugget.app.
find_daemons() {
  [ -n "$daemon_exe" ] || return 0
  ps -axo pid=,command= | awk -v exe="$daemon_exe" '{ pid = $1; $1 = ""; if (substr($0, 2) == exe) print pid }'
}

# A layer-0 (normal) window of the app titled exactly "Nugget", seen by either source.
have_window() {
  local cg se
  cg="$(cg_windows "$app_pid" || true)"
  if printf '%s\n' "$cg" | awk -F'\t' '$2 == 0 && $3 == 1 && $4 == "Nugget" { found = 1 } END { exit !found }'; then
    title_source="window server (CGWindowList)"
    return 0
  fi
  se="$(se_windows "$app_pid" || true)"
  if printf '%s\n' "$se" | tr ',' '\n' | sed 's/^ *//' | grep -qxF Nugget; then
    title_source="System Events (accessibility)"
    return 0
  fi
  { echo "[$(elapsed "$t0")s] window server:"; echo "$cg"; echo "[$(elapsed "$t0")s] System Events:"; echo "$se"; } >>"$probe_log"
  return 1
}

app_logged() { grep -qF "$1" "$app_log" 2>/dev/null; }
daemon_running() { daemon_pid="$(find_daemons | head -n1)"; [ -n "$daemon_pid" ]; }
daemon_logged() { local f; f="$(daemon_log_file)"; [ -n "$f" ] && grep -qF "$1" "$f"; }
app_exited() { ! kill -0 "$app_pid" 2>/dev/null; }
daemon_exited() { ! kill -0 "$daemon_pid" 2>/dev/null; }

cat >"$cg_script" <<'EOF'
ObjC.import('CoreGraphics');
function run(argv) {
  const pid = parseInt(argv[0], 10);
  // kCGWindowListOptionAll (0), kCGNullWindowID (0)
  const all = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo(0, 0))) || [];
  return all
    .filter((w) => w.kCGWindowOwnerPID === pid)
    .map((w) => [w.kCGWindowNumber, w.kCGWindowLayer, w.kCGWindowIsOnscreen ? 1 : 0,
      w.kCGWindowName === undefined ? '<no title>' : w.kCGWindowName].join('\t'))
    .join('\n');
}
EOF

# ---- copy the app out of the disk image ------------------------------------------------------

log "dmg: $dmg ($(du -h "$dmg" | cut -f1))"
t_mount=$(ms)
mkdir -p "$mnt"
hdiutil attach -nobrowse -noautoopen -readonly -mountpoint "$mnt" "$dmg" >/dev/null ||
  fail "hdiutil could not attach $dmg"
mounted=1
[ -d "$mnt/Nugget.app" ] || { ls -la "$mnt"; fail "no Nugget.app at the root of the disk image"; }
[ -L "$mnt/Applications" ] || log "note: the disk image has no Applications link"
ditto "$mnt/Nugget.app" "$app" || fail "could not copy Nugget.app out of the disk image"
hdiutil detach "$mnt" >/dev/null || fail "hdiutil could not detach $mnt"
mounted=0
t_copy=$(elapsed "$t_mount")
log "copied Nugget.app out of the disk image in ${t_copy}s ($(du -sh "$app" | cut -f1))"

exe_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")" ||
  fail "Info.plist has no CFBundleExecutable"
exe="$app/Contents/MacOS/$exe_name"
daemon_exe="$app/Contents/MacOS/nuggetd"
[ -x "$exe" ] || fail "no executable $exe"
[ -x "$daemon_exe" ] || fail "no nuggetd sidecar at $daemon_exe"
codesign --verify --deep --strict "$app" 2>&1 || fail "codesign does not verify Nugget.app"
log "app: $exe_name; signature: $(codesign -dv "$app" 2>&1 | sed -n 's/^Signature=//p')"

# ---- launch ---------------------------------------------------------------------------------

# The binary inside the bundle, not `open`, so its stdout/stderr (the app's log) land in a file
# and its exit status is ours to collect. It is still a regular GUI app with a Dock icon.
t_launch=$(ms)
"$exe" >"$app_log" 2>&1 &
app_pid=$!
app_alive_required=1
log "launched (pid $app_pid)"

wait_until "$window_timeout" "the main window" have_window ||
  fail "no window titled 'Nugget' within ${window_timeout}s (see the window probes: a title the window server reports as <no title> needs Screen Recording; System Events needs Accessibility)"
t_window=$(elapsed "$t_launch")
log "window 'Nugget' is up after ${t_window}s (title read from the $title_source)"

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

wait_until 15 "a nuggetd process" daemon_running || fail "no nuggetd process started from $daemon_exe"
log "nuggetd running (pid $daemon_pid, parent $(ps -o ppid= -p "$daemon_pid" | tr -d ' '))"

# Logged once the daemon listens (crates/nuggetd/src/daemon.rs).
wait_until 15 "the daemon's ready line" daemon_logged '"nuggetd ready"' ||
  fail "no 'nuggetd ready' line in the daemon log under $daemon_log_dir"
log "daemon log: $(daemon_log_file)"

# Needs Screen Recording; without it the capture shows only the desktop. Best effort.
if screencapture -x "$screenshot" 2>/dev/null; then
  log "screenshot taken"
else
  log "screenshot failed (ignored)"
fi

# ---- quit -----------------------------------------------------------------------------------

# The window's red close button, pressed through accessibility, as a user's click would.
t_quit=$(ms)
close_out="$(bounded 20 osascript -e "tell application \"System Events\" to tell (first process whose unix id is $app_pid)
  click (first button of window \"Nugget\" whose subrole is \"AXCloseButton\")
end tell" 2>&1)" ||
  fail "could not press the window's close button through System Events: $close_out"
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

PASS: macOS dmg smoke test ($(basename "$dmg"))
  dmg mount + copy           ${t_copy}s
  window 'Nugget' shown      ${t_window}s after launch (title via ${title_source})
  connected to nuggetd       ${t_connect}s after launch (daemon started by: ${started_by})
  app exit after close       ${t_exit}s (status 0)
  nuggetd stop after TERM    ${t_stop}s
  total                      $(elapsed "$t0")s
EOF
