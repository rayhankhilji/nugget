#!/usr/bin/env bash
# Deterministic build environment for release binaries (docs/security/reproducible-builds.md).
#
#   SOURCE_DATE_EPOCH  kept if already set, else the commit time of HEAD (`git log -1 --format=%ct`)
#   RUSTFLAGS          the caller's RUSTFLAGS plus --remap-path-prefix for the workspace root
#                      (-> /build), CARGO_HOME (-> /cargo), the rustc sysroot (-> /rustc-sysroot)
#                      and CARGO_TARGET_DIR when it lies outside the workspace (-> /target), so no
#                      machine-specific path ends up in panic messages or symbol paths
#
# Usage:
#   . packaging/scripts/repro-env.sh                     export into the current bash shell
#   packaging/scripts/repro-env.sh --github-env          append to $GITHUB_ENV (CI)
#   eval "$(packaging/scripts/repro-env.sh)"             print `export` lines (any POSIX shell)
#
# RUSTFLAGS is whitespace-separated, so none of the remapped paths may contain spaces.
# Setting RUSTFLAGS changes cargo's fingerprints: everything is rebuilt once, and caches keyed
# without it are not reused.

# Absolute, symlink-free, and in the form rustc sees: on Windows (Git Bash) a native C:\ path.
_repro_abs() {
  local d
  d="$(cd "$1" && pwd -P)" || return 1
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$d"; else printf '%s\n' "$d"; fi
}

_repro_env() {
  local root epoch cargo_home sysroot target_dir flags p
  root="$(_repro_abs "$(dirname "${BASH_SOURCE[0]}")/../..")" || return 1

  epoch="${SOURCE_DATE_EPOCH:-}"
  if [ -z "$epoch" ]; then
    epoch="$(git -C "$root" log -1 --format=%ct 2>/dev/null)" || epoch=""
  fi
  case "$epoch" in
    '' | *[!0-9]*)
      echo "repro-env: set SOURCE_DATE_EPOCH (no git history to read it from)" >&2
      return 1 ;;
  esac

  cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  if [ -d "$cargo_home" ]; then cargo_home="$(_repro_abs "$cargo_home")"; fi
  # Run from the workspace so rust-toolchain.toml selects the same toolchain cargo will use.
  sysroot="$(cd "$root" && rustc --print sysroot 2>/dev/null)" || sysroot=""

  # rustc applies the LAST matching --remap-path-prefix, so the broad prefixes come first and the
  # workspace (which may sit under one of them) and an explicit target dir come last.
  flags="${RUSTFLAGS:-}"
  for p in "$sysroot=/rustc-sysroot" "$cargo_home=/cargo" "$root=/build"; do
    case "$p" in =*) continue ;; esac
    flags="${flags:+$flags }--remap-path-prefix=$p"
  done
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    mkdir -p "$CARGO_TARGET_DIR" && target_dir="$(_repro_abs "$CARGO_TARGET_DIR")" || return 1
    case "$target_dir" in
      "$root"/* | "$root"\\*) ;; # already covered by the workspace mapping
      *) flags="$flags --remap-path-prefix=$target_dir=/target" ;;
    esac
  fi
  for p in "$root" "$cargo_home" "$sysroot" "${target_dir:-}"; do
    case "$p" in *[[:space:]]*)
      echo "repro-env: path contains whitespace, RUSTFLAGS cannot remap it: $p" >&2
      return 1 ;;
    esac
  done

  _REPRO_EPOCH="$epoch"
  _REPRO_FLAGS="$flags"
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  # Sourced: export into the caller's shell without changing its options.
  if _repro_env; then
    export SOURCE_DATE_EPOCH="$_REPRO_EPOCH" RUSTFLAGS="$_REPRO_FLAGS"
  fi
  unset -f _repro_env _repro_abs
  unset _REPRO_EPOCH _REPRO_FLAGS
else
  set -euo pipefail
  _repro_env
  case "${1:-}" in
    --github-env)
      : "${GITHUB_ENV:?--github-env needs GITHUB_ENV}"
      {
        echo "SOURCE_DATE_EPOCH=$_REPRO_EPOCH"
        echo "RUSTFLAGS=$_REPRO_FLAGS"
      } >> "$GITHUB_ENV"
      echo "SOURCE_DATE_EPOCH=$_REPRO_EPOCH"
      echo "RUSTFLAGS=$_REPRO_FLAGS" ;;
    '')
      printf 'export SOURCE_DATE_EPOCH=%q\n' "$_REPRO_EPOCH"
      printf 'export RUSTFLAGS=%q\n' "$_REPRO_FLAGS" ;;
    *)
      echo "usage: $0 [--github-env]   (or source it)" >&2
      exit 2 ;;
  esac
fi
