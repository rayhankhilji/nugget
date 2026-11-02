-- Security hardening (SEC1, docs/security/cloud-threat-model.md).
--
-- 1. Cross-tenant references are impossible: every foreign key between per-user tables also
--    matches user_id. RLS checks the row being written, but a plain FK check bypasses RLS, so
--    without this a service bug could attach a chunk, claim or message to another user's item,
--    entity or conversation (and A's export would then pull B's text through a join).
-- 2. The sync scheduler claims due connections with a lease (FOR UPDATE SKIP LOCKED), so two
--    machines, or a tick that overlaps a slow pass, never sync the same connection twice.
-- 3. search_path is pinned on every nugget_private function, and new tables and functions in
--    public no longer get Supabase's default grants to anon/authenticated: each migration must
--    grant explicitly (supabase/tests/hardening.sql checks the result).

-- ---------------------------------------------------------------------------------------------
-- 1. Same-user foreign keys
-- ---------------------------------------------------------------------------------------------

alter table public.connections   add constraint connections_id_user   unique (id, user_id);
alter table public.items         add constraint items_id_user         unique (id, user_id);
alter table public.entities      add constraint entities_id_user      unique (id, user_id);
alter table public.conversations add constraint conversations_id_user unique (id, user_id);

alter table public.items add constraint items_connection_same_user
  foreign key (connection_id, user_id) references public.connections (id, user_id) on delete cascade;
alter table public.chunks add constraint chunks_item_same_user
  foreign key (item_id, user_id) references public.items (id, user_id) on delete cascade;
alter table public.claims add constraint claims_item_same_user
  foreign key (item_id, user_id) references public.items (id, user_id) on delete cascade;
alter table public.claims add constraint claims_subject_same_user
  foreign key (subject_id, user_id) references public.entities (id, user_id) on delete cascade;
alter table public.claims add constraint claims_object_same_user
  foreign key (object_id, user_id) references public.entities (id, user_id) on delete cascade;
alter table public.messages add constraint messages_conversation_same_user
  foreign key (conversation_id, user_id) references public.conversations (id, user_id) on delete cascade;

-- ---------------------------------------------------------------------------------------------
-- 2. Leased claim of due connections
-- ---------------------------------------------------------------------------------------------

-- Returns up to max_rows (capped at 100) active connections that are due, oldest first, and
-- pushes their next_sync_at lease_secs (at least 60) into the future so no other caller gets
-- them. The sync itself sets the real next_sync_at when it finishes.
create or replace function nugget_private.claim_due_connections(max_rows int, lease_secs int)
returns table (user_id uuid, connection_id uuid, toolkit text)
language sql volatile security definer set search_path = '' as $$
  with due as (
    select c.id from public.connections c
    where c.status = 'active' and c.next_sync_at <= now()
    order by c.next_sync_at
    limit greatest(least(max_rows, 100), 0)
    for update skip locked
  )
  update public.connections c
     set next_sync_at = now() + make_interval(secs => greatest(lease_secs, 60))
    from due
   where c.id = due.id
  returning c.user_id, c.id, c.toolkit
$$;
revoke all on function nugget_private.claim_due_connections(int, int) from public;
grant execute on function nugget_private.claim_due_connections(int, int) to nugget_service;

-- A user deleting a connection row directly through the Data API would drop its items but
-- leave the Google grant alive at Composio (and the next listing re-adds it). Disconnecting goes
-- through DELETE /v1/connections/{id}, which revokes at Composio first.
revoke delete on public.connections from authenticated;

-- ---------------------------------------------------------------------------------------------
-- 3. search_path and default privileges
-- ---------------------------------------------------------------------------------------------

-- Used inside every service policy; pin it like the definer functions.
alter function nugget_private.current_user_id() set search_path = '';

-- Nothing in nugget_private is for the API roles (schema usage is already revoked).
revoke execute on all functions in schema nugget_private from public;
grant execute on function nugget_private.current_user_id() to nugget_service;
grant execute on function nugget_private.connection_owner(text) to nugget_service;
grant execute on function nugget_private.due_connections(int) to nugget_service;
alter default privileges in schema nugget_private revoke execute on functions from public;

-- Supabase grants anon/authenticated everything on new objects in public by default. Make
-- every grant explicit instead, so a forgotten REVOKE cannot expose a new table through the
-- Data API. (Applies to objects created by the role running migrations.)
alter default privileges in schema public revoke all on tables from anon, authenticated;
alter default privileges in schema public revoke all on sequences from anon, authenticated;
alter default privileges in schema public revoke execute on functions from public, anon, authenticated;
