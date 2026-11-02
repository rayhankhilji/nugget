-- Heed cloud model endpoint (`/llm/v1/messages`, crates/nugget-cloud/src/llm): each metered call
-- records which tier and provider served it (`route`, e.g. `heed-fast:anthropic`) and its
-- prompt-cache tokens, which are billed differently from plain input (reads at 0.1x, writes at
-- 1.25x). `cost_micros` stays the one number quotas are checked against.
alter table public.usage_events
  add column if not exists route text not null default '',
  add column if not exists cache_read_tokens int not null default 0,
  add column if not exists cache_write_tokens int not null default 0;
