-- Nugget Cloud M1 schema: per-user data with row-level security everywhere.
--
-- Roles:
--   authenticated   a user's own JWT through PostgREST: may only SELECT and DELETE their own rows.
--   nugget_service  the Rust cloud service: subject to RLS, scoped per transaction by
--                   set_config('nugget.user_id', <uid>, true). Cross-user lookups go through
--                   SECURITY DEFINER functions in nugget_private.

create extension if not exists vector with schema extensions;

do $$
begin
  if not exists (select 1 from pg_roles where rolname = 'nugget_service') then
    -- NOLOGIN here; the operator enables login with a password outside the repo.
    create role nugget_service nologin;
  end if;
end $$;

grant usage on schema public to nugget_service;
grant usage on schema extensions to nugget_service;

create schema if not exists nugget_private;
revoke all on schema nugget_private from public;
grant usage on schema nugget_private to nugget_service;

-- The user a service transaction acts for (null outside one).
create or replace function nugget_private.current_user_id() returns uuid
language sql stable as $$
  select nullif(current_setting('nugget.user_id', true), '')::uuid
$$;
grant execute on function nugget_private.current_user_id() to nugget_service;

-- ---------------------------------------------------------------------------------------------
-- Tables
-- ---------------------------------------------------------------------------------------------

create table public.profiles (
  user_id uuid primary key references auth.users (id) on delete cascade,
  plan text not null default 'free' check (plan in ('free', 'pro')),
  last_active_at timestamptz,
  created_at timestamptz not null default now()
);

create table public.connections (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  toolkit text not null check (toolkit ~ '^[a-z0-9_]{1,64}$'),
  composio_account_id text not null,
  status text not null default 'active' check (status in ('initiated', 'active', 'failed', 'disabled')),
  sync_cursor jsonb not null default '{}'::jsonb,
  last_synced_at timestamptz,
  next_sync_at timestamptz not null default now(),
  last_error text,
  created_at timestamptz not null default now(),
  unique (composio_account_id)
);
create index connections_user on public.connections (user_id);
create index connections_due on public.connections (next_sync_at) where status = 'active';

create table public.items (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  connection_id uuid references public.connections (id) on delete cascade,
  source text not null,
  external_id text not null,
  kind text not null check (kind in ('email', 'event', 'doc')),
  title text,
  author text,
  url text,
  occurred_at timestamptz,
  body text not null default '',
  metadata jsonb not null default '{}'::jsonb,
  content_hash text not null,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (user_id, source, external_id)
);
create index items_user_time on public.items (user_id, occurred_at desc);

create table public.chunks (
  id bigint generated always as identity primary key,
  user_id uuid not null references auth.users (id) on delete cascade,
  item_id uuid not null references public.items (id) on delete cascade,
  ord int not null,
  text text not null,
  tsv tsvector generated always as (to_tsvector('simple', text)) stored,
  embedding extensions.vector(1536),
  unique (item_id, ord)
);
create index chunks_user on public.chunks (user_id);
create index chunks_tsv on public.chunks using gin (tsv);
create index chunks_embedding on public.chunks using hnsw (embedding extensions.vector_cosine_ops);

create table public.entities (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  kind text not null,
  name text not null,
  canonical text not null,
  metadata jsonb not null default '{}'::jsonb,
  created_at timestamptz not null default now(),
  unique (user_id, kind, canonical)
);

create table public.claims (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  subject_id uuid not null references public.entities (id) on delete cascade,
  predicate text not null,
  object_text text,
  object_id uuid references public.entities (id) on delete cascade,
  item_id uuid not null references public.items (id) on delete cascade,
  confidence real not null default 0.5 check (confidence between 0 and 1),
  valid_from timestamptz,
  valid_to timestamptz,
  created_at timestamptz not null default now()
);
create index claims_subject on public.claims (user_id, subject_id);

create table public.cards (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  kind text not null,
  title text not null,
  body text not null default '',
  item_ids uuid[] not null default '{}',
  score real not null default 0,
  status text not null default 'new' check (status in ('new', 'seen', 'dismissed', 'acted')),
  due_at timestamptz,
  created_at timestamptz not null default now()
);
create index cards_user on public.cards (user_id, status, created_at desc);

create table public.conversations (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  title text,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table public.messages (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  conversation_id uuid not null references public.conversations (id) on delete cascade,
  role text not null check (role in ('user', 'assistant')),
  content text not null,
  citations jsonb not null default '[]'::jsonb,
  created_at timestamptz not null default now()
);
create index messages_conv on public.messages (conversation_id, created_at);

create table public.usage_events (
  id bigint generated always as identity primary key,
  user_id uuid not null references auth.users (id) on delete cascade,
  kind text not null check (kind in ('llm', 'embed', 'tool')),
  model text not null default '',
  input_tokens int not null default 0,
  output_tokens int not null default 0,
  cost_micros bigint not null default 0,
  created_at timestamptz not null default now()
);
create index usage_user_time on public.usage_events (user_id, created_at);

-- ---------------------------------------------------------------------------------------------
-- RLS: same shape on every table.
-- ---------------------------------------------------------------------------------------------

do $$
declare
  t text;
begin
  foreach t in array array['profiles', 'connections', 'items', 'chunks', 'entities', 'claims',
                           'cards', 'conversations', 'messages', 'usage_events']
  loop
    execute format('alter table public.%I enable row level security', t);
    execute format('revoke all on public.%I from anon, authenticated', t);
    execute format('grant select, delete on public.%I to authenticated', t);
    execute format('grant select, insert, update, delete on public.%I to nugget_service', t);
    execute format($p$create policy own_rows_read on public.%I for select to authenticated
                      using (user_id = (select auth.uid()))$p$, t);
    execute format($p$create policy own_rows_delete on public.%I for delete to authenticated
                      using (user_id = (select auth.uid()))$p$, t);
    execute format($p$create policy service_scoped on public.%I for all to nugget_service
                      using (user_id = (select nugget_private.current_user_id()))
                      with check (user_id = (select nugget_private.current_user_id()))$p$, t);
  end loop;
end $$;

-- Only the service decides plans; a user deleting their profile row would be pointless.
revoke delete on public.profiles from authenticated;
-- The metering ledger is append-only for the service and read-only for the user.
revoke update, delete on public.usage_events from nugget_service, authenticated;

-- ---------------------------------------------------------------------------------------------
-- Functions
-- ---------------------------------------------------------------------------------------------

-- A profile for every new auth user.
create or replace function nugget_private.handle_new_user() returns trigger
language plpgsql security definer set search_path = '' as $$
begin
  insert into public.profiles (user_id) values (new.id) on conflict do nothing;
  return new;
end $$;
create trigger on_auth_user_created after insert on auth.users
  for each row execute function nugget_private.handle_new_user();

-- Which user owns a Composio connected account (webhooks arrive without a user).
create or replace function nugget_private.connection_owner(account_id text)
returns table (user_id uuid, connection_id uuid)
language sql stable security definer set search_path = '' as $$
  select c.user_id, c.id from public.connections c where c.composio_account_id = account_id
$$;

-- Active connections whose next sync is due, oldest first.
create or replace function nugget_private.due_connections(max_rows int)
returns table (user_id uuid, connection_id uuid, toolkit text)
language sql stable security definer set search_path = '' as $$
  select c.user_id, c.id, c.toolkit from public.connections c
  where c.status = 'active' and c.next_sync_at <= now()
  order by c.next_sync_at limit max_rows
$$;

revoke all on function nugget_private.connection_owner(text) from public;
revoke all on function nugget_private.due_connections(int) from public;
grant execute on function nugget_private.connection_owner(text) to nugget_service;
grant execute on function nugget_private.due_connections(int) to nugget_service;

-- Storage: per-user prefix in the private exports bucket (Supabase only; skipped elsewhere).
do $$
begin
  if exists (select 1 from pg_namespace where nspname = 'storage')
     and exists (select 1 from pg_tables where schemaname = 'storage' and tablename = 'buckets') then
    insert into storage.buckets (id, name, public) values ('exports', 'exports', false)
      on conflict (id) do nothing;
    execute $p$create policy exports_own_read on storage.objects for select to authenticated
      using (bucket_id = 'exports' and (storage.foldername(name))[1] = (select auth.uid())::text)$p$;
  end if;
end $$;
