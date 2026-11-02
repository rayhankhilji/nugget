#!/usr/bin/env bash
# First start of the database only: the roles and schemas a Supabase project would have, which
# GoTrue and the Heed migrations expect. Passwords come from the environment, never this file.
set -euo pipefail
psql -v ON_ERROR_STOP=1 --username postgres --dbname postgres \
  -v auth_pw="$AUTH_DB_PASSWORD" <<'SQL'
create role anon nologin noinherit;
create role authenticated nologin noinherit;
create role service_role nologin noinherit bypassrls;
create role supabase_auth_admin login noinherit createrole password :'auth_pw';
create schema auth authorization supabase_auth_admin;
alter role supabase_auth_admin set search_path = auth;
grant usage on schema auth to anon, authenticated, service_role;
create schema extensions;
create extension vector with schema extensions;
grant usage on schema extensions to anon, authenticated, service_role;
SQL
