-- Reserve-then-settle for AI spend on the model endpoint (/llm/v1). Before a request goes to
-- the provider, its worst-case cost is reserved against the user's limit; when it ends, the
-- real usage is recorded and the reservation released, in one transaction. Concurrent
-- requests therefore cannot jointly pass the limit (they could with a cached month total).
--
-- Month spend stays what it was: the sum of this month's usage_events (excluding tool calls).
-- A per-user advisory lock serialises reserve and settle, so each sees the other's effects.

create table public.ai_reservations (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  micros bigint not null check (micros >= 0),
  created_at timestamptz not null default now(),
  -- A crashed server's reservations stop counting after this.
  expires_at timestamptz not null
);
create index ai_reservations_user on public.ai_reservations (user_id, expires_at);

alter table public.ai_reservations enable row level security;
revoke all on public.ai_reservations from public, anon, authenticated;
grant select, insert, delete on public.ai_reservations to nugget_service;
create policy service_scoped on public.ai_reservations for all to nugget_service
  using (user_id = (select nugget_private.current_user_id()))
  with check (user_id = (select nugget_private.current_user_id()));

-- Reserves `estimate` micro-USD for `uid` if this month's spend plus open reservations plus
-- the estimate stays within `cap`. Returns the reservation id, or null when it does not fit.
create or replace function nugget_private.ai_reserve(uid uuid, estimate bigint, cap bigint, hold_secs int)
returns uuid
language plpgsql security definer set search_path = '' as $$
declare
  spent bigint;
  held bigint;
  rid uuid;
begin
  perform pg_advisory_xact_lock(hashtextextended('heed.ai:' || uid::text, 0));
  delete from public.ai_reservations where user_id = uid and expires_at <= now();
  select coalesce(sum(cost_micros), 0) into spent from public.usage_events
    where user_id = uid and kind <> 'tool' and created_at >= date_trunc('month', now());
  select coalesce(sum(micros), 0) into held from public.ai_reservations where user_id = uid;
  if spent + held + greatest(estimate, 0) > cap then
    return null;
  end if;
  insert into public.ai_reservations (user_id, micros, expires_at)
    values (uid, greatest(estimate, 0), now() + make_interval(secs => greatest(hold_secs, 60)))
    returning id into rid;
  return rid;
end $$;

-- Records a finished request's usage and releases its reservation, atomically.
create or replace function nugget_private.ai_settle(
  rid uuid, uid uuid, model text, route text,
  input_tokens int, output_tokens int, cache_read int, cache_write int, cost bigint)
returns void
language plpgsql security definer set search_path = '' as $$
begin
  perform pg_advisory_xact_lock(hashtextextended('heed.ai:' || uid::text, 0));
  insert into public.usage_events
    (user_id, kind, model, input_tokens, output_tokens, cost_micros, route, cache_read_tokens, cache_write_tokens)
    values (uid, 'llm', model, input_tokens, output_tokens, greatest(cost, 0), route, cache_read, cache_write);
  delete from public.ai_reservations where id = rid and user_id = uid;
end $$;

-- Releases a reservation that was never used (the request failed before any usage).
create or replace function nugget_private.ai_release(rid uuid, uid uuid) returns void
language sql security definer set search_path = '' as $$
  delete from public.ai_reservations where id = rid and user_id = uid
$$;

revoke all on function nugget_private.ai_reserve(uuid, bigint, bigint, int) from public;
revoke all on function nugget_private.ai_settle(uuid, uuid, text, text, int, int, int, int, bigint) from public;
revoke all on function nugget_private.ai_release(uuid, uuid) from public;
grant execute on function nugget_private.ai_reserve(uuid, bigint, bigint, int) to nugget_service;
grant execute on function nugget_private.ai_settle(uuid, uuid, text, text, int, int, int, int, bigint) to nugget_service;
grant execute on function nugget_private.ai_release(uuid, uuid) to nugget_service;
