#!/usr/bin/env bash
# Build Nugget's first-party plugins and the SDK example plugins as release WebAssembly
# components, stage each as a package directory, and (when a signing key is set) sign each into a
# single `.nuggetplugin` file.
#
# Usage: plugins/build.sh [--out DIR] [--check] [--sign]
#
#   --out DIR   Where packages go (default: <repo>/target/plugin-packages). For each plugin:
#                 DIR/<id>/nugget-plugin.toml
#                 DIR/<id>/plugin.wasm
#                 DIR/<id>/plugin.sig          (only when signed)
#                 DIR/<id>.nuggetplugin        (only when signed)
#   --check     First run, in the SDK workspace, plugins/ and plugins/template: cargo fmt --check,
#               cargo clippy for wasm32-wasip2 with -D warnings, and the native tests
#               (`cargo test --target host-tuple`).
#   --sign      Require signing: fail unless a signing key is set (see below).
#
# Environment:
#   NUGGET_PLUGIN_SIGNING_KEY  hex ed25519 seed, or
#   NUGGET_PLUGIN_SIGNING_KEY_FILE  a file holding it. When either is set, every package is
#                              signed by the `sign-plugin` example of nugget-plugin. The signer is
#                              compiled first with both variables removed from its environment,
#                              so no build script ever sees the key. CI uses a throwaway key
#                              generated per run; never a release key.
#   SDK_TARGET_DIR             cargo target dir for the SDK workspace
#                              (default: <repo>/target/plugin-guests)
#   PLUGINS_TARGET_DIR         cargo target dir for plugins/ and the template
#                              (default: <repo>/target/plugins)
#   CARGO_FLAGS                extra flags for the cargo build/clippy/test calls, e.g. --locked
#
# Prints one line per package: `<id> <path>` (the .nuggetplugin when signed, else the directory).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
sdk="$repo/crates/nugget-plugin/sdk"
out="$repo/target/plugin-packages"
check=0
require_sign=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --out=*) out="${1#--out=}"; shift ;;
        --check) check=1; shift ;;
        --sign) require_sign=1; shift ;;
        -h|--help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | grep '^#' | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
done

sdk_target="${SDK_TARGET_DIR:-$repo/target/plugin-guests}"
plugins_target="${PLUGINS_TARGET_DIR:-$repo/target/plugins}"
# Split CARGO_FLAGS on whitespace without glob expansion.
flags=()
set -f
read -r -a flags <<< "${CARGO_FLAGS:-}"
set +f

sign=0
if [[ -n "${NUGGET_PLUGIN_SIGNING_KEY:-}" ]]; then
    sign=1
elif [[ -n "${NUGGET_PLUGIN_SIGNING_KEY_FILE:-}" ]]; then
    if [[ ! -r "$NUGGET_PLUGIN_SIGNING_KEY_FILE" ]]; then
        echo "build.sh: NUGGET_PLUGIN_SIGNING_KEY_FILE ($NUGGET_PLUGIN_SIGNING_KEY_FILE) is not readable" >&2
        exit 1
    fi
    sign=1
fi
if [[ $require_sign -eq 1 && $sign -eq 0 ]]; then
    echo "build.sh: --sign needs NUGGET_PLUGIN_SIGNING_KEY or NUGGET_PLUGIN_SIGNING_KEY_FILE" >&2
    exit 1
fi
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# workspace dir, target dir
workspaces=("$sdk|$sdk_target" "$here|$plugins_target" "$here/template|$plugins_target")

if [[ $check -eq 1 ]]; then
    for ws in "${workspaces[@]}"; do
        dir="${ws%%|*}"
        target="${ws#*|}"
        echo "== checking $dir" >&2
        (
            cd "$dir"
            export CARGO_TARGET_DIR="$target"
            cargo fmt --all --check
            cargo clippy ${flags[@]+"${flags[@]}"} --release --workspace --target wasm32-wasip2 -- -D warnings
            cargo test ${flags[@]+"${flags[@]}"} --workspace --target host-tuple
        )
    done
fi

# Build every workspace once (the template too, so it never rots), for wasm32-wasip2.
for ws in "${workspaces[@]}"; do
    dir="${ws%%|*}"
    target="${ws#*|}"
    echo "== building $dir" >&2
    (cd "$dir" && CARGO_TARGET_DIR="$target" cargo build ${flags[@]+"${flags[@]}"} --release --workspace --target wasm32-wasip2)
done

# manifest dir, wasm file: the packages to stage. The template and the hostile test guest are
# not shipped.
packages=(
    "$sdk/examples/feed-reader|$sdk_target/wasm32-wasip2/release/feed_reader.wasm"
    "$sdk/examples/text-tools|$sdk_target/wasm32-wasip2/release/text_tools.wasm"
    "$here/rss|$plugins_target/wasm32-wasip2/release/nugget_rss.wasm"
    "$here/obsidian|$plugins_target/wasm32-wasip2/release/nugget_obsidian.wasm"
    "$here/read-later|$plugins_target/wasm32-wasip2/release/nugget_read_later.wasm"
)

signer=""
if [[ $sign -eq 1 ]]; then
    # Compile the signer without the key in the environment, then run the binary with it.
    echo "== building sign-plugin" >&2
    (cd "$repo" && env -u CARGO_TARGET_DIR -u NUGGET_PLUGIN_SIGNING_KEY -u NUGGET_PLUGIN_SIGNING_KEY_FILE \
        cargo build ${flags[@]+"${flags[@]}"} --quiet -p nugget-plugin --example sign-plugin)
    signer="$repo/target/debug/examples/sign-plugin"
    [[ -x "$signer" ]] || signer="$signer.exe"
    if [[ ! -x "$signer" ]]; then
        echo "build.sh: sign-plugin was not built at $repo/target/debug/examples" >&2
        exit 1
    fi
fi

mkdir -p "$out"
for p in "${packages[@]}"; do
    dir="${p%%|*}"
    wasm="${p#*|}"
    id="$(sed -n 's/^id[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$dir/nugget-plugin.toml" | head -n1)"
    if [[ -z "$id" ]]; then
        echo "build.sh: no id in $dir/nugget-plugin.toml" >&2
        exit 1
    fi
    if [[ ! -f "$wasm" ]]; then
        echo "build.sh: $wasm was not built" >&2
        exit 1
    fi
    stage="$out/$id"
    rm -rf "$stage" "$out/$id.nuggetplugin"
    mkdir -p "$stage"
    cp "$dir/nugget-plugin.toml" "$stage/nugget-plugin.toml"
    cp "$wasm" "$stage/plugin.wasm"
    if [[ $sign -eq 1 ]]; then
        "$signer" "$stage" --out "$out/$id.nuggetplugin" >&2
        echo "$id $out/$id.nuggetplugin"
    else
        echo "$id $stage"
    fi
done
