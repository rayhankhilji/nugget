-- Same-user foreign keys for the CL4 tables (SEC1's rule from 20261102130000: a foreign key
-- between per-user tables also matches user_id, because a plain FK check bypasses RLS).
--
-- * pending_actions.connection_id must be the same user's connection. On disconnect only
--   connection_id is cleared (the action stays, and can no longer run).
-- * action_audit.action_id must be the same user's action.
--
-- This runs before 20261102130000, so it adds its own (id, user_id) key on connections rather
-- than reuse connections_id_user from that migration.

alter table public.connections
  add constraint connections_id_user_for_actions unique (id, user_id);
alter table public.pending_actions
  add constraint pending_actions_id_user unique (id, user_id);

alter table public.pending_actions drop constraint pending_actions_connection_id_fkey;
alter table public.pending_actions add constraint pending_actions_connection_same_user
  foreign key (connection_id, user_id) references public.connections (id, user_id)
  on delete set null (connection_id);

alter table public.action_audit add constraint action_audit_action_same_user
  foreign key (action_id, user_id) references public.pending_actions (id, user_id);
