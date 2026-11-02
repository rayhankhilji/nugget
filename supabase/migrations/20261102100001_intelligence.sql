-- M2/M3 intelligence: knowledge graph projection, "Needs you" cards, the morning brief.
-- Same RLS shape as the M1 tables: users read and delete their own rows, the service is scoped
-- per transaction by nugget.user_id, and cross-user lookups go through nugget_private definers.

-- The user's IANA time zone (sent by the desktop), for "today" and the 07:00 brief.
alter table public.profiles add column timezone text;

-- For "items changed since the last intelligence run" (nugget_private.intel_due).
create index items_user_updated on public.items (user_id, updated_at);

-- Entities: aliases and identifiers in metadata; the graph id keeps the projection stable.
alter table public.entities add column graph_id text;

-- Claims: status from the graph, and every piece of evidence ({item_id, quote}); item_id is the
-- first evidence item.
alter table public.claims
  add column status text not null default 'active',
  add column evidence jsonb not null default '[]'::jsonb;

-- Cards: the full card from nugget-proactive (title_text/detail_text LocalText, evidence,
-- factors, actions) keyed by its stable id, and the proactive state machine's statuses.
alter table public.cards drop constraint cards_status_check;
alter table public.cards add constraint cards_status_check
  check (status in ('new', 'seen', 'snoozed', 'accepted', 'dismissed', 'done', 'expired', 'acted'));
alter table public.cards
  add column card_key text,
  add column data jsonb not null default '{}'::jsonb,
  add column snoozed_until timestamptz,
  add column updated_at timestamptz not null default now();
create unique index cards_user_key on public.cards (user_id, card_key);

-- The proactive state (cards, scoring model, learned per-sender/domain/kind feedback with decay).
create table public.proactive_state (
  user_id uuid primary key references auth.users (id) on delete cascade,
  state jsonb not null,
  updated_at timestamptz not null default now()
);

-- When the knowledge and card jobs last ran for the user, and on which items.
create table public.intel_runs (
  user_id uuid primary key references auth.users (id) on delete cascade,
  items_through timestamptz,
  ran_at timestamptz not null default now(),
  stats jsonb not null default '{}'::jsonb,
  last_error text
);

-- Model answers for extraction prompts, so unchanged items are never paid for twice.
create table public.llm_cache (
  user_id uuid not null references auth.users (id) on delete cascade,
  key text not null check (key ~ '^[0-9a-f]{64}$'),
  response text not null,
  input_tokens int not null default 0,
  output_tokens int not null default 0,
  created_at timestamptz not null default now(),
  primary key (user_id, key)
);

-- One morning brief per user per local day.
create table public.briefs (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  day date not null,
  timezone text not null,
  prose text,
  citations jsonb not null default '[]'::jsonb,
  data jsonb not null,
  created_at timestamptz not null default now(),
  unique (user_id, day)
);

do $$
declare
  t text;
begin
  foreach t in array array['proactive_state', 'intel_runs', 'llm_cache', 'briefs']
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

-- Users whose items changed since their last intelligence run, or whose last run is older than
-- `stale` (cards depend on the clock: meetings come up, deadlines pass), oldest run first.
create or replace function nugget_private.intel_due(max_rows int, stale interval)
returns table (user_id uuid)
language sql stable security definer set search_path = '' as $$
  select p.user_id from public.profiles p
  left join public.intel_runs r on r.user_id = p.user_id
  where exists (select 1 from public.items i where i.user_id = p.user_id)
    and (r.user_id is null
         or r.ran_at < now() - stale
         or exists (select 1 from public.items i
                    where i.user_id = p.user_id and i.updated_at > coalesce(r.items_through, '-infinity')))
  order by r.ran_at nulls first
  limit max_rows
$$;

-- Users whose local time is past 07:00 and who have no brief for their local today.
create or replace function nugget_private.brief_due(max_rows int)
returns table (user_id uuid, timezone text)
language sql stable security definer set search_path = '' as $$
  select p.user_id, coalesce(p.timezone, 'UTC') from public.profiles p
  where exists (select 1 from public.items i where i.user_id = p.user_id)
    and extract(hour from now() at time zone coalesce(p.timezone, 'UTC')) >= 7
    and not exists (select 1 from public.briefs b
                    where b.user_id = p.user_id
                      and b.day = (now() at time zone coalesce(p.timezone, 'UTC'))::date)
  limit max_rows
$$;

revoke all on function nugget_private.intel_due(int, interval) from public;
revoke all on function nugget_private.brief_due(int) from public;
grant execute on function nugget_private.intel_due(int, interval) to nugget_service;
grant execute on function nugget_private.brief_due(int) to nugget_service;
