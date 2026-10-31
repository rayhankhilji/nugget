# Shared settings for the release rehearsal scripts (sourced, not run).
#
# The rehearsal (.github/workflows/release-rehearsal.yml) cuts a complete release with throwaway
# keys that are generated in the job and never leave it. Everything lives under one state
# directory:
#
#   $REHEARSAL_DIR/keys/       PRIVATE: throwaway updater and manifest keys and their passwords.
#                              Never uploaded, never copied anywhere else.
#   $REHEARSAL_DIR/evidence/   public material and logs only; uploaded as the evidence artifact
#   $REHEARSAL_DIR/build/      the old and new installers (not uploaded)
#   $REHEARSAL_DIR/serve/      one directory per feed variant, served over HTTPS on localhost
#   $REHEARSAL_DIR/bin/        nuggetd built with the throwaway (and the wrong) manifest key
#   $REHEARSAL_DIR/tls/        the throwaway local CA and the localhost server certificate

# Native paths (D:/a/...) under Git Bash on Windows, so native tools get them unchanged.
native_pwd() { pwd -W 2>/dev/null || pwd; }
REHEARSAL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && native_pwd)"
REHEARSAL_SCRIPTS="$REHEARSAL_ROOT/packaging/scripts/rehearsal"
: "${REHEARSAL_DIR:=${RUNNER_TEMP:-${TMPDIR:-/tmp}}/rehearsal}"
mkdir -p "$REHEARSAL_DIR"
REHEARSAL_DIR="$(cd "$REHEARSAL_DIR" && native_pwd)"
KEYS="$REHEARSAL_DIR/keys"
PUB="$REHEARSAL_DIR/evidence/public-keys"
EVIDENCE="$REHEARSAL_DIR/evidence"

# Both versions carry a "-rehearsal" marker, and the feed URL baked into both builds points at
# localhost, so nothing built here could serve real users even if it escaped the job.
OLD_VERSION="${REHEARSAL_OLD_VERSION:-0.9.0-rehearsal.1}"
NEW_VERSION="${REHEARSAL_NEW_VERSION:-0.9.1-rehearsal.1}"
PORT="${REHEARSAL_PORT:-47443}"
FEED_BASE="https://localhost:$PORT/feed"
DIST_BASE="https://localhost:$PORT/dist"

TARGET="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$TARGET" in
  *windows*) EXE=".exe" ;;
  *) EXE="" ;;
esac

PY="$(command -v python3 || command -v python)"

rlog() { echo "rehearsal: $*"; }
rdie() {
  echo "rehearsal: FAIL: $*" >&2
  [ -z "${GITHUB_ACTIONS:-}" ] || echo "::error::release rehearsal: $*"
  exit 1
}

# record CHECK PASS|FAIL|INFO DETAIL: one row of the evidence report.
record() {
  mkdir -p "$EVIDENCE"
  local detail="${3:-}"
  detail="${detail//$'\t'/ }"
  detail="${detail//$'\n'/ }"
  printf '%s\t%s\t%s\n' "$1" "$2" "$detail" >>"$EVIDENCE/results.tsv"
  echo "rehearsal: [$2] $1${3:+: $detail}"
}

# Hide a secret from the GitHub log (it is never printed, but be safe).
mask() { [ -z "${GITHUB_ACTIONS:-}" ] || echo "::add-mask::$1"; }

sha256() { sha256sum "$1" | cut -d' ' -f1; }
