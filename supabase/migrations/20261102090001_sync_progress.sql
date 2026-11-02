-- Background sync progress: a per-connection run lease (one sync at a time, whoever starts
-- it), whether the first backfill has finished, and how many items it has stored.
alter table public.connections
  add column syncing_until timestamptz,
  add column backfill_done boolean not null default false,
  add column items_synced integer not null default 0 check (items_synced >= 0);
