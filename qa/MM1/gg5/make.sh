#!/bin/sh
# Rebuild MM1's adapted GG5 harness copies from the originals + patches (keeps qa/MM1 small).
# Usage: sh qa/MM1/gg5/make.sh   -> qa/MM1/gg5/nugget_qa_mm1.py, qa/MM1/gg5/vault_edits_mm1.py
set -eu
cd "$(dirname "$0")/../../.."
cp qa/GG5/harness/nugget_qa.py qa/MM1/gg5/nugget_qa_mm1.py && patch -s qa/MM1/gg5/nugget_qa_mm1.py qa/MM1/gg5/nugget_qa_mm1.patch
cp qa/GG5/scenarios/vault-edits/run_main.py qa/MM1/gg5/vault_edits_mm1.py && patch -s qa/MM1/gg5/vault_edits_mm1.py qa/MM1/gg5/vault_edits_mm1.patch
