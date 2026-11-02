#!/usr/bin/env bash
# Applies every supabase/migrations/*.sql once, in name order, recording each in
# selfhost.schema_migrations. Safe to run again: applied files are skipped. Then lets the
# nugget_service role log in with SERVICE_DB_PASSWORD.
set -euo pipefail
export PGDATABASE=postgres
until pg_isready -q; do sleep 1; done
psql -v ON_ERROR_STOP=1 -q <<'SQL'
create schema if not exists selfhost;
create table if not exists selfhost.schema_migrations (name text primary key, applied_at timestamptz not null default now());
revoke all on schema selfhost from public;
-- GoTrue creates auth.uid() in older versions only; the Heed policies need it.
do $$ begin
  if not exists (select 1 from pg_proc p join pg_namespace n on n.oid = p.pronamespace
                 where n.nspname = 'auth' and p.proname = 'uid') then
    execute $f$create function auth.uid() returns uuid language sql stable as
      'select nullif(coalesce(current_setting(''request.jwt.claim.sub'', true),
                     (current_setting(''request.jwt.claims'', true)::jsonb ->> ''sub'')), '''')::uuid'$f$;
  end if;
end $$;
grant execute on function auth.uid() to anon, authenticated;
SQL
for f in /migrations/*.sql; do
  name="$(basename "$f")"
  if [ "$(psql -tAc "select 1 from selfhost.schema_migrations where name = '$name'")" = "1" ]; then
    continue
  fi
  echo "applying $name"
  psql -v ON_ERROR_STOP=1 -q --single-transaction -f "$f" \
    -c "insert into selfhost.schema_migrations (name) values ('$name')"
done
psql -v ON_ERROR_STOP=1 -q -v pw="$SERVICE_DB_PASSWORD" <<'SQL'
alter role nugget_service with login password :'pw';
SQL
echo "migrations: up to date"
