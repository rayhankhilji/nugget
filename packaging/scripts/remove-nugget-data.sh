#!/usr/bin/env bash
# List, and on request delete, what Heed leaves behind after the app is removed on Linux or macOS.
# Package managers can't ask each user about their data, so this does it.
#
# Usage: remove-nugget-data.sh [--yes] [--include-vault]
#
#   (no flags)       dry run: print every location that exists, with its size, and the saved keys
#   --yes            delete the models, store, settings, logs, cache, the app's own folders, the
#                    login entries and the saved keys in the OS keychain
#   --include-vault  also delete the vault (~/Nugget Vault). Only with --yes, and asks you to
#                    type "delete" first when run from a terminal
#
# The vault is listed but never deleted when `[vault] path` in config.toml points elsewhere: that
# is a folder you chose. Data under NUGGET_HOME is not touched either.
# Refuses to delete anything while nuggetd or the desktop app is running.
#
# Paths follow nugget-core paths.rs and identity.rs (directories::ProjectDirs("app", "heed",
# "Heed"), the desktop app's identifier app.heed.desktop), nuggetd service.rs, and the names
# from before the rename that a first launch copies from and leaves in place
# (ProjectDirs("dev", "Nugget", "Nugget"), dev.nugget.desktop); see
# docs/book/src/reference/data-dir.md.
set -euo pipefail

yes=0
include_vault=0
for arg in "$@"; do
  case "$arg" in
    --yes) yes=1 ;;
    --include-vault) include_vault=1 ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "remove-nugget-data: unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done
if [ "$include_vault" = 1 ] && [ "$yes" = 0 ]; then
  echo "remove-nugget-data: dry run; --include-vault only adds the vault to what would be deleted." >&2
fi

[ -n "${HOME:-}" ] || { echo "remove-nugget-data: HOME is not set" >&2; exit 2; }
# Keychain services: Heed's, and the pre-rename ones (the store's and the cloud session's).
services=("app.heed.Heed" "dev.nugget.Nugget" "nugget")
os="$(uname -s)"

# Folders: "label|path". The vault is kept apart and never deleted in the same step.
dirs=()
login=()
case "$os" in
  Linux)
    # `directories` uses an XDG variable only when it is an absolute path.
    xdg() { case "${1:-}" in /*) printf '%s' "$1" ;; *) printf '%s' "$2" ;; esac; }
    data_home="$(xdg "${XDG_DATA_HOME:-}" "$HOME/.local/share")"
    config_home="$(xdg "${XDG_CONFIG_HOME:-}" "$HOME/.config")"
    cache_home="$(xdg "${XDG_CACHE_HOME:-}" "$HOME/.cache")"
    data_dir="$data_home/heed"
    config_dir="$config_home/heed"
    legacy_data_dir="$data_home/nugget"
    for d in "$data_dir" "$legacy_data_dir"; do
      dirs+=("Models|$d/models")
      dirs+=("Data folder: store, index, logs (and models)|$d")
    done
    dirs+=("Settings|$config_dir")
    dirs+=("Settings (before the rename)|$config_home/nugget")
    dirs+=("Cache|$cache_home/heed")
    dirs+=("Cache (before the rename)|$cache_home/nugget")
    for id in app.heed.desktop dev.nugget.desktop; do
      dirs+=("App settings and history|$config_home/$id")
      dirs+=("App data and WebView storage|$data_home/$id")
      dirs+=("App WebView cache|$cache_home/$id")
    done
    login+=("$config_home/systemd/user/nuggetd.service")
    login+=("$config_home/autostart/app.heed.desktop.desktop")
    login+=("$config_home/autostart/dev.nugget.desktop.desktop")
    sha256() { sha256sum | cut -d' ' -f1; }
    ;;
  Darwin)
    lib="$HOME/Library"
    data_dir="$lib/Application Support/app.heed.Heed"
    config_dir="$data_dir"
    legacy_data_dir="$lib/Application Support/dev.Nugget.Nugget"
    for d in "$data_dir" "$legacy_data_dir"; do
      dirs+=("Models|$d/models")
      dirs+=("Data folder: store, index, settings, logs, models|$d")
    done
    dirs+=("Cache|$lib/Caches/app.heed.Heed")
    dirs+=("Cache (before the rename)|$lib/Caches/dev.Nugget.Nugget")
    for id in app.heed.desktop dev.nugget.desktop; do
      dirs+=("App settings and history|$lib/Application Support/$id")
      dirs+=("App cache|$lib/Caches/$id")
      dirs+=("App WebView storage|$lib/WebKit/$id")
      dirs+=("App web storage|$lib/HTTPStorages/$id")
      dirs+=("App saved window state|$lib/Saved Application State/$id.savedState")
      login+=("$lib/LaunchAgents/$id.autostart.plist")
      login+=("$lib/Preferences/$id.plist")
      login+=("$lib/HTTPStorages/$id.binarycookies")
    done
    login+=("$lib/LaunchAgents/app.heed.nuggetd.plist")
    login+=("$lib/LaunchAgents/dev.nugget.nuggetd.plist")
    sha256() { shasum -a 256 | cut -d' ' -f1; }
    ;;
  *)
    echo "remove-nugget-data: for Linux and macOS; on Windows the uninstaller offers this" >&2
    exit 2
    ;;
esac

# The vault: `[vault] path` in config.toml when set (never deleted here), else ~/Nugget Vault.
custom_vault=""
# Settings not moved yet: read the pre-rename copy.
if [ ! -f "$config_dir/config.toml" ]; then
  case "$os" in
    Linux) [ -f "$config_home/nugget/config.toml" ] && config_dir="$config_home/nugget" ;;
    Darwin) [ -f "$legacy_data_dir/config.toml" ] && config_dir="$legacy_data_dir" ;;
  esac
fi
if [ -f "$config_dir/config.toml" ]; then
  custom_vault="$(awk '
    /^[[:space:]]*\[/ { sect = $0; gsub(/[[:space:]]/, "", sect); next }
    sect == "[vault]" && /^[[:space:]]*path[[:space:]]*=/ {
      v = $0; sub(/^[^=]*=[[:space:]]*/, "", v)
      if (v ~ /^"/) { sub(/^"/, "", v); sub(/".*$/, "", v) }
      else if (v ~ /^'\''/) { sub(/^'\''/, "", v); sub(/'\''.*$/, "", v) }
      print v; exit
    }' "$config_dir/config.toml")"
fi
vault=""
if [ -z "$custom_vault" ] && [ -d "$HOME/Nugget Vault" ]; then
  vault="$HOME/Nugget Vault"
fi

# A folder that holds the vault you chose is kept whole, even when it is one of Heed's own (a
# vault put in the data folder, or reached through a link into it). Paths are compared after
# links are resolved; a leading ~ is the home folder, as nuggetd reads it.
canon() {
  local p="$1" rest=""
  while [ "$p" != "/" ] && [ -n "$p" ] && [ ! -d "$p" ]; do
    rest="/${p##*/}$rest"
    p="${p%/*}"
  done
  [ -n "$p" ] || p=/
  local real
  real="$(cd -- "$p" 2>/dev/null && pwd -P)" || real="$p"
  printf '%s%s' "${real%/}" "$rest"
}
custom_vault_real=""
case "$custom_vault" in
  "~") custom_vault_real="$(canon "$HOME")" ;;
  "~/"*) custom_vault_real="$(canon "$HOME/${custom_vault#\~/}")" ;;
  /*) custom_vault_real="$(canon "$custom_vault")" ;;
esac
holds_vault() {
  [ -n "$custom_vault_real" ] || return 1
  local dir
  dir="$(canon "$1")"
  case "${custom_vault_real%/}/" in
    "${dir%/}/"*) return 0 ;;
    *) return 1 ;;
  esac
}

size_of() { du -sh "$1" 2>/dev/null | cut -f1; }

# The store key of the default data folder is "store-key-" + 16 hex digits of the SHA-256 of its
# path (nugget-store key.rs), for the folder now and the one before the rename. Keys of other
# data folders (HEED_HOME/NUGGET_HOME, --home) are kept.
own_store_key="store-key-$(printf '%s' "$data_dir" | sha256 | cut -c1-16)"
legacy_store_key="store-key-$(printf '%s' "$legacy_data_dir" | sha256 | cut -c1-16)"
want_key() {
  case "$1" in
    store-key-*) [ "$1" = "$own_store_key" ] || [ "$1" = "$legacy_store_key" ] ;;
    *) return 0 ;;
  esac
}

# Account names of the saved keys under service $1, one per line.
list_keys() {
  local service="$1"
  case "$os" in
    Linux)
      command -v secret-tool >/dev/null || return 0
      secret-tool search --all service "$service" 2>/dev/null \
        | sed -n 's/^attribute\.username = //p' | sort -u
      ;;
    Darwin)
      # Attributes only; `dump-keychain` without -d never prints a secret.
      security dump-keychain 2>/dev/null | awk -v svc="$service" '
        /^keychain: / { if (s == svc && a != "") print a; s = ""; a = "" }
        /"svce"<blob>="/ { v = $0; sub(/.*"svce"<blob>="/, "", v); sub(/"$/, "", v); s = v }
        /"acct"<blob>="/ { v = $0; sub(/.*"acct"<blob>="/, "", v); sub(/"$/, "", v); a = v }
        END { if (s == svc && a != "") print a }' | sort -u
      ;;
  esac
}

# delete_key SERVICE ACCOUNT
delete_key() {
  local service="$1"
  shift
  case "$os" in
    Linux) secret-tool clear service "$service" username "$1" ;;
    Darwin) security delete-generic-password -s "$service" -a "$1" >/dev/null ;;
  esac
}

echo "Heed data on this computer ($os):"
found=0
for entry in "${dirs[@]}"; do
  label="${entry%%|*}"
  path="${entry#*|}"
  if [ -e "$path" ]; then
    printf '  %-48s %6s  %s\n' "$label" "$(size_of "$path")" "$path"
    if holds_vault "$path"; then
      echo "      (kept: your vault, in a folder you chose, is inside it)"
    fi
    found=1
  fi
done
for path in "${login[@]}"; do
  if [ -e "$path" ]; then
    printf '  %-48s %6s  %s\n' "Login entry or app preference" "" "$path"
    found=1
  fi
done
# "service|account" pairs.
keys=()
for service in "${services[@]}"; do
  while IFS= read -r account; do
    [ -n "$account" ] && keys+=("$service|$account")
  done < <(list_keys "$service")
done
for key in "${keys[@]+"${keys[@]}"}"; do
  service="${key%%|*}"
  account="${key#*|}"
  if want_key "$account"; then
    printf '  %-48s %6s  %s\n' "Saved key (keychain service $service)" "" "$account"
    found=1
  else
    printf '  %-48s %6s  %s\n' "Kept: key of another data folder" "" "$account"
  fi
done
if [ "$os" = Linux ] && ! command -v secret-tool >/dev/null; then
  echo "  (secret-tool is not installed, so saved keys in the Secret Service were not listed)"
fi
if [ -n "$vault" ]; then
  printf '  %-48s %6s  %s\n' "Your Nugget Vault (your notes)" "$(size_of "$vault")" "$vault"
fi
if [ -n "$custom_vault" ]; then
  echo "  Your vault, in a folder you chose (never deleted here): $custom_vault"
fi
if [ -n "${NUGGET_HOME:-}" ]; then
  echo "  NUGGET_HOME is set: the data in $NUGGET_HOME is not touched."
fi
[ "$found" = 1 ] || [ -n "$vault" ] || echo "  nothing found"

if [ "$yes" = 0 ]; then
  echo
  echo "Dry run: nothing was deleted. Run again with --yes to delete the above (except the vault);"
  echo "add --include-vault to delete the vault as well."
  exit 0
fi

# Never delete under a running daemon or app: they would recreate files or lose writes.
for proc in nuggetd nugget-desktop; do
  if pgrep -x "$proc" >/dev/null 2>&1; then
    echo "remove-nugget-data: $proc is running. Quit Heed (and run 'nuggetd uninstall-service' if" >&2
    echo "it starts at login), then try again. Nothing was deleted." >&2
    exit 1
  fi
done

if [ "$include_vault" = 1 ] && [ -n "$vault" ] && [ -t 0 ]; then
  printf 'Delete your Nugget Vault (%s, %s)? This deletes your notes. Type "delete" to confirm: ' \
    "$vault" "$(size_of "$vault")"
  read -r answer
  if [ "$answer" != "delete" ]; then
    echo "The vault is kept."
    include_vault=0
  fi
fi

echo
# Login entries first, so nothing starts Heed again halfway through.
case "$os" in
  Linux)
    if [ -e "${login[0]}" ]; then
      systemctl --user disable --now nuggetd.service >/dev/null 2>&1 || true
    fi
    ;;
  Darwin)
    for label in app.heed.nuggetd app.heed.desktop.autostart dev.nugget.nuggetd dev.nugget.desktop.autostart; do
      launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
    done
    ;;
esac
for path in "${login[@]}"; do
  if [ -e "$path" ]; then
    rm -f -- "$path" && echo "Deleted $path"
  fi
done
if [ "$os" = Linux ] && [ -e "$config_home/systemd/user" ]; then
  systemctl --user daemon-reload >/dev/null 2>&1 || true
fi

for key in "${keys[@]+"${keys[@]}"}"; do
  service="${key%%|*}"
  account="${key#*|}"
  if want_key "$account"; then
    if delete_key "$service" "$account"; then
      echo "Removed saved key $account"
    else
      echo "Could not remove saved key $account" >&2
    fi
  fi
done

# rm -r never follows a symlink inside the folder, so nothing outside it is deleted.
for entry in "${dirs[@]}"; do
  path="${entry#*|}"
  if [ -e "$path" ] && holds_vault "$path"; then
    echo "Kept $path: your vault ($custom_vault) is inside it"
  elif [ -e "$path" ]; then
    rm -rf -- "$path" && echo "Deleted $path"
  fi
done

if [ "$include_vault" = 1 ] && [ -n "$vault" ]; then
  rm -rf -- "$vault" && echo "Deleted $vault"
elif [ -n "$vault" ]; then
  echo "Kept your vault: $vault"
fi
