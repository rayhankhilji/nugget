#!/bin/sh
# Usage: render_check.sh SCRATCH_DIR  (SCRATCH_DIR has react@18.3.1 react-dom@18.3.1 esbuild@0.23.1
# installed and stubs/tauri.js exporting invoke/listen stubs). Bundles render_check.tsx and runs it.
set -e
S="$1"; HERE=$(cd "$(dirname "$0")" && pwd)
"$S/node_modules/.bin/esbuild" "$HERE/render_check.tsx" --bundle --platform=node --format=cjs \
  --jsx=automatic --define:import.meta.glob=globalThis.__glob --banner:js="globalThis.__glob=()=>({});" --loader:.css=empty --loader:.json=json \
  --alias:@tauri-apps/api/core="$S/stubs/tauri.js" --alias:@tauri-apps/api/event="$S/stubs/tauri.js" \
  --alias:react="$S/node_modules/react" --alias:react-dom="$S/node_modules/react-dom" \
  --outfile="$S/render_check.cjs" --log-level=warning
cd "$S" && node render_check.cjs
