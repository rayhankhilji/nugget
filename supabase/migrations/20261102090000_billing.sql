-- Heed billing: Stripe customers and subscriptions, processed webhook events, devices.
-- There is no free plan: the entitlement comes from an active or trialing subscription.

alter table public.profiles drop column if exists plan;

create table public.billing_customers (
  user_id uuid primary key references auth.users (id) on delete cascade,
  stripe_customer_id text not null unique,
  created_at timestamptz not null default now()
);

create table public.subscriptions (
  id text primary key, -- Stripe subscription id
  user_id uuid not null references auth.users (id) on delete cascade,
  plan text not null,
  interval text not null check (interval in ('month', 'year')),
  status text not null,
  stripe_price_id text not null,
  trial_end timestamptz,
  current_period_end timestamptz,
  cancel_at_period_end boolean not null default false,
  updated_at timestamptz not null default now()
);
create index subscriptions_user on public.subscriptions (user_id);

create table public.devices (
  user_id uuid not null references auth.users (id) on delete cascade,
  device_id text not null check (length(device_id) between 8 and 64),
  first_seen_at timestamptz not null default now(),
  last_seen_at timestamptz not null default now(),
  primary key (user_id, device_id)
);

do $$
declare
  t text;
begin
  foreach t in array array['billing_customers', 'subscriptions', 'devices']
  loop
    execute format('alter table public.%I enable row level security', t);
    execute format('revoke all on public.%I from anon, authenticated', t);
    execute format('grant select on public.%I to authenticated', t);
    execute format('grant select, insert, update, delete on public.%I to nugget_service', t);
    execute format($p$create policy own_rows_read on public.%I for select to authenticated
                      using (user_id = (select auth.uid()))$p$, t);
    execute format($p$create policy service_scoped on public.%I for all to nugget_service
                      using (user_id = (select nugget_private.current_user_id()))
                      with check (user_id = (select nugget_private.current_user_id()))$p$, t);
  end loop;
end $$;
-- Users may forget their own devices (frees a seat); billing rows are Stripe's to change.
grant delete on public.devices to authenticated;
create policy own_rows_delete on public.devices for delete to authenticated
  using (user_id = (select auth.uid()));

-- Stripe events already applied (idempotency). Private: no API access at all.
create table nugget_private.stripe_events (
  id text primary key,
  type text not null,
  processed_at timestamptz not null default now()
);

create or replace function nugget_private.stripe_event_seen(event_id text) returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (select 1 from nugget_private.stripe_events where id = event_id)
$$;

create or replace function nugget_private.mark_stripe_event(event_id text, event_type text) returns void
language sql security definer set search_path = '' as $$
  insert into nugget_private.stripe_events (id, type) values (event_id, event_type) on conflict do nothing
$$;

-- Which user a Stripe customer belongs to (webhooks arrive without a user).
create or replace function nugget_private.customer_owner(customer_id text) returns uuid
language sql stable security definer set search_path = '' as $$
  select user_id from public.billing_customers where stripe_customer_id = customer_id
$$;

revoke all on function nugget_private.stripe_event_seen(text) from public;
revoke all on function nugget_private.mark_stripe_event(text, text) from public;
revoke all on function nugget_private.customer_owner(text) from public;
grant execute on function nugget_private.stripe_event_seen(text) to nugget_service;
grant execute on function nugget_private.mark_stripe_event(text, text) to nugget_service;
grant execute on function nugget_private.customer_owner(text) to nugget_service;

-- Sync only for users with a live subscription.
create or replace function nugget_private.due_connections(max_rows int)
returns table (user_id uuid, connection_id uuid, toolkit text)
language sql stable security definer set search_path = '' as $$
  select c.user_id, c.id, c.toolkit from public.connections c
  where c.status = 'active' and c.next_sync_at <= now()
    and exists (select 1 from public.subscriptions s
                where s.user_id = c.user_id and s.status in ('active', 'trialing', 'past_due'))
  order by c.next_sync_at limit max_rows
$$;
