#!/usr/bin/env bash
# GG4 real-model run (GitHub runner): install the manifest's models once on the real clock, then
# replay Maya's two weeks and the action/brief scenarios with [runtime] on and the judge local.
# Usage: qa/GG4/ci/run.sh OUT CHAT_MODEL EMBED_MODEL [SUITES]
set -uo pipefail
OUT=$(realpath -m "$1"); CHAT=${2:-qwen3-1.7b-q8}; EMB=${3:-nomic-embed-text-v1.5-q8}
SUITES=${4:-"install replay actions brief judge"}
MODELS=${GG4_MODELS_HOME:-$PWD/qa/GG4/.models}
mkdir -p "$OUT" "$MODELS"
export GG4_NUGGETD=${GG4_NUGGETD:-$PWD/target/release/nuggetd}
cd qa/GG4
rc=0
for s in $SUITES; do
  echo "::group::GG4 $s"
  case $s in
    install) python3 -u ci/install_models.py --home "$OUT/install-home" --models-home "$MODELS" --chat "$CHAT" --embed "$EMB" || rc=1 ;;
    replay) python3 -u scenarios/replay.py --out "$OUT/replay" --user --runtime --chat-model "$CHAT" --embed-model "$EMB" --models-home "$MODELS" --judge local --brief-wait 600 || rc=1 ;;
    *) if [ -f "scenarios/$s.py" ]; then
         python3 -u "scenarios/$s.py" --out "$OUT/$s" --runtime --chat-model "$CHAT" --embed-model "$EMB" --models-home "$MODELS" || rc=1
       fi ;;
  esac
  echo "::endgroup::"
done
exit $rc
