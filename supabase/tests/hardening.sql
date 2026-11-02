-- Schema-wide hardening checks (docs/security/cloud-threat-model.md). Run by run.sh after
-- rls.sql. These cover every table and function, including ones added by later migrations.
\set ON_ERROR_STOP on

-- Every table in public has RLS on and at least one policy; anon has no privileges at all;
-- authenticated can never write (all writes go through the service, so quotas hold).
do $$
declare
  r record;
begin
  for r in select c.oid, c.relname, c.relrowsecurity from pg_class c
           join pg_namespace n on n.oid = c.relnamespace
           where n.nspname = 'public' and c.relkind in ('r', 'p')
  loop
    assert r.relrowsecurity, format('RLS is off on public.%I', r.relname);
    assert exists (select 1 from pg_policy p where p.polrelid = r.oid),
      format('public.%I has no policies', r.relname);
    assert not has_table_privilege('anon', r.oid, 'select,insert,update,delete,truncate,references,trigger'),
      format('anon has privileges on public.%I', r.relname);
    assert not has_table_privilege('authenticated', r.oid, 'insert,update,truncate,references,trigger'),
      format('authenticated can write to public.%I', r.relname);
  end loop;
end $$;

do $$ begin
  assert not has_table_privilege('authenticated', 'public.connections', 'delete'),
    'connections must be disconnected through the service (Composio revocation first)';
end $$;

-- No views in public that would bypass RLS (views run as their owner unless security_invoker).
do $$
declare
  r record;
begin
  for r in select c.relname, c.reloptions from pg_class c join pg_namespace n on n.oid = c.relnamespace
           where n.nspname = 'public' and c.relkind in ('v', 'm')
  loop
    assert coalesce('security_invoker=true' = any (r.reloptions), false)
           or not has_table_privilege('authenticated', format('public.%I', r.relname), 'select'),
      format('view public.%I is readable by authenticated without security_invoker', r.relname);
  end loop;
end $$;

-- Every SECURITY DEFINER function in our schemas pins search_path, and none is callable by
-- the API roles unless it lives in public on purpose (none do today).
do $$
declare
  r record;
begin
  for r in select p.oid, n.nspname, p.proname, p.prosecdef, p.proconfig from pg_proc p
           join pg_namespace n on n.oid = p.pronamespace
           where n.nspname in ('public', 'nugget_private')
  loop
    if r.prosecdef then
      assert exists (select 1 from unnest(r.proconfig) s where s like 'search_path=%'),
        format('security definer %I.%I does not pin search_path', r.nspname, r.proname);
    end if;
    if r.nspname = 'nugget_private' then
      assert not has_function_privilege('anon', r.oid, 'execute')
         and not has_function_privilege('authenticated', r.oid, 'execute'),
        format('%I.%I is executable by an API role', r.nspname, r.proname);
    elsif r.prosecdef then
      assert not has_function_privilege('anon', r.oid, 'execute'),
        format('security definer public.%I is executable by anon', r.proname);
    end if;
  end loop;
end $$;

insert into auth.users (id, email) values
  ('00000000-0000-0000-0000-0000000000c1', 'c1@example.com'),
  ('00000000-0000-0000-0000-0000000000c2', 'c2@example.com');

-- Cross-tenant references are refused even for the service scoped to the writing user:
-- C2 cannot hang a chunk, claim or message off C1's item, entity or conversation.
begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-0000000000c1', true);
insert into public.items (id, user_id, source, external_id, kind, body, content_hash)
  values ('00000000-0000-0000-0000-00000000c1a1', '00000000-0000-0000-0000-0000000000c1', 'gmail', 'c1m', 'email', 'c1 secret', 'h');
insert into public.entities (id, user_id, kind, name, canonical)
  values ('00000000-0000-0000-0000-00000000c1e1', '00000000-0000-0000-0000-0000000000c1', 'person', 'X', 'x');
insert into public.conversations (id, user_id)
  values ('00000000-0000-0000-0000-00000000c1c1', '00000000-0000-0000-0000-0000000000c1');
commit;

begin;
set local role nugget_service;
select set_config('nugget.user_id', '00000000-0000-0000-0000-0000000000c2', true);
insert into public.entities (id, user_id, kind, name, canonical)
  values ('00000000-0000-0000-0000-00000000c2e1', '00000000-0000-0000-0000-0000000000c2', 'person', 'Y', 'y');
do $$ begin
  begin
    insert into public.chunks (user_id, item_id, ord, text)
      values ('00000000-0000-0000-0000-0000000000c2', '00000000-0000-0000-0000-00000000c1a1', 0, 'x');
    raise exception 'chunk on another user''s item was allowed';
  exception when foreign_key_violation then null;
  end;
  begin
    insert into public.claims (user_id, subject_id, predicate, item_id)
      values ('00000000-0000-0000-0000-0000000000c2', '00000000-0000-0000-0000-00000000c2e1', 'p',
              '00000000-0000-0000-0000-00000000c1a1');
    raise exception 'claim on another user''s item was allowed';
  exception when foreign_key_violation then null;
  end;
  begin
    insert into public.claims (user_id, subject_id, predicate, item_id, object_id)
      values ('00000000-0000-0000-0000-0000000000c2', '00000000-0000-0000-0000-00000000c1e1', 'p',
              '00000000-0000-0000-0000-00000000c1a1', null);
    raise exception 'claim about another user''s entity was allowed';
  exception when foreign_key_violation then null;
  end;
  begin
    insert into public.messages (user_id, conversation_id, role, content)
      values ('00000000-0000-0000-0000-0000000000c2', '00000000-0000-0000-0000-00000000c1c1', 'user', 'x');
    raise exception 'message in another user''s conversation was allowed';
  exception when foreign_key_violation then null;
  end;
end $$;
rollback;

-- The leased claim hands each due connection to exactly one caller and pushes it out.
insert into public.connections (user_id, toolkit, composio_account_id)
  values ('00000000-0000-0000-0000-0000000000c1', 'gmail', 'ca_c1'),
         ('00000000-0000-0000-0000-0000000000c2', 'gmail', 'ca_c2');
begin;
set local role nugget_service;
create temp table first_claim as select * from nugget_private.claim_due_connections(10, 600);
do $$ begin
  assert (select count(*) from first_claim) = 2, 'both due connections claimed';
  assert (select count(*) from nugget_private.claim_due_connections(10, 600)) = 0, 'a second claim gets nothing';
  assert (select count(*) from nugget_private.due_connections(10)) = 0, 'claimed rows are no longer due';
end $$;
rollback;
begin;
set local role authenticated;
do $$ begin
  begin
    perform nugget_private.claim_due_connections(10, 600);
    raise exception 'authenticated could call claim_due_connections';
  exception when insufficient_privilege then null;
  end;
end $$;
rollback;

delete from auth.users where id in ('00000000-0000-0000-0000-0000000000c1', '00000000-0000-0000-0000-0000000000c2');
do $$ begin
  assert (select count(*) from public.items where user_id = '00000000-0000-0000-0000-0000000000c1') = 0, 'cascade still works';
end $$;
select 'hardening.sql: all assertions passed' as result;

-- The waitlist is the service's alone: no API role reads or deletes it; the service may only
-- insert (reads and deletions go through nugget_private.waitlist_forget).
do $$ begin
  assert not has_table_privilege('authenticated', 'public.waitlist', 'select,delete'),
    'authenticated can read or delete the waitlist';
  assert not has_table_privilege('anon', 'public.waitlist', 'select,insert'),
    'anon can touch the waitlist';
  assert not has_table_privilege('nugget_service', 'public.waitlist', 'select,update,delete'),
    'nugget_service can do more than insert into the waitlist';
  assert has_table_privilege('nugget_service', 'public.waitlist', 'insert'), 'service cannot insert';
end $$;
select 'hardening.sql: waitlist assertions passed' as result;
