#!/bin/sh
# Assemble a scratch copy of qa/GG3/probes (never edited) with the root Cargo.lock, absolute
# path deps and the MM1 probe p8_dd3, then build it into the shared target dir.
# Usage: qa/MM1/gg3/probes-mm1/build.sh <scratch-dir>   -> <target>/release/gg3-probes
set -eu
REPO=$(cd "$(dirname "$0")/../../../.." && pwd)
S=${1:?scratch dir}
rm -rf "$S/probes" && cp -r "$REPO/qa/GG3/probes" "$S/probes"
cd "$S/probes"
sed -i "s#path = \"../../../#path = \"$REPO/#" Cargo.toml
cp "$REPO/Cargo.lock" Cargo.lock                      # qa/GG3's copy is stale vs the root
cp "$REPO/qa/MM1/gg3/probes-mm1/p8_dd3.rs" src/p8_dd3.rs
sed -i 's/^mod rig;/mod rig;\nmod p8_dd3;/' src/main.rs
sed -i 's/("p7_triage", probes::p7_triage),/("p7_triage", probes::p7_triage),\n        ("p8_dd3", p8_dd3::p8_dd3),/' src/main.rs
export CARGO_INCREMENTAL=0 NUGGET_TRUST_DEV_MANIFEST_KEY=1
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$REPO/target} nice -n 10 cargo build --release
