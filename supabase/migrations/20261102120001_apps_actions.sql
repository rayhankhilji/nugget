-- More apps (Slack, Notion, Drive, Outlook, Teams) and approved actions (CL4).
--
-- * items.kind gains 'message' (Slack and Teams).
-- * connections gain the Composio trigger instance that nudges them (trigger-mode apps).
-- * pending_actions: every outbound action (send mail, reply, create/update event, post a
--   message, create a page) is proposed here, shown to the user as a preview, and executed only
--   after an explicit approval that echoes the preview's digest.
-- * action_audit: an append-only, per-user hash chain of every step of every action.
--
-- Same RLS shape as the M1 tables: the user's JWT may read its own rows; only the service
-- (scoped by nugget.user_id) writes. Neither may rewrite the audit chain.

alter table public.items drop constraint if exists items_kind_check;
alter table public.items add constraint items_kind_check
  check (kind in ('email', 'event', 'doc', 'message'));
-- Deletes and edits reconcile by source and time window.
create index if not exists items_user_source_time on public.items (user_id, source, occurred_at);

alter table public.connections add column if not exists trigger_id text;

create table public.pending_actions (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null references auth.users (id) on delete cascade,
  connection_id uuid references public.connections (id) on delete set null,
  kind text not null check (kind in ('send_mail', 'reply_mail', 'create_event', 'update_event',
                                     'post_message', 'create_page')),
  toolkit text not null check (toolkit ~ '^[a-z0-9_]{1,64}$'),
  -- The exact Composio call that approval releases.
  tool text not null,
  arguments jsonb not null,
  -- What the user is shown: fields and a before/after diff.
  summary text not null,
  preview jsonb not null default '{}'::jsonb,
  -- sha256 over the tool and arguments; an approval must echo it.
  digest text not null check (digest ~ '^[0-9a-f]{64}$'),
  risk text not null,
  decision text not null check (decision in ('confirm', 'deny')),
  reason text,
  idempotency_key text not null check (length(idempotency_key) between 1 and 128),
  status text not null default 'pending'
    check (status in ('pending', 'executing', 'succeeded', 'failed', 'cancelled', 'expired', 'denied')),
  result jsonb,
  error text,
  created_at timestamptz not null default now(),
  expires_at timestamptz not null default now() + interval '24 hours',
  decided_at timestamptz,
  executed_at timestamptz,
  unique (user_id, idempotency_key)
);
create index pending_actions_user on public.pending_actions (user_id, created_at desc);

create table public.action_audit (
  id bigint generated always as identity primary key,
  user_id uuid not null references auth.users (id) on delete cascade,
  -- Kept when an action row goes away with its user only (cascade); never rewritten.
  action_id uuid,
  seq bigint not null,
  event text not null,
  detail jsonb not null default '{}'::jsonb,
  prev_hash text not null check (prev_hash ~ '^[0-9a-f]{64}$'),
  hash text not null check (hash ~ '^[0-9a-f]{64}$'),
  created_at timestamptz not null default now(),
  unique (user_id, seq)
);
create index action_audit_action on public.action_audit (user_id, action_id);

do $$
declare
  t text;
begin
  foreach t in array array['pending_actions', 'action_audit']
  loop
    execute format('alter table public.%I enable row level security', t);
    execute format('revoke all on public.%I from anon, authenticated', t);
    execute format('grant select on public.%I to authenticated', t);
    execute format('grant select, insert, update on public.%I to nugget_service', t);
    execute format($p$create policy own_rows_read on public.%I for select to authenticated
                      using (user_id = (select auth.uid()))$p$, t);
    execute format($p$create policy service_scoped on public.%I for all to nugget_service
                      using (user_id = (select nugget_private.current_user_id()))
                      with check (user_id = (select nugget_private.current_user_id()))$p$, t);
  end loop;
end $$;

-- The audit chain is append-only for everyone but the cascade from auth.users.
revoke update, delete on public.action_audit from nugget_service, authenticated;
-- Actions change state only through the service; nobody deletes them (the audit points at them).
revoke delete on public.pending_actions from nugget_service, authenticated;
