#!/usr/bin/env bash
# Apply the shim and every migration to a fresh database, then run the SQL checks.
# Usage: PGHOST=/tmp PGPORT=54329 PGUSER=postgres supabase/tests/run.sh [dbname]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
db="${1:-nugget_test}"
dropdb --if-exists "$db"
createdb "$db"
psql -q -v ON_ERROR_STOP=1 -d "$db" -f "$here/supabase_shim.sql"
for m in "$here"/../migrations/*.sql; do
  psql -q -v ON_ERROR_STOP=1 -d "$db" -f "$m"
done
psql -q -v ON_ERROR_STOP=1 -d "$db" -f "$here/rls.sql"
psql -q -v ON_ERROR_STOP=1 -d "$db" -f "$here/hardening.sql"
