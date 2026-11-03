#!/usr/bin/env bash
# One command for the desktop's `cloud` end-to-end scenario: builds the server, the desktop's
# cloud shell and nuggetd, starts this stack (project "heed-e2e", its own volumes and ports) with
# fake Composio, Anthropic, OpenAI and Stripe, and drives the real desktop app against it: once
# with billing on (fake Stripe) and once with NUGGET_BILLING=off. Tears the stack down at the end.
#
#   selfhost/e2e.sh                      # both runs
#   E2E_CLOUD_RUNS=3 selfhost/e2e.sh     # repeat both runs three times
#   E2E_CLOUD_KEEP=1 selfhost/e2e.sh     # leave the stack up afterwards
#
# Needs Docker (compose v2), cargo, node and pnpm. Ports: 8010 (gateway), 8035 (Mailpit),
# 9100 (fake providers); override with E2E_CLOUD_PORT, E2E_MAILPIT_PORT, E2E_FAKE_PORT.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
port=${E2E_CLOUD_PORT:-8010}
mail_port=${E2E_MAILPIT_PORT:-8035}
fake_port=${E2E_FAKE_PORT:-9100}
runs=${E2E_CLOUD_RUNS:-1}
work=$(mktemp -d)
env_file="$work/.env"
override="$work/e2e.override.yml"
compose=(docker compose -p heed-e2e --env-file "$env_file" -f "$here/docker-compose.yml" -f "$override" --profile dev)
fake_pid=

cleanup() {
  [ -n "$fake_pid" ] && kill "$fake_pid" 2>/dev/null || true
  if [ -z "${E2E_CLOUD_KEEP:-}" ]; then
    "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$work"
  else
    echo "e2e: stack left up; env file $env_file"
  fi
}
trap cleanup EXIT

echo "e2e: building"
cd "$root"
cargo build -p nugget-cloud --bin nugget-cloud --example fake_providers
cargo build -p nugget-cloud-client --example e2e_shell
cargo build -p nuggetd
# The image is built once (Dockerfile.prebuilt); each run mounts the binary it just built.
docker image inspect heed-cloud:e2e >/dev/null 2>&1 ||
  docker build -q -f selfhost/Dockerfile.prebuilt --build-arg BINARY=target/debug/nugget-cloud -t heed-cloud:e2e . >/dev/null
printf 'services:\n  cloud:\n    volumes:\n      - %s:/usr/local/bin/nugget-cloud:ro\n' "$root/target/debug/nugget-cloud" >"$override"

webhook_secret="whsec_$(openssl rand -hex 16)"
{
  "$here/gen-keys.sh"
  cat <<EOF
PUBLIC_URL=http://localhost:$port
SITE_ADDRESS=:8000
PUBLIC_PORT=$port
MAILPIT_PORT=$mail_port
HEED_CLOUD_IMAGE=heed-cloud:e2e
HEED_ENV_FILE=$env_file
APP_REDIRECTS=nugget://auth/callback,heed://auth/callback
SMTP_HOST=mail
SMTP_PORT=1025
SMTP_SENDER=heed@localhost
COMPOSIO_API_KEY=fake
COMPOSIO_AUTH_CONFIGS=gmail=ac_gmail,googlecalendar=ac_cal
ANTHROPIC_API_KEY=fake
OPENAI_API_KEY=fake
COMPOSIO_BASE_URL=http://host.docker.internal:$fake_port
NUGGET_ANTHROPIC_BASE_URL=http://host.docker.internal:$fake_port
NUGGET_OPENAI_BASE_URL=http://host.docker.internal:$fake_port
NUGGET_STRIPE_BASE_URL=http://host.docker.internal:$fake_port
STRIPE_SECRET_KEY=sk_test_fake
STRIPE_WEBHOOK_SECRET=$webhook_secret
STRIPE_PRICE_HEED_MONTHLY=price_heedmonthly
STRIPE_PRICE_HEED_ANNUAL=price_heedannual
STRIPE_PRICE_HEED_MAX_MONTHLY=price_maxmonthly
STRIPE_PRICE_HEED_MAX_ANNUAL=price_maxannual
NUGGET_PUBLIC_URL=http://localhost:$port
NUGGET_WAITLIST_SALT=e2e
NUGGET_APP_SCHEME=nugget
NUGGET_BILLING=on
EOF
} >"$env_file"
set -a
# shellcheck disable=SC1090
. "$env_file"
set +a

FAKE_PROVIDERS_BIND=0.0.0.0:$fake_port FAKE_PUBLIC_URL=http://127.0.0.1:$fake_port FAKE_COMPOSIO_DELAY_MS=1500 \
  FAKE_STRIPE_WEBHOOK_URL=http://localhost:$port/v1/webhooks/stripe FAKE_STRIPE_WEBHOOK_SECRET=$webhook_secret \
  target/debug/examples/fake_providers >"$work/fake.log" 2>&1 &
fake_pid=$!

echo "e2e: starting the stack"
"${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
"${compose[@]}" up -d --wait --no-build db auth mail gateway cloud
wait_up() {
  for _ in $(seq 1 60); do
    curl -fsS "http://localhost:$port/healthz" >/dev/null 2>&1 && return 0
    sleep 1
  done
  "${compose[@]}" logs cloud | tail -50
  echo "e2e: the server did not come up" >&2
  return 1
}
wait_up

[ -d apps/desktop/node_modules ] || pnpm -C apps/desktop install --frozen-lockfile
export E2E_SCENARIOS=cloud
export NUGGETD="$root/target/debug/nuggetd"
export E2E_CLOUD_SHELL="$root/target/debug/examples/e2e_shell"
export E2E_CLOUD_URL="http://localhost:$port"
export E2E_CLOUD_ANON_KEY="$SUPABASE_ANON_KEY"
export E2E_MAILPIT_URL="http://localhost:$mail_port"
export E2E_FAKE_URL="http://127.0.0.1:$fake_port"
export E2E_CLOUD_PSQL="docker compose -p heed-e2e --env-file $env_file -f $here/docker-compose.yml -f $override exec -T db psql -U postgres -tAX -v ON_ERROR_STOP=1"

billing() {
  sed -i "s/^NUGGET_BILLING=.*/NUGGET_BILLING=$1/" "$env_file"
  "${compose[@]}" up -d --wait --no-build cloud >/dev/null
  wait_up
}

artifacts=${E2E_ARTIFACTS:-$root/apps/desktop/e2e/artifacts}
for i in $(seq 1 "$runs"); do
  for mode in on off; do
    echo "e2e: run $i/$runs, billing $mode"
    billing "$mode"
    E2E_CLOUD_BILLING=$mode E2E_ARTIFACTS="$artifacts/cloud-$mode" node apps/desktop/e2e/run.mjs || {
      echo "e2e: failed (run $i, billing $mode); fake providers log: $work/fake.log" >&2
      "${compose[@]}" logs --tail 80 cloud >&2 || true
      E2E_CLOUD_KEEP=${E2E_CLOUD_KEEP:-}
      exit 1
    }
  done
done
echo "e2e: cloud scenario passed ($runs x billing on and off)"
