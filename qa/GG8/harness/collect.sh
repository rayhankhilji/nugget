#!/usr/bin/env bash
# Copy the small evidence files of a lab run into the repo: collect.sh <run dir> <dest dir>
set -euo pipefail
src=$1; dst=$2
mkdir -p "$dst"
for f in summary.json requests.jsonl dns.jsonl flows.txt ledger-final.json ledger-features.json error.txt; do
  [ -f "$src/$f" ] && head -c 2000000 "$src/$f" > "$dst/$f"
done
if [ -d "$src/logs" ]; then for f in "$src"/logs/*.log; do [ -f "$f" ] && tail -c 1000000 "$f" > "$dst/daemon-$(basename "$f")"; done; fi
[ -f "$src/daemon.stdout" ] && tail -c 300000 "$src/daemon.stdout" > "$dst/daemon.stdout" || true
