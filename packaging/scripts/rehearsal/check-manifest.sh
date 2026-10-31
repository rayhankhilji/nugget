#!/usr/bin/env bash
# Release rehearsal (Linux): does a release-built nuggetd accept or reject the bundled model
# manifest? Starts NUGGETD against a throwaway NUGGET_HOME, waits (bounded) for its "nuggetd
# ready" log line, asks it for `models.list` over its IPC socket, then stops it with SIGTERM.
#
#   accept: no "bundled model manifest failed verification" in the daemon log, and models.list
#           returns the manifest's models
#   reject: the daemon logs "bundled model manifest failed verification", and models.list
#           fails with "model manifest unavailable" (no model downloads are offered)
#
# Usage: check-manifest.sh NUGGETD accept|reject CHECK_NAME
# Records one row in the evidence report and copies the daemon log into the evidence.
set -uo pipefail
# shellcheck source=packaging/scripts/rehearsal/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

nuggetd="${1:?usage: check-manifest.sh NUGGETD accept|reject NAME}"
expect="${2:?}"
name="${3:?}"
slug="manifest-$expect"
work="$(mktemp -d "${TMPDIR:-/tmp}/nrm.XXXXXX")"
export NUGGET_HOME="$work/h" HOME="$work/home" XDG_RUNTIME_DIR="$work/run" NUGGET_LOG=info NO_COLOR=1
mkdir -p "$NUGGET_HOME" "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
logdir="$NUGGET_HOME/data/logs"
out="$EVIDENCE/logs/$slug"
mkdir -p "$out"

daemon_pids() {
  local pid
  for pid in $(pgrep -x nuggetd 2>/dev/null || true); do
    tr '\0' '\n' 2>/dev/null <"/proc/$pid/environ" | grep -qxF "NUGGET_HOME=$NUGGET_HOME" && echo "$pid"
  done
}
logfile() { local f last=""; for f in "$logdir"/nuggetd*.log; do [ -f "$f" ] && last="$f"; done; echo "$last"; }
logged() { local f; f="$(logfile)"; [ -n "$f" ] && grep -qF "$1" "$f"; }

finish() { # PASS|FAIL detail
  local pid
  for pid in $(daemon_pids); do kill -TERM "$pid" 2>/dev/null; done
  for _ in $(seq 1 60); do [ -z "$(daemon_pids)" ] && break; sleep 0.25; done
  for pid in $(daemon_pids); do kill -KILL "$pid" 2>/dev/null; done
  [ -n "$runner_pid" ] && kill "$runner_pid" 2>/dev/null
  cp "$logdir"/nuggetd*.log "$out/" 2>/dev/null
  cp "$work/stdout.log" "$out/nuggetd.stdout.log" 2>/dev/null
  record "$name" "$1" "$2"
  if [ "$1" != PASS ]; then
    echo "---- daemon log ----"; cat "$(logfile)" 2>/dev/null; cat "$work/stdout.log" 2>/dev/null
  fi
  rm -rf "$work"
  [ "$1" = PASS ]
  exit $?
}

# Run it as "nuggetd" (process names are cut to 15 characters, and pgrep matches on them).
mkdir -p "$work/bin"
cp "$nuggetd" "$work/bin/nuggetd"
# Own session bus, as when the app starts it; own process group for cleanup.
setsid dbus-run-session -- "$work/bin/nuggetd" >"$work/stdout.log" 2>&1 &
runner_pid=$!

deadline=$((SECONDS + 90))
until logged '"nuggetd ready"'; do
  [ "$SECONDS" -lt "$deadline" ] || finish FAIL "no 'nuggetd ready' within 90s"
  kill -0 "$runner_pid" 2>/dev/null || finish FAIL "nuggetd exited before it was ready"
  sleep 0.5
done
socket="$("$PY" -I -c '
import json, sys
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        rec = json.loads(line)
    except ValueError:
        continue
    f = rec.get("fields", {})
    if f.get("message") == "nuggetd ready":
        print(f.get("socket", ""))
        break
' "$(logfile)")"
[ -n "$socket" ] || finish FAIL "the ready line names no socket"

resp="$("$PY" -I "$REHEARSAL_SCRIPTS/ipc-call.py" "$socket" models.list --timeout 30)"
status=$?
echo "$resp" >"$out/models.list.json"
count="$(printf '%s' "$resp" | "$PY" -I -c '
import json, sys
try:
    r = json.load(sys.stdin).get("result")
except ValueError:
    r = None
print(len(r) if isinstance(r, list) else -1)')"
failed=0
logged "bundled model manifest failed verification" && failed=1

case "$expect" in
  accept)
    if [ "$failed" = 0 ] && [ "$status" = 0 ] && [ "$count" -gt 0 ]; then
      finish PASS "no verification failure logged; models.list returned $count models"
    fi
    finish FAIL "verification failure logged=$failed, models.list status=$status count=$count: $resp"
    ;;
  reject)
    if [ "$failed" = 1 ] && [ "$status" = 3 ] && grep -qF "model manifest unavailable" <<<"$resp"; then
      finish PASS "logged 'bundled model manifest failed verification'; models.list: model manifest unavailable"
    fi
    finish FAIL "verification failure logged=$failed, models.list status=$status: $resp"
    ;;
  *) finish FAIL "expect must be accept or reject" ;;
esac
