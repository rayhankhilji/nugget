#!/usr/bin/env bash
# II4: single instance and nugget:// delivery in the real app (Linux, Xvfb, private session bus).
# Starts nugget-desktop (debug build, own NUGGET_HOME; it starts target/debug/nuggetd itself), then
# launches a second copy with each argument in links.txt and records: whether the second copy
# exits, how fast, and what the first instance's log says about the link. Output: OUT/*.log,
# OUT/result.tsv, OUT/*.png.
#   qa/JJ1/platform/single_instance.sh OUT
set -uo pipefail
out="$(mkdir -p "$1" && cd "$1" && pwd)"
repo="$(cd "$(dirname "$0")/../../.." && pwd)"
app="${APP:-$repo/target/debug/nugget-desktop}"
display=:98
Xvfb $display -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 & xvfb=$!
trap 'kill $xvfb 2>/dev/null' EXIT
sleep 1
export DISPLAY=$display XDG_SESSION_TYPE=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
export NUGGET_HOME="$out/home" NUGGET_KEYCHAIN=off NUGGET_LOG="info,nugget_desktop=debug"
rm -rf "$NUGGET_HOME"
dbus-run-session -- bash -c '
  app="$1"; out="$2"
  openbox >/dev/null 2>&1 & wm=$!
  "$app" > "$out/first.log" 2>&1 & first=$!
  for i in $(seq 120); do grep -q "connected to nuggetd" "$out/first.log" && break; sleep 0.5; done
  sleep 3
  import -window root "$out/first.png" 2>/dev/null
  if [ -n "${PRE_LINK:-}" ]; then   # a link delivered during onboarding, then "Skip setup"
    timeout 20 "$app" "$PRE_LINK" > "$out/second-pre.log" 2>&1; sleep 2
    import -window root "$out/pre-before-skip.png" 2>/dev/null
  fi
  if [ -n "${SKIP_SETUP:-}" ]; then
    wid=$(xdotool search --name "^Nugget$" | head -1); xdotool windowactivate --sync "$wid" 2>/dev/null
    eval "$(xdotool getwindowgeometry --shell "$wid")"
    xdotool mousemove 1326 236 click 1   # "Skip setup" (1920x1080 Xvfb, default window placement)
    sleep 3; import -window root "$out/after-skip.png" 2>/dev/null
  fi
  printf "label\tsecond_exit\tsecond_ms\tfirst_alive\tfirst_log_after\n" > "$out/result.tsv"
  while IFS=$'"'"'\t'"'"' read -r label arg; do
    [ -z "$label" ] && continue; case "$label" in \#*) continue;; esac
    before=$(wc -l < "$out/first.log")
    t0=$(date +%s%N)
    timeout 20 "$app" "$arg" > "$out/second-$label.log" 2>&1; rc=$?
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    sleep 1.5
    alive=$(kill -0 $first 2>/dev/null && echo yes || echo no)
    after=$(tail -n +$((before+1)) "$out/first.log" | grep -iE "deeplink|link|plugin|quick|single" | tr "\n\t" "  " | cut -c1-300)
    printf "%s\t%s\t%s\t%s\t%s\n" "$label" "$rc" "$ms" "$alive" "$after" >> "$out/result.tsv"
    import -window root "$out/after-$label.png" 2>/dev/null
  done < "$3"
  kill $first; sleep 2; kill -9 $first 2>/dev/null; kill $wm
' _ "$app" "$out" "$(dirname "$0")/si-links.tsv"
cat "$out/result.tsv"
