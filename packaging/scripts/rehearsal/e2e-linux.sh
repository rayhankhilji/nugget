#!/usr/bin/env bash
# Release rehearsal, Linux x64: the end-to-end checks, after prepare.sh has built and signed
# everything and the throwaway CA is trusted (trust-ca.sh add).
#
#   1. Model manifest: nuggetd built with the throwaway release key accepts the bundled manifest
#      (signed in the job); nuggetd built with a different key rejects it.
#   2. Update: the OLD AppImage, copied to a temp install path with auto-install on, finds the
#      signed update on the HTTPS localhost feed, downloads and verifies it, and replaces itself
#      with the NEW AppImage when its window is closed. The result is byte-identical to the new
#      build, carries the new version and starts.
#   3. Wrong feed key: beta.json signed by a key the app doesn't trust is refused; nothing
#      installs.
#   4. Wrong payload key: a correctly signed feed whose payload signature is by the wrong key is
#      offered, but the download fails verification; nothing installs.
#
# Each launch runs like packaging/scripts/smoke-appimage.sh: throwaway HOME/XDG/NUGGET_HOME,
# Xvfb + openbox, NUGGET_TRAY=off, closed through the window manager. Every wait is bounded
# (the app's first update check runs 60 s after launch). Results go to the evidence report;
# logs of every case go to $EVIDENCE/logs/<case>/. Exits 1 if any check failed.
#
# Needs: Xvfb, openbox, wmctrl, dbus-run-session, python3.
set -uo pipefail
# shellcheck source=packaging/scripts/rehearsal/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

for tool in Xvfb openbox wmctrl dbus-run-session; do
  command -v "$tool" >/dev/null || rdie "missing required tool: $tool"
done

old="$(ls "$REHEARSAL_DIR"/build/old/*.AppImage)" || rdie "no old AppImage"
new="$(ls "$REHEARSAL_DIR"/build/new/*.AppImage)" || rdie "no new AppImage"
old_sha="$(sha256 "$old")"
new_sha="$(sha256 "$new")"
[ "$old_sha" != "$new_sha" ] || rdie "old and new AppImages are identical"
payload="$(basename "$new")"
tls="$REHEARSAL_DIR/tls"

check_timeout="${REHEARSAL_CHECK_TIMEOUT:-240}" # first check at 60 s, plus download time
window_timeout=90
quit_timeout=90
fails=0

work="$(mktemp -d "${TMPDIR:-/tmp}/nugget-rehearsal.XXXXXX")"
xvfb_pid="" wm_pid="" server_pid="" app_pid="" display=""
case_dir="" app_log="" install=""

ms() { date +%s%3N; }

pass() { record "$1" PASS "${2:-}"; }
fail() {
  record "$1" FAIL "${2:-}"
  fails=$((fails + 1))
  [ -z "$app_log" ] || { echo "---- app log ($app_log) ----"; tail -n 80 "$app_log" 2>/dev/null; }
}

# ---- processes ---------------------------------------------------------------------------------

stop_pid() {
  local pid="$1"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 0
  kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.25; done
  kill -KILL "$pid" 2>/dev/null
}

# nuggetd processes of the current case (their NUGGET_HOME is the case's).
case_daemons() {
  local pid
  for pid in $(pgrep -x nuggetd 2>/dev/null || true); do
    tr '\0' '\n' 2>/dev/null <"/proc/$pid/environ" | grep -qxF "NUGGET_HOME=${NUGGET_HOME:-unset}" && echo "$pid"
  done
}

stop_app() {
  if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
    kill -TERM -- "-$app_pid" 2>/dev/null
    sleep 1
    kill -KILL -- "-$app_pid" 2>/dev/null
  fi
  app_pid=""
  local pid
  for pid in $(case_daemons); do stop_pid "$pid"; done
}

stop_server() { stop_pid "$server_pid"; server_pid=""; }

cleanup() {
  set +e
  stop_app
  stop_server
  stop_pid "$wm_pid"
  stop_pid "$xvfb_pid"
  rm -rf "$work"
}
trap cleanup EXIT

wait_for() { # timeout cmd... ; polls every 0.5 s
  local deadline=$(($(ms) + $1 * 1000))
  shift
  until "$@"; do
    [ "$(ms)" -lt "$deadline" ] || return 1
    sleep 0.5
  done
}

# ---- feed server -------------------------------------------------------------------------------

start_server() { # variant
  stop_server
  local ready="$work/server-ready"
  rm -f "$ready"
  "$PY" -I "$REHEARSAL_SCRIPTS/feed-server.py" --root "$REHEARSAL_DIR/serve/$1" --port "$PORT" \
    --cert "$tls/server.crt" --key "$tls/server.key" \
    --log "$case_dir/feed-access.log" --ready-file "$ready" 2>>"$case_dir/feed-server.log" &
  server_pid=$!
  wait_for 15 test -s "$ready" || rdie "the feed server did not start (see $case_dir/feed-server.log)"
}

# ---- the app -----------------------------------------------------------------------------------

main_window() {
  DISPLAY="$display" wmctrl -l 2>/dev/null |
    awk '{ id = $1; $1 = $2 = $3 = ""; if (substr($0, 4) == "Nugget") { print id; exit } }'
}
have_window() { [ -n "$(main_window)" ]; }
no_window() { [ -z "$(main_window)" ]; }
logged() { grep -qF "$1" "$app_log" 2>/dev/null; }
app_gone() { ! kill -0 "$app_pid" 2>/dev/null; }

# Fresh profile for a case; the install path holds a copy of the OLD AppImage.
new_case() { # name
  case_dir="$EVIDENCE/logs/$1"
  mkdir -p "$case_dir"
  local w="$work/$1"
  mkdir -p "$w/install"
  install="$w/install/Nugget.AppImage"
  cp "$old" "$install"
  chmod +x "$install"
  export NUGGET_HOME="$w/nh" HOME="$w/home"
  export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
  export XDG_CACHE_HOME="$HOME/.cache" XDG_STATE_HOME="$HOME/.local/state"
  export XDG_RUNTIME_DIR="$w/run"
  mkdir -p "$NUGGET_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  # Beta channel (where a prerelease is published) with auto-install: download and verify in
  # the background, install at quit.
  mkdir -p "$XDG_CONFIG_HOME/dev.nugget.desktop"
  printf '{"channel":"beta","auto_install":true}\n' >"$XDG_CONFIG_HOME/dev.nugget.desktop/settings.json"
  # Any temp files (the updater's backup of the running AppImage) stay in the case directory.
  export TMPDIR="$w/tmp"
  mkdir -p "$TMPDIR"
  # A window left over from an earlier case would be mistaken for this one.
  wait_for 15 no_window || rdie "a 'Nugget' window from an earlier case is still open"
}

launch() { # log-name
  app_log="$case_dir/$1"
  setsid dbus-run-session -- "$install" >"$app_log" 2>&1 &
  app_pid=$!
  if ! wait_for "$window_timeout" have_window; then
    return 1
  fi
}

# Close the window as a user would and wait for the process to end. Sets close_status to the
# exit status (or "timeout"). Not in a subshell: `wait` needs the app to be our child.
close_status=""
close_app() {
  local id
  id="$(main_window)"
  [ -n "$id" ] && DISPLAY="$display" wmctrl -i -c "$id"
  if ! wait_for "$quit_timeout" app_gone; then
    close_status="timeout"
    return 1
  fi
  close_status=0
  wait "$app_pid" 2>/dev/null || close_status=$?
  app_pid=""
}

# Wait for TEXT in the app log; give up early if the app exits.
wait_logged() { # timeout text
  local deadline=$(($(ms) + $1 * 1000))
  until logged "$2"; do
    [ "$(ms)" -lt "$deadline" ] || return 1
    app_gone && { logged "$2"; return; }
    sleep 0.5
  done
}

save_daemon_logs() {
  cp "$NUGGET_HOME"/data/logs/nuggetd*.log "$case_dir/" 2>/dev/null || true
}

end_case() {
  stop_app
  save_daemon_logs
  stop_server
  unset TMPDIR
  app_log=""
}

# minisign's error when a signature was made by another key (the only refusal these cases accept;
# "download or signature check failed" alone would also cover a 404 or a TLS error).
KEY_MISMATCH="different key than the one provided"

# Line number of the first app-log line containing TEXT (empty if none).
first_line() { grep -n -m1 -F "$1" "$app_log" 2>/dev/null | cut -d: -f1 || true; }

served() { grep -qE "\"GET /$1 HTTP/1\\.[01]\" 200" "$case_dir/feed-access.log" 2>/dev/null; }

# ---- 1. model manifest -------------------------------------------------------------------------

rlog "manifest checks"
"$REHEARSAL_SCRIPTS/check-manifest.sh" "$REHEARSAL_DIR/bin/nuggetd-release-key" accept \
  "Manifest: nuggetd built with the throwaway release key accepts the signed manifest" || fails=$((fails + 1))
"$REHEARSAL_SCRIPTS/check-manifest.sh" "$REHEARSAL_DIR/bin/nuggetd-wrong-key" reject \
  "Manifest: nuggetd built with the wrong release key rejects it (no models offered)" || fails=$((fails + 1))

# ---- X server and window manager ---------------------------------------------------------------

Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp 3>"$work/display" >"$work/xvfb.log" 2>&1 &
xvfb_pid=$!
wait_for 15 test -s "$work/display" || rdie "Xvfb did not start"
display=":$(tr -dc '0-9' <"$work/display")"
export DISPLAY="$display"
openbox --sm-disable >"$work/openbox.log" 2>&1 &
wm_pid=$!
wait_for 15 eval "wmctrl -m >/dev/null 2>&1" || rdie "openbox did not start"

export NUGGET_TRAY=off NUGGET_LOG=info NO_COLOR=1 APPIMAGE_EXTRACT_AND_RUN=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 GDK_BACKEND=x11
export NO_PROXY="localhost,127.0.0.1,::1" no_proxy="localhost,127.0.0.1,::1"
unset WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS HTTPS_PROXY https_proxy HTTP_PROXY http_proxy ALL_PROXY all_proxy APPIMAGE APPDIR

# ---- 2. the update -----------------------------------------------------------------------------

case_update() {
  local c="Update (Linux AppImage)" st
  new_case update
  start_server good
  rlog "case update: old $OLD_VERSION -> new $NEW_VERSION"
  launch app.log || { fail "$c: old app starts" "no window within ${window_timeout}s"; end_case; return; }
  if wait_logged "$check_timeout" "update available"; then
    pass "$c: old app finds the update on the signed feed" "$(grep -m1 -F 'update available' "$app_log" | sed 's/^.*INFO //')"
  else
    fail "$c: old app finds the update on the signed feed" "no 'update available' within ${check_timeout}s"
    end_case
    return
  fi
  if wait_logged "$check_timeout" "update downloaded; installs at quit"; then
    pass "$c: payload downloaded and its signature verified" "logged 'update downloaded; installs at quit'"
  else
    fail "$c: payload downloaded and its signature verified" "no 'update downloaded; installs at quit' within ${check_timeout}s"
    end_case
    return
  fi
  close_app; st="$close_status"
  if [ "$st" = 0 ] && logged "installing downloaded update at quit" && ! logged "update install at quit failed"; then
    pass "$c: installs when the window is closed" "logged 'installing downloaded update at quit'; exit status 0"
  else
    fail "$c: installs when the window is closed" "exit status $st; $(grep -F 'at quit' "$app_log" | tail -n 2 | tr '\n' ' ')"
  fi
  save_daemon_logs
  local stopped
  for stopped in $(case_daemons); do stop_pid "$stopped"; done

  local got
  got="$(sha256 "$install")"
  if [ "$got" = "$new_sha" ]; then
    pass "$c: installed AppImage is byte-identical to the new build" "sha256 $got"
  else
    fail "$c: installed AppImage is byte-identical to the new build" "sha256 $got; new $new_sha; old $old_sha"
  fi

  # The version compiled into the installed app.
  local x="$work/update/extract"
  mkdir -p "$x"
  if (cd "$x" && "$install" --appimage-extract usr/bin/nugget-desktop >/dev/null 2>&1) &&
    grep -qaF "$NEW_VERSION" "$x/squashfs-root/usr/bin/nugget-desktop" &&
    ! grep -qaF "$OLD_VERSION" "$x/squashfs-root/usr/bin/nugget-desktop"; then
    pass "$c: installed app reports the new version" "usr/bin/nugget-desktop embeds $NEW_VERSION, not $OLD_VERSION"
  else
    fail "$c: installed app reports the new version" "version string $NEW_VERSION not found in the extracted binary"
  fi

  if served "feed/beta.json" && served "feed/beta.json.sig" && served "dist/$payload"; then
    pass "$c: fetched the feed, its signature and the payload over HTTPS" "see logs/update/feed-access.log"
  else
    fail "$c: fetched the feed, its signature and the payload over HTTPS" "$(tr '\n' ' ' <"$case_dir/feed-access.log" 2>/dev/null)"
  fi

  local dlog
  dlog="$(ls "$NUGGET_HOME"/data/logs/nuggetd*.log 2>/dev/null | tail -n1)"
  if [ -n "$dlog" ] && grep -qF '"nuggetd ready"' "$dlog" && ! grep -qF "bundled model manifest failed verification" "$dlog"; then
    pass "Manifest: the bundled nuggetd (started by the app) accepts the signed manifest" "no verification failure in its log"
  else
    fail "Manifest: the bundled nuggetd (started by the app) accepts the signed manifest" "log: ${dlog:-none}"
  fi

  # The updated app starts and connects to its daemon.
  if launch app-after-update.log && wait_logged 120 "connected to nuggetd"; then
    pass "$c: the updated app starts and connects to nuggetd" ""
  else
    fail "$c: the updated app starts and connects to nuggetd" "no window or no 'connected to nuggetd' within the limit"
  fi
  close_app
  end_case
}

# ---- 3. wrong feed key -------------------------------------------------------------------------

case_wrong_feed_key() {
  local c="Wrong feed key (Linux)" st
  new_case wrong-feed-key
  start_server wrong-feed-key
  rlog "case wrong-feed-key"
  launch app.log || { fail "$c: old app starts" "no window within ${window_timeout}s"; end_case; return; }
  if wait_logged "$check_timeout" "update check failed" && logged "the update feed failed signature verification" &&
    logged "$KEY_MISMATCH" && served "feed/beta.json.sig" && ! logged "update available"; then
    pass "$c: feed refused" "$(grep -m1 -F 'update check failed' "$app_log" | sed 's/^.*WARN //')"
  else
    fail "$c: feed refused" "no 'update check failed ... the update feed failed signature verification ... $KEY_MISMATCH' within ${check_timeout}s, beta.json.sig not served, or an update was offered"
  fi
  close_app; st="$close_status"
  if [ "$(sha256 "$install")" = "$old_sha" ] && ! logged "installing downloaded update"; then
    pass "$c: nothing installed" "install path unchanged (old sha256); exit status $st"
  else
    fail "$c: nothing installed" "the install path changed"
  fi
  end_case
}

# ---- 4. wrong payload key ----------------------------------------------------------------------

case_wrong_payload_key() {
  local c="Wrong payload key (Linux)" st
  new_case wrong-payload-key
  start_server wrong-payload-key
  rlog "case wrong-payload-key"
  launch app.log || { fail "$c: old app starts" "no window within ${window_timeout}s"; end_case; return; }
  local avail dlfail
  wait_logged "$check_timeout" "update download failed"
  avail="$(first_line "update available")"
  dlfail="$(first_line "update download failed")"
  # Offered first, the payload really downloaded (HTTP 200), then refused for the key alone.
  if [ -n "$avail" ] && [ -n "$dlfail" ] && [ "$avail" -lt "$dlfail" ] && served "dist/$payload" &&
    logged "download or signature check failed" && logged "$KEY_MISMATCH" && ! logged "update downloaded"; then
    pass "$c: payload signature refused" "$(grep -m1 -F 'update download failed' "$app_log" | sed 's/^.*WARN //' | cut -c1-300)"
  else
    fail "$c: payload signature refused" "need 'update available' (line ${avail:-none}), then 'update download failed ... $KEY_MISMATCH' (line ${dlfail:-none}) within ${check_timeout}s, and a 200 for dist/$payload; or the payload was accepted"
  fi
  close_app; st="$close_status"
  if [ "$(sha256 "$install")" = "$old_sha" ] && ! logged "installing downloaded update"; then
    pass "$c: nothing installed" "install path unchanged (old sha256); exit status $st"
  else
    fail "$c: nothing installed" "the install path changed"
  fi
  end_case
}

case_update
case_wrong_feed_key
case_wrong_payload_key

rlog "done: $fails failed check(s)"
[ "$fails" = 0 ]
