#!/usr/bin/env bash
# Prints fresh secrets for .env: database passwords, the JWT secret, and the anon and
# service-role keys (HS256 JWTs signed with that secret, as Supabase issues them).
set -euo pipefail
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
rand() { openssl rand -hex 24; }
jwt_secret="$(openssl rand -hex 32)"
sign() {
  local header payload
  header=$(printf '{"alg":"HS256","typ":"JWT"}' | b64url)
  payload=$(printf '{"role":"%s","iss":"heed-selfhost","iat":%s,"exp":%s}' "$1" "$(date +%s)" "$(( $(date +%s) + 10*365*86400 ))" | b64url)
  printf '%s.%s.%s' "$header" "$payload" "$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -hmac "$jwt_secret" -binary | b64url)"
}
echo "POSTGRES_PASSWORD=$(rand)"
echo "AUTH_DB_PASSWORD=$(rand)"
echo "SERVICE_DB_PASSWORD=$(rand)"
echo "JWT_SECRET=$jwt_secret"
echo "SUPABASE_ANON_KEY=$(sign anon)"
echo "SUPABASE_SERVICE_ROLE_KEY=$(sign service_role)"
