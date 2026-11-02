-- Editing and snoozing proposed actions (CL4, for DS1's v4 approval UI).
--
-- * input: the user-facing input the action was planned from (what PATCH /v1/actions/{id}
--   merges an edit into). Null on actions proposed before this migration: those cannot be
--   edited, only approved, cancelled or proposed again.
-- * revision: 1 when proposed, +1 per edit. Every edit re-plans the call and changes `digest`.
-- * proposed_digest: the digest at proposal, so retrying the original proposal with its
--   idempotency key still finds the (since edited) action instead of a conflict.
-- * snoozed_until: hidden from lists until then; never later than expires_at (snoozing does
--   not extend an action's life, so an approval is never more than 24 hours from its preview).
--
-- No new tables: pending_actions keeps its RLS (users read their own rows; only the scoped
-- service writes) and its same-user foreign keys.

alter table public.pending_actions
  add column input jsonb,
  add column revision int not null default 1 check (revision >= 1),
  add column proposed_digest text check (proposed_digest ~ '^[0-9a-f]{64}$'),
  add column snoozed_until timestamptz,
  add constraint pending_actions_snooze_within_expiry
    check (snoozed_until is null or snoozed_until <= expires_at);

create index pending_actions_user_open on public.pending_actions (user_id, status, snoozed_until);
