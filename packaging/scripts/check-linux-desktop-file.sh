#!/usr/bin/env bash
# Check that a Linux package ships the desktop entry named after the app id, which the XDG
# desktop portal needs to accept "dev.nugget.desktop" from an unsandboxed app (GlobalShortcuts
# refuses a session without it), and that its Exec and Icon resolve inside the package.
# Usage: packaging/scripts/check-linux-desktop-file.sh <deb|rpm> path/to/package
#
# Needs: dpkg-deb (deb), rpm2cpio and cpio, or bsdtar (rpm). desktop-file-validate is used when
# installed.
set -euo pipefail

usage() { echo "usage: $0 <deb|rpm> path/to/package" >&2; exit 2; }
[ $# -eq 2 ] || usage
kind="$1"
pkg="$2"
[ -f "$pkg" ] || { echo "check-desktop: no such file: $pkg" >&2; exit 2; }

app_id_file="usr/share/applications/dev.nugget.desktop.desktop"
tauri_file="usr/share/applications/Nugget.desktop"
binary="/usr/bin/nugget-desktop"
icon="nugget-desktop"

root="$(mktemp -d "${TMPDIR:-/tmp}/nugget-desktop-check.XXXXXX")"
trap 'rm -rf "$root"' EXIT

case "$kind" in
  deb) dpkg-deb -x "$pkg" "$root" ;;
  rpm)
    pkg_abs="$(cd "$(dirname "$pkg")" && pwd)/$(basename "$pkg")"
    if command -v rpm2cpio >/dev/null && command -v cpio >/dev/null; then
      (cd "$root" && rpm2cpio "$pkg_abs" | cpio -idm --quiet)
    elif command -v bsdtar >/dev/null; then
      bsdtar -xf "$pkg_abs" -C "$root"
    else
      echo "check-desktop: need rpm2cpio and cpio, or bsdtar, to unpack an rpm" >&2
      exit 2
    fi
    ;;
  *) usage ;;
esac

errors=0
fail() {
  echo "check-desktop: FAIL: $*" >&2
  [ -z "${GITHUB_ACTIONS:-}" ] || echo "::error::$(basename "$pkg"): $*"
  errors=$((errors + 1))
}

# The first value of a key in the [Desktop Entry] group.
entry_key() { # file key
  awk -v key="$2" '
    /^\[/ { if (in_group) exit; in_group = ($0 == "[Desktop Entry]"); next }
    in_group {
      eq = index($0, "=")
      if (eq == 0) next
      k = substr($0, 1, eq - 1); sub(/[ \t]+$/, "", k)
      if (k == key) { v = substr($0, eq + 1); sub(/^[ \t]+/, "", v); print v; exit }
    }' "$1"
}

# argv[0] of an Exec value: these packages use unquoted Exec lines (a quoted one is accepted
# when it holds no escapes).
exec_program() { # exec-value
  local v="$1"
  case "$v" in
    \"*) v="${v#\"}"; echo "${v%%\"*}" ;;
    *) echo "${v%% *}" ;;
  esac
}

# Resolve a program the way the installed system would: absolute, or a bare name in /usr/bin.
check_exec() { # label desktop-file
  local exec prog path
  exec="$(entry_key "$2" Exec)"
  [ -n "$exec" ] || { fail "$1: no Exec"; return; }
  prog="$(exec_program "$exec")"
  case "$prog" in
    /*) path="$prog" ;;
    */*) fail "$1: Exec program '$prog' is a relative path"; return ;;
    *) path="/usr/bin/$prog" ;;
  esac
  if [ -x "$root$path" ] && [ -f "$root$path" ]; then
    echo "check-desktop: $1: Exec=$exec -> $path (in the package, executable)"
  else
    fail "$1: Exec program '$prog' ($path) is not an executable file in the package"
  fi
}

if [ -f "$root/$app_id_file" ]; then
  f="$root/$app_id_file"
  echo "check-desktop: found /$app_id_file"
  sed 's/^/    /' "$f"
  [ "$(entry_key "$f" Type)" = "Application" ] || fail "/$app_id_file: Type is not Application"
  [ "$(entry_key "$f" Hidden)" != "true" ] || fail "/$app_id_file: Hidden=true (GLib would not load it)"
  check_exec "/$app_id_file" "$f"
  exec_prog="$(exec_program "$(entry_key "$f" Exec)")"
  [ "$exec_prog" = "$binary" ] || fail "/$app_id_file: Exec program is '$exec_prog', expected $binary"
  try_exec="$(entry_key "$f" TryExec)"
  [ -z "$try_exec" ] || [ -x "$root$try_exec" ] || fail "/$app_id_file: TryExec '$try_exec' not in the package"
  [ "$(entry_key "$f" Icon)" = "$icon" ] || fail "/$app_id_file: Icon is not $icon"
  if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$f" || fail "/$app_id_file: desktop-file-validate reported errors"
  fi
else
  fail "missing /$app_id_file"
fi

# Tauri's own entry (the visible menu entry) must still be there.
if [ -f "$root/$tauri_file" ]; then
  check_exec "/$tauri_file" "$root/$tauri_file"
else
  fail "missing /$tauri_file (Tauri's own desktop entry)"
fi

if compgen -G "$root/usr/share/icons/hicolor/*/apps/$icon.png" >/dev/null; then
  echo "check-desktop: icon '$icon' installed: $(cd "$root" && ls usr/share/icons/hicolor/*/apps/"$icon".png | tr '\n' ' ')"
else
  fail "no usr/share/icons/hicolor/*/apps/$icon.png in the package"
fi

if [ "$errors" -gt 0 ]; then
  echo "check-desktop: $errors problem(s) in $(basename "$pkg")" >&2
  exit 1
fi
echo "check-desktop: PASS ($(basename "$pkg"))"
