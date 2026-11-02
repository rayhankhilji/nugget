-- Waitlist for the website while Heed Cloud is not open. Addresses only (no account), so the
-- table is the service's alone: no API role can read it, and nugget_service can only insert.
-- Deletion on request goes through nugget_private.waitlist_forget (support, or account
-- deletion when the same address later signs up and deletes its account).

create extension if not exists citext with schema extensions;

create table public.waitlist (
  email extensions.citext primary key check (length(email) between 3 and 254),
  created_at timestamptz not null default now(),
  source text not null default 'site' check (source ~ '^[a-z0-9_-]{1,32}$'),
  -- Salted SHA-256 of the sign-up IP (per-deploy secret); null when no salt is configured.
  ip_hash text check (ip_hash is null or ip_hash ~ '^[0-9a-f]{64}$')
);

alter table public.waitlist enable row level security;
revoke all on public.waitlist from public, anon, authenticated;
grant insert on public.waitlist to nugget_service;
create policy service_insert on public.waitlist for insert to nugget_service with check (true);

-- Removes an address (support requests; account deletion). Returns whether it was there.
create or replace function nugget_private.waitlist_forget(addr text) returns boolean
language plpgsql security definer set search_path = '' as $$
declare
  n int;
begin
  delete from public.waitlist where email = lower(trim(addr))::extensions.citext;
  get diagnostics n = row_count;
  return n > 0;
end $$;
revoke all on function nugget_private.waitlist_forget(text) from public;
grant execute on function nugget_private.waitlist_forget(text) to nugget_service;
