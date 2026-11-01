#!/usr/bin/env bash
# Regression test for remove-nugget-data.sh (Linux): a vault the user chose is never deleted, even
# when it sits inside Nugget's data or settings folder, directly or through a link (LC-1).
# Runs in a throwaway HOME; touches nothing else. Usage: test-remove-nugget-data.sh
set -euo pipefail
script="$(cd "$(dirname "$0")" && pwd)/remove-nugget-data.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail=0

home_with() { # name config-vault-line
  local h="$tmp/$1"
  mkdir -p "$h/.config/nugget" "$h/.local/share/nugget/vault" "$h/.local/share/nugget/models"
  echo note > "$h/.local/share/nugget/vault/me.md"
  printf '[vault]\n%s\n' "$2" > "$h/.config/nugget/config.toml"
  printf '%s' "$h"
}
run() { env -i PATH=/usr/bin:/bin HOME="$1" bash "$script" --yes </dev/null >/dev/null; }
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; fail=1; fi; }

h="$(home_with tilde 'path = "~/.local/share/nugget/vault"')"
run "$h"
check "vault inside the data folder (~) is kept" '[ -f "$h/.local/share/nugget/vault/me.md" ]'
check "the rest is still removed" '[ ! -e "$h/.local/share/nugget/models" ] && [ ! -e "$h/.config/nugget" ]'

h="$tmp/link"
mkdir -p "$h"
ln -s "$h/.local/share/nugget/vault" "$h/Notes"
home_with link "path = '$h/Notes'" >/dev/null
run "$h"
check "vault linked into the data folder is kept" '[ -f "$h/.local/share/nugget/vault/me.md" ]'

h="$(home_with elsewhere 'path = "~/Notes"')"
mkdir -p "$h/Notes" "$h/.local/share/nugget-notes"
echo x > "$h/Notes/a.md"
run "$h"
check "vault elsewhere: data removed, vault kept" \
  '[ ! -e "$h/.local/share/nugget" ] && [ -f "$h/Notes/a.md" ] && [ -d "$h/.local/share/nugget-notes" ]'

exit "$fail"
