-- RLS isolation checks. Run by supabase/tests/run.sh after the shim and migrations.
-- Any failed assertion raises and aborts with ON_ERROR_STOP.
\set ON_ERROR_STOP on
insert into auth.users (id, email) values
  ('00000000-0000-0000-0000-00000000000a', 'a@example.com'),
  ('00000000-0000-0000-0000-00000000000b', 'b@example.com');

do $$ begin
  assert (select count(*) from public.profiles) = 2, 'a profile per new user';
end $$;

-- The service writes as A, then as B.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.items (user_id, source, external_id, kind, title, body, content_hash)
  values ('00000000-0000-0000-0000-00000000000a', 'gmail', 'm1', 'email', 'Lease renewal', 'rent is due', 'h1');
insert into public.chunks (user_id, item_id, ord, text)
  select user_id, id, 0, body from public.items where external_id = 'm1';
insert into public.usage_events (user_id, kind, cost_micros) values ('00000000-0000-0000-0000-00000000000a', 'llm', 10);
commit;

begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000b', true);
insert into public.items (user_id, source, external_id, kind, title, body, content_hash)
  values ('00000000-0000-0000-0000-00000000000b', 'gmail', 'm2', 'email', 'Secret', 'b only', 'h2');
-- The service scoped to B sees nothing of A's, even with no WHERE clause.
do $$ begin
  assert (select count(*) from public.items) = 1, 'service as B sees only B';
  assert (select count(*) from public.chunks) = 0, 'service as B sees no A chunks';
end $$;
commit;

-- The service cannot write rows for another user.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  begin
    insert into public.items (user_id, source, external_id, kind, body, content_hash)
      values ('00000000-0000-0000-0000-00000000000a', 'gmail', 'x', 'email', '', 'h');
    raise exception 'cross-user insert was allowed';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- The service with no user set sees nothing.
begin;
set local role nugget_service;
do $$ begin
  assert (select count(*) from public.items) = 0, 'unscoped service sees nothing';
end $$;
rollback;

-- A user (PostgREST role) reads own rows only and cannot write.
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  assert (select count(*) from public.items) = 1, 'A sees one item';
  assert (select title from public.items) = 'Lease renewal', 'A sees own item';
  assert (select count(*) from public.usage_events) = 1, 'A sees own usage';
  begin
    insert into public.items (user_id, source, external_id, kind, body, content_hash)
      values ('00000000-0000-0000-0000-00000000000a', 'gmail', 'y', 'email', '', 'h');
    raise exception 'user insert was allowed';
  exception when insufficient_privilege then null;
  end;
  delete from public.items where external_id = 'm2';
end $$;
rollback;
do $$ begin
  assert (select count(*) from public.items where external_id = 'm2') = 1, 'A cannot delete B rows';
end $$;

-- Anonymous callers see nothing.
begin;
set local role anon;
do $$ begin
  begin
    perform count(*) from public.items;
    raise exception 'anon could read items';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- Usage ledger is append-only for the service.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  begin
    delete from public.usage_events;
    raise exception 'service could delete usage';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- Cross-user lookups only through the definer functions.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.connections (user_id, toolkit, composio_account_id)
  values ('00000000-0000-0000-0000-00000000000a', 'gmail', 'ca_1');
commit;
begin;
set local role nugget_service;
do $$ begin
  assert (select user_id from nugget_private.connection_owner('ca_1')) = '00000000-0000-0000-0000-00000000000a', 'owner lookup';
  assert (select count(*) from nugget_private.due_connections(10)) = 0, 'no subscription, no sync';
end $$;
rollback;

-- Billing: the service writes A's subscription; due sync needs it; B cannot see it.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.billing_customers (user_id, stripe_customer_id) values ('00000000-0000-0000-0000-00000000000a', 'cus_A');
insert into public.subscriptions (id, user_id, plan, interval, status, stripe_price_id)
  values ('sub_A', '00000000-0000-0000-0000-00000000000a', 'heed', 'month', 'trialing', 'price_1');
insert into public.devices (user_id, device_id) values ('00000000-0000-0000-0000-00000000000a', 'device-0001');
commit;
begin;
set local role nugget_service;
do $$ begin
  assert (select count(*) from nugget_private.due_connections(10)) = 1, 'subscribed connection is due';
  assert nugget_private.customer_owner('cus_A') = '00000000-0000-0000-0000-00000000000a', 'customer owner';
  assert not nugget_private.stripe_event_seen('evt_1'), 'event unseen';
  perform nugget_private.mark_stripe_event('evt_1', 'checkout.session.completed');
  perform nugget_private.mark_stripe_event('evt_1', 'checkout.session.completed');
  assert nugget_private.stripe_event_seen('evt_1'), 'event recorded once';
end $$;
commit;
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  assert (select count(*) from public.subscriptions) = 0, 'B sees no subscriptions of A';
  assert (select count(*) from public.billing_customers) = 0, 'B sees no customers of A';
  begin
    perform count(*) from nugget_private.stripe_events;
    raise exception 'authenticated could read stripe_events';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  assert (select status from public.subscriptions) = 'trialing', 'A sees own subscription';
  begin
    update public.subscriptions set status = 'active';
    raise exception 'user could change own subscription';
  exception when insufficient_privilege then null;
  end;
  delete from public.devices;
  assert (select count(*) from public.devices) = 0, 'A can forget own devices';
end $$;
rollback;
begin;
set local role authenticated;
do $$ begin
  begin
    perform nugget_private.connection_owner('ca_1');
    raise exception 'authenticated could call connection_owner';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- Deleting the auth user cascades everywhere.
delete from auth.users where id = '00000000-0000-0000-0000-00000000000a';
do $$ begin
  assert (select count(*) from public.items where user_id = '00000000-0000-0000-0000-00000000000a') = 0, 'items cascade';
  assert (select count(*) from public.chunks) = 0, 'chunks cascade';
  assert (select count(*) from public.usage_events) = 0, 'usage cascade';
  assert (select count(*) from public.connections) = 0, 'connections cascade';
  assert (select count(*) from public.subscriptions) = 0, 'subscriptions cascade';
  assert (select count(*) from public.billing_customers) = 0, 'customers cascade';
end $$;
select 'rls.sql: all assertions passed' as result;
