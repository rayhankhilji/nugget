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

-- Actions and their audit chain (20261102120001_apps_actions.sql).
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.pending_actions (id, user_id, kind, toolkit, tool, arguments, summary, digest, risk, decision, idempotency_key)
  values ('00000000-0000-0000-0000-0000000000a1', '00000000-0000-0000-0000-00000000000a', 'send_mail', 'gmail',
          'GMAIL_SEND_EMAIL', '{}', 'Send an email', repeat('a', 64), 'send', 'confirm', 'k1');
insert into public.action_audit (user_id, action_id, seq, event, prev_hash, hash)
  values ('00000000-0000-0000-0000-00000000000a', '00000000-0000-0000-0000-0000000000a1', 1, 'proposed', repeat('0', 64), repeat('1', 64));
update public.pending_actions set status = 'cancelled' where id = '00000000-0000-0000-0000-0000000000a1';
commit;

begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  assert (select count(*) from public.pending_actions) = 0, 'service as B sees no A actions';
  assert (select count(*) from public.action_audit) = 0, 'service as B sees no A audit';
  update public.pending_actions set status = 'pending';
  assert not found, 'service as B cannot touch A actions';
  begin
    insert into public.pending_actions (user_id, kind, toolkit, tool, arguments, summary, digest, risk, decision, idempotency_key)
      values ('00000000-0000-0000-0000-00000000000a', 'send_mail', 'gmail', 'T', '{}', 's', repeat('b', 64), 'send', 'confirm', 'k2');
    raise exception 'cross-user action insert was allowed';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- The audit chain is append-only for the service, and actions are never deleted by it.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  begin
    update public.action_audit set event = 'forged';
    raise exception 'service could rewrite the audit';
  exception when insufficient_privilege then null;
  end;
  begin
    delete from public.action_audit;
    raise exception 'service could delete the audit';
  exception when insufficient_privilege then null;
  end;
  begin
    delete from public.pending_actions;
    raise exception 'service could delete actions';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

-- A user reads their own actions and audit, and can neither write nor delete them.
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  assert (select count(*) from public.pending_actions) = 1, 'A sees own action';
  assert (select count(*) from public.action_audit) = 1, 'A sees own audit';
  begin
    update public.pending_actions set status = 'pending';
    raise exception 'user could approve directly';
  exception when insufficient_privilege then null;
  end;
  begin
    delete from public.action_audit;
    raise exception 'user could delete the audit';
  exception when insufficient_privilege then null;
  end;
  begin
    insert into public.pending_actions (user_id, kind, toolkit, tool, arguments, summary, digest, risk, decision, idempotency_key)
      values ('00000000-0000-0000-0000-00000000000a', 'send_mail', 'gmail', 'T', '{}', 's', repeat('c', 64), 'send', 'confirm', 'k3');
    raise exception 'user could propose directly';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  assert (select count(*) from public.pending_actions) = 0, 'B sees no A actions';
  assert (select count(*) from public.action_audit) = 0, 'B sees no A audit';
end $$;
rollback;

-- Same-user foreign keys (20261102120002): an action cannot point at another user's
-- connection, and an audit entry cannot point at another user's action.
insert into public.connections (id, user_id, toolkit, composio_account_id)
  values ('00000000-0000-0000-0000-0000000000c2', '00000000-0000-0000-0000-00000000000b', 'gmail', 'ca_b_fk');
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  begin
    insert into public.pending_actions (user_id, connection_id, kind, toolkit, tool, arguments, summary, digest, risk, decision, idempotency_key)
      values ('00000000-0000-0000-0000-00000000000a', '00000000-0000-0000-0000-0000000000c2', 'send_mail', 'gmail',
              'GMAIL_SEND_EMAIL', '{}', 's', repeat('d', 64), 'send', 'confirm', 'fk1');
    raise exception 'action on another user''s connection was allowed';
  exception when foreign_key_violation then null;
  end;
end $$;
rollback;
-- Even a superuser write (a service bug with RLS off) cannot cross users.
do $$ begin
  begin
    insert into public.action_audit (user_id, action_id, seq, event, prev_hash, hash)
      values ('00000000-0000-0000-0000-00000000000b', '00000000-0000-0000-0000-0000000000a1', 1, 'forged', repeat('0', 64), repeat('2', 64));
    raise exception 'audit entry for another user''s action was allowed';
  exception when foreign_key_violation then null;
  end;
end $$;
-- Disconnecting clears only connection_id.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000b', true);
insert into public.pending_actions (id, user_id, connection_id, kind, toolkit, tool, arguments, summary, digest, risk, decision, idempotency_key)
  values ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-00000000000b', '00000000-0000-0000-0000-0000000000c2',
          'send_mail', 'gmail', 'GMAIL_SEND_EMAIL', '{}', 's', repeat('e', 64), 'send', 'confirm', 'fk2');
commit;
delete from public.connections where id = '00000000-0000-0000-0000-0000000000c2';
do $$ begin
  assert (select connection_id is null and user_id = '00000000-0000-0000-0000-00000000000b'
          from public.pending_actions where id = '00000000-0000-0000-0000-0000000000b1'),
         'disconnect clears the action''s connection only';
end $$;

-- Slack and Teams messages are items too.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.items (user_id, source, external_id, kind, body, content_hash)
  values ('00000000-0000-0000-0000-00000000000a', 'slack', 'C1:1.0', 'message', 'hi', 'h');
rollback;

-- M2/M3 tables: the same isolation.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000a', true);
insert into public.proactive_state (user_id, state) values ('00000000-0000-0000-0000-00000000000a', '{"cards": {}}');
insert into public.intel_runs (user_id) values ('00000000-0000-0000-0000-00000000000a');
insert into public.llm_cache (user_id, key, response)
  values ('00000000-0000-0000-0000-00000000000a', repeat('a', 64), '{}');
insert into public.briefs (user_id, day, timezone, data)
  values ('00000000-0000-0000-0000-00000000000a', '2026-11-02', 'Europe/London', '{}');
insert into public.cards (user_id, kind, title, status, card_key)
  values ('00000000-0000-0000-0000-00000000000a', 'renewal', 'Renews', 'snoozed', 'k1');
update public.profiles set timezone = 'UTC' where user_id = '00000000-0000-0000-0000-00000000000a';
commit;
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  assert (select count(*) from public.proactive_state) = 0, 'service as B sees no A proactive state';
  assert (select count(*) from public.intel_runs) = 0, 'service as B sees no A runs';
  assert (select count(*) from public.llm_cache) = 0, 'service as B sees no A cache';
  assert (select count(*) from public.briefs) = 0, 'service as B sees no A briefs';
  assert (select count(*) from public.cards) = 0, 'service as B sees no A cards';
  begin
    insert into public.briefs (user_id, day, timezone, data)
      values ('00000000-0000-0000-0000-00000000000a', '2026-11-03', 'UTC', '{}');
    raise exception 'cross-user brief insert was allowed';
  exception when insufficient_privilege then null;
  end;
  begin
    update public.proactive_state set state = '{}';
    if found then raise exception 'cross-user update touched A'; end if;
  end;
end $$;
commit;
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000b', true);
do $$ begin
  assert (select count(*) from public.briefs) = 0, 'B cannot read A briefs';
  assert (select count(*) from public.llm_cache) = 0, 'B cannot read A cache';
  begin
    insert into public.llm_cache (user_id, key, response)
      values ('00000000-0000-0000-0000-00000000000b', repeat('b', 64), 'x');
    raise exception 'user insert into llm_cache was allowed';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;
begin;
set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-00000000000a', true);
do $$ begin
  assert (select count(*) from public.briefs) = 1, 'A reads own brief';
  assert (select status from public.cards where card_key = 'k1') = 'snoozed', 'A reads own card';
end $$;
rollback;
-- The schedulers' definers: A has items and no run or brief yet; only the service may call them.
begin;
set local role nugget_service;
do $$ begin
  -- A (subscribed) has a run that covers no item yet: due. B has items but no subscription: never.
  assert (select count(*) from nugget_private.intel_due(10, '6 hours')) = 1, 'intel_due lists subscribed A only';
  assert (select user_id from nugget_private.intel_due(10, '6 hours')) = '00000000-0000-0000-0000-00000000000a', 'it is A';
  assert not exists (select 1 from nugget_private.brief_due(10) where user_id = '00000000-0000-0000-0000-00000000000b'), 'no brief for unsubscribed B';
  perform * from nugget_private.brief_due(10);
end $$;
rollback;
update public.intel_runs set items_through = now() + interval '1 minute';
begin;
set local role nugget_service;
do $$ begin
  assert (select count(*) from nugget_private.intel_due(10, '6 hours')) = 0, 'a covered, fresh run is not due';
end $$;
rollback;
begin;
set local role authenticated;
do $$ begin
  begin
    perform nugget_private.intel_due(10, '6 hours');
    raise exception 'authenticated could call intel_due';
  exception when insufficient_privilege then null;
  end;
  begin
    perform nugget_private.brief_due(10);
    raise exception 'authenticated could call brief_due';
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
  assert (select count(*) from public.briefs where user_id = '00000000-0000-0000-0000-00000000000a') = 0, 'briefs cascade';
  assert (select count(*) from public.proactive_state) = 0, 'proactive state cascades';
  assert (select count(*) from public.llm_cache) = 0, 'llm cache cascades';
  assert (select count(*) from public.pending_actions where user_id = '00000000-0000-0000-0000-00000000000a') = 0, 'actions cascade';
  assert (select count(*) from public.action_audit where user_id = '00000000-0000-0000-0000-00000000000a') = 0, 'audit cascades with the user';
  assert (select count(*) from public.subscriptions) = 0, 'subscriptions cascade';
  assert (select count(*) from public.billing_customers) = 0, 'customers cascade';
end $$;
select 'rls.sql: all assertions passed' as result;

-- Nothing in nugget_private is executable by the API roles.
do $$ begin
  assert not exists (
    select 1 from pg_proc p join pg_namespace n on n.oid = p.pronamespace,
                  unnest(array['anon', 'authenticated']) r
    where n.nspname = 'nugget_private' and has_function_privilege(r, p.oid, 'execute')
  ), 'nugget_private functions are private';
  assert not exists (
    select 1 from pg_proc p join pg_namespace n on n.oid = p.pronamespace
    where p.prosecdef and n.nspname in ('public', 'nugget_private')
      and not exists (select 1 from unnest(p.proconfig) c where c like 'search_path=%')
  ), 'definer functions pin search_path';
end $$;
-- A new auth user still gets a profile after the revoke.
insert into auth.users (id, email) values ('00000000-0000-0000-0000-00000000000c', 'c@example.com');
do $$ begin
  assert exists (select 1 from public.profiles where user_id = '00000000-0000-0000-0000-00000000000c'), 'trigger still works';
end $$;
select 'rls.sql: hardening assertions passed' as result;
