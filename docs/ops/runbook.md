# Heed cloud runbook

For whoever is on call (today: the founder). Commands assume the repository root as the working
directory and a logged-in `fly` and `supabase` CLI. Code identifiers still say `nugget`: the Fly app
is `nugget-cloud`, the crate is `crates/nugget-cloud`, the database role is `nugget_service`.

Background: [docs/architecture-cloud.md](../architecture-cloud.md). Pre-launch list:
[prelaunch-checklist.md](prelaunch-checklist.md).

## 1. Overview

| Component | Where | Holds |
|---|---|---|
| `nugget-cloud` (Rust, axum) | Fly.io app `nugget-cloud`, region `lhr`, `shared-cpu-1x`, always on | API, AI gateway, in-process sync scheduler. Stateless. |
| Supabase (Pro) | Project in `eu-west-2` (London) | Auth users, Postgres + pgvector (all user data), Storage bucket `exports` |
| Composio (Hobby, then Pro) | Composio cloud | Google OAuth tokens and connected accounts, keyed by our user id |
| Anthropic, OpenAI | Provider APIs | Nothing persistent from us beyond their API log retention (up to 30 days) |
| Stripe | Being added | Customers and subscriptions, once billing lands |
| Sentry (optional) | Any Sentry-compatible host | 5xx and panic reports: route, status, request id only |

Secrets live only in Fly secrets and the providers' dashboards. `fly secrets list -a nugget-cloud`
shows names and digests, never values. The desktop app ships only `SUPABASE_URL`, the anon (or
publishable) key and the API base URL.

Endpoints you will use: `GET /healthz` (process up), `GET /readyz` (database reachable within 2 s,
else 503; Fly's health check uses this), `GET /metrics` on the internal listener only.

## 2. Deploy

1. **Migrations first.** Any migration the new code needs goes in before the code. Migrations must
   be additive (new tables, nullable columns, new functions) so the *running* code keeps working.
   ```sh
   supabase/tests/run.sh                      # local: RLS tests on a fresh database
   supabase link --project-ref <ref>          # once per machine
   supabase db push --dry-run                 # read the list of pending migrations
   supabase db push
   # or, without the CLI:
   psql "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -f supabase/migrations/<file>.sql
   ```
   A migration that recreates a role or grant may reset `nugget_service` rights; re-check with
   `\du nugget_service` and `\dp public.*` afterwards. `nugget_service` is created `NOLOGIN` by the
   migration; login and password are set by hand (section 4.3).
2. **Deploy the code** (from `fly.toml`'s header):
   ```sh
   fly deploy --config crates/nugget-cloud/fly.toml --dockerfile crates/nugget-cloud/Dockerfile
   ```
   Fly rolls machines one by one. Each old machine gets SIGTERM, stops accepting, and drains
   in-flight requests and the current sync pass for up to `NUGGET_SHUTDOWN_GRACE_SECS` (default 25)
   before `kill_timeout = 30` kills it.
3. **Verify.**
   ```sh
   fly status -a nugget-cloud                 # all machines started, checks passing
   curl -fsS https://nugget-cloud.fly.dev/healthz
   curl -fsS https://nugget-cloud.fly.dev/readyz
   fly logs -a nugget-cloud                   # watch for 5 minutes: 5xx, panics, sync errors
   ```
   Then sign in from a desktop build and run one Ask.
4. **Contract later.** Dropping a column or table the old code reads happens in a *later* release,
   after no running code uses it.

## 3. Rollback

```sh
fly releases -a nugget-cloud --image          # versions with their image refs
fly deploy --config crates/nugget-cloud/fly.toml \
  --image registry.fly.io/nugget-cloud:deployment-<previous>
```
- Rolling back the image does not touch secrets. If the bad release came with a secret change,
  set the old value back too (`fly secrets set ...`).
- **Migrations are forward-only.** Do not hand-edit or revert an applied migration file. Fix
  forward with a new, compensating migration (`supabase migration new fix_<thing>`).
- **Never roll the code back past a schema it cannot read.** Because deploys are expand-then-
  contract (section 2), the previous image should always work on the current schema. If it does
  not, fix forward instead of rolling back.

## 4. Rotate keys

General order for every secret: **create the new credential → `fly secrets set` (this restarts the
machines) → verify → revoke the old one.** Where a provider allows only one live credential, there
is a window of failures; do it in a quiet hour (UK early morning) and watch the logs.

```sh
fly secrets set -a nugget-cloud NAME=value    # restarts machines with the new value
fly secrets set -a nugget-cloud --stage NAME=value && fly deploy ...   # batch several changes
```
Never paste secrets into the shell history on a shared machine: use `NAME=- < file` or
`read -rs V; fly secrets set NAME="$V"`.

### 4.1 SUPABASE_SERVICE_ROLE_KEY
Used only for the admin user-deletion call (`DELETE /v1/me`).
- **Preferred: Supabase API keys (`sb_secret_…`).** Create a second secret key in Project
  Settings → API Keys, set it, verify, then delete the old one. No downtime.
  ```sh
  fly secrets set -a nugget-cloud SUPABASE_SERVICE_ROLE_KEY=sb_secret_...
  ```
  Verify by deleting a throwaway test account end to end (section 7) before revoking the old key.
- **Legacy JWT `service_role` key:** it can only change by rotating the project's JWT secret, which
  also changes the anon key and logs out every user (see 4.2). Migrate to the new keys instead.
- What breaks if it is wrong: only account deletion (503). Everything else keeps working.

### 4.2 SUPABASE_ANON_KEY and JWT signing keys
The anon key is **baked into the desktop build**. Changing it needs a desktop release, and old
builds stop being able to sign in once the old key is revoked.
- **Prefer publishable keys (`sb_publishable_…`) and asymmetric JWT signing keys.** Publishable keys
  can be added and revoked individually; ship the new one in a desktop release, wait for adoption,
  then revoke the old one. Asymmetric signing keys rotate with a standby key and no forced logout.
- The service validates user tokens by calling `GET /auth/v1/user`, so it works with either JWT
  scheme and needs no change when signing keys rotate. Update its `SUPABASE_ANON_KEY` secret when
  the key it uses is replaced.
- **Legacy JWT secret rotation** (only if the secret leaked): invalidates the anon and service role
  keys and every issued access token at once. Order: rotate in Supabase → set new
  `SUPABASE_ANON_KEY` and `SUPABASE_SERVICE_ROLE_KEY` on Fly → release a desktop build with the new
  anon key. Users of old builds cannot sign in until they update.

### 4.3 DATABASE_URL (`nugget_service` password)
```sql
-- as postgres in the SQL editor or psql; generate with: openssl rand -base64 32 | tr -d '/+='
alter role nugget_service with login password '<new>';
```
```sh
fly secrets set -a nugget-cloud \
  DATABASE_URL='postgres://nugget_service.<project-ref>:<new>@<pooler-host>:<port>/postgres'
# keep host, port and any query parameters exactly as in the current value; only the password changes
fly status -a nugget-cloud && curl -fsS https://nugget-cloud.fly.dev/readyz
```
- Postgres has one password per role, so from `alter role` until the restart completes, new
  connections fail and `/readyz` returns 503. Existing pooled connections keep working. Run the two
  steps back to back; expect under a minute of errors.
- Zero-downtime alternative: create `nugget_service_2` with `login password ...` and
  `in role nugget_service` (member-only, no other rights), switch `DATABASE_URL` to it, then
  `alter role nugget_service nologin` (or drop the old login role).

### 4.4 COMPOSIO_API_KEY
Create a new key in the Composio dashboard (same project), `fly secrets set COMPOSIO_API_KEY=...`,
check a sync (`fly logs`, `heed_sync_runs_total{result}`) and `GET /v1/connections`, then delete the
old key. Connected accounts belong to the project, not the key, so nothing needs reconnecting.

### 4.5 COMPOSIO_WEBHOOK_SECRET
Standard Webhooks signing. The service accepts several space-separated signatures in
`webhook-signature`, but holds only **one** secret. Between regenerating it in Composio and the Fly
restart, webhooks fail verification (401) and Composio retries. Impact is low: a webhook only marks a
connection due, and the scheduler polls anyway. Rotate in a quiet window:
regenerate in Composio → `fly secrets set COMPOSIO_WEBHOOK_SECRET=...` → watch for 401s on
`/v1/webhooks/composio` stopping.

### 4.6 ANTHROPIC_API_KEY, OPENAI_API_KEY
Both providers allow several live keys. Create a new key in the same project/workspace (with its
spend limit), set it, run one Ask (and check embeddings for OpenAI), then disable the old key.
While `ANTHROPIC_API_KEY` is unset or bad, Ask falls back to OpenAI only if `OPENAI_API_KEY` is set;
otherwise Asks fail. Without `OPENAI_API_KEY`, embeddings stop and retrieval is full-text only.

### 4.7 SENTRY_DSN
A DSN only allows sending events, but rotate it if it is abused (spam events). Create a new client
key in Sentry, `fly secrets set SENTRY_DSN=...`, revoke the old key. Nothing user-facing breaks.

### 4.8 Stripe keys (once billing lands; names may change)
- `STRIPE_SECRET_KEY`: prefer a restricted key. Roll it in the Dashboard (Stripe lets the old key
  live for a chosen overlap period), set the new one, verify, let the old one expire.
- `STRIPE_WEBHOOK_SECRET`: rolling the endpoint secret in Stripe also allows an overlap period, so
  set the new secret on Fly within that window.

## 5. Restore a backup

Supabase Pro keeps **daily backups for 7 days**. Point-in-time recovery is a paid add-on; without it,
the worst case is losing up to 24 hours. Backups cover the database (including `auth`), **not**
Storage objects.

1. **Contain first.** If data is being corrupted, stop writers: `fly scale count 0 -a nugget-cloud`.
2. **Restore into a NEW project** (Dashboard → Database → Backups → restore to a new project, or
   download the backup and load it with `psql` into a fresh project in `eu-west-2`).
3. **Verify** in the new project: row counts per table, the latest `items.created_at` and
   `usage_events.created_at`, `supabase/tests/rls.sql` policies present, and
   `nugget_service` exists. Set its login password (section 4.3), because passwords may not match.
4. **Re-apply deletions.** A backup resurrects every user deleted after the backup point. For each
   entry in the deletion log dated after the backup, delete the auth user again (section 7, step 4).
   This is why we keep a **deletion log of user ids and dates outside the database** (a private
   spreadsheet or a file in the ops vault, never in Supabase).
5. **Switch over.**
   - Database content only (Auth unaffected): point `DATABASE_URL` at the restored project and
     restart. Note that `SUPABASE_URL` and the keys still belong to the old project, so this is only
     right when the old project's Auth is intact and the user ids match.
   - Whole project: the new project has a new URL and new keys, which are baked into the desktop
     build. Prefer restoring **in place** once the restore has been rehearsed and verified in the
     new project; switching projects needs a desktop release.
6. `fly scale count 1 -a nugget-cloud` (or the previous count), verify `/readyz`, run an Ask.
7. Record what was lost (window between backup and incident) in the incident notes.

## 6. Incident response

| Sev | Meaning | Response |
|---|---|---|
| 1 | Personal data exposed or at risk, key leaked, cross-user access, service fully down | Drop everything now |
| 2 | Major feature down (Ask, sync, sign-in) for many users; spend runaway | Within 1 hour |
| 3 | Degraded or single-user issue | Next working day |

**First 15 minutes**
1. Open an incident note (time-stamped): what was seen, by whom, when.
2. Look: `fly status`, `fly logs -a nugget-cloud`, `/readyz`, Fly metrics dashboard, Sentry,
   Supabase dashboard (Logs, Reports), provider status pages.
3. Decide the severity. For Sev 1, contain before you investigate further.

**Contain**
- Stop the service: `fly scale count 0 -a nugget-cloud` (desktop shows the cloud as unavailable).
- Revoke leaked keys (section 4): provider key first, then set the replacement.
- Cut a single user off: delete their sessions in Supabase Auth (or ban the user) and disable their
  Composio connected accounts.
- Runaway AI spend: lower the provider spend limit or disable the key.
- Preserve evidence: copy relevant logs (`fly logs` output, Supabase logs) before they age out.

**Communicate**
- Status: a short note on the website/status channel for Sev 1–2.
- **UK GDPR breach notification:** if personal data is (or may be) affected, assess the risk and,
  unless it is unlikely to result in a risk to people, **notify the ICO within 72 hours** of becoming
  aware (ico.org.uk, report a breach). If the risk to people is **high**, tell affected users
  **without undue delay**, in plain language: what happened, what data, what we did, what they
  should do. Record every breach, notified or not, in the breach log with the reasoning.
- Tell processors where relevant (Supabase, Composio, Fly) and ask for their logs.

**Afterwards:** a blameless postmortem within 5 working days: timeline, impact (users, data), root
cause, what worked, actions with owners and dates. Update this runbook.

## 7. Delete a user's data on request

**Preferred:** the user clicks **Delete account** in the app, which calls `DELETE /v1/me`. That
deletes their Composio connected accounts first (if that fails nothing is deleted and they can
retry), then the Supabase auth user, which cascades to every public table.

**Manual procedure** (emailed request):
1. **Verify identity:** the request must come from the account's email address, or ask the sender
   to confirm from it. Do not act on a request from another address.
2. **Find the user id:** Supabase Dashboard → Authentication → Users, search by email. Note the
   UUID. Collect their Composio account ids *before* deleting anything:
   ```sql
   select id, toolkit, composio_account_id from public.connections where user_id = '<id>';
   ```
3. **Delete Composio connected accounts** for that user id (dashboard, or API):
   ```sh
   curl -sS -H "x-api-key: $COMPOSIO_API_KEY" \
     "https://backend.composio.dev/api/v3.1/connected_accounts?user_ids=<id>"
   curl -sS -X DELETE -H "x-api-key: $COMPOSIO_API_KEY" \
     "https://backend.composio.dev/api/v3.1/connected_accounts/<account_id>"
   ```
4. **Delete the auth user** (cascades to all public tables):
   ```sh
   curl -sS -X DELETE "$SUPABASE_URL/auth/v1/admin/users/<id>" \
     -H "apikey: $SUPABASE_SERVICE_ROLE_KEY" -H "Authorization: Bearer $SUPABASE_SERVICE_ROLE_KEY"
   ```
5. **Verify** in the SQL editor (runs as `postgres`, so RLS does not hide rows). Every count is 0:
   ```sql
   select 'profiles' t, count(*) from public.profiles where user_id = '<id>'
   union all select 'connections', count(*) from public.connections where user_id = '<id>'
   union all select 'items', count(*) from public.items where user_id = '<id>'
   union all select 'chunks', count(*) from public.chunks where user_id = '<id>'
   union all select 'entities', count(*) from public.entities where user_id = '<id>'
   union all select 'claims', count(*) from public.claims where user_id = '<id>'
   union all select 'cards', count(*) from public.cards where user_id = '<id>'
   union all select 'conversations', count(*) from public.conversations where user_id = '<id>'
   union all select 'messages', count(*) from public.messages where user_id = '<id>'
   union all select 'usage_events', count(*) from public.usage_events where user_id = '<id>'
   union all select 'auth.users', count(*) from auth.users where id = '<id>';
   -- New tables since this list was written:
   select table_name from information_schema.columns
    where table_schema = 'public' and column_name = 'user_id';
   ```
6. **Delete Storage objects** under `exports/<id>/` (they do not cascade): Dashboard → Storage →
   `exports` → folder `<id>` → delete, or list with
   `POST $SUPABASE_URL/storage/v1/object/list/exports` `{"prefix":"<id>/"}` and remove with
   `DELETE $SUPABASE_URL/storage/v1/object/exports` `{"prefixes":["<id>/<file>", ...]}`.
7. **Stripe** (once billing exists): delete the customer (this cancels subscriptions). Keep invoices
   only as long as tax law requires.
8. **Provider retention:** Anthropic and OpenAI may keep API inputs and outputs for up to 30 days
   for abuse monitoring; they then expire. Say so in the reply.
9. **Record** the user id, date and channel in the **deletion log** (outside the database; needed
   for backup restores, section 5). Do not record the email address once done.
10. **Reply** to confirm, **within one month** of the request (UK GDPR Art. 12; extendable by two
    months for complex cases if you tell them within the first month).

## 8. Data export on request

The user can use **Download my data** in the app, which calls `GET /v1/export` (all their rows as
JSON). For an emailed request, verify identity as in section 7 and ask them to use the app. If they
cannot, produce it yourself: run `select * from public.<table> where user_id = '<id>'` for every
table with a `user_id` column (as `postgres`), save as JSON, and send it through an expiring link.
Never send it to any address other than the account email. Respond within one month.

## 9. Observability

**Logs.** Structured JSON lines on stdout (`NUGGET_LOG_FORMAT=text` for local dev only). Each
request is logged once with `request_id`, method, route template, status and `latency_ms`. No user
id hash, emails, prompts, tokens or message bodies are ever logged. `RUST_LOG` sets the filter
(e.g. `RUST_LOG=info,nugget_cloud=debug`; set with `fly secrets set` or `[env]`).
```sh
fly logs -a nugget-cloud
fly logs -a nugget-cloud | grep '"status":5'
fly logs -a nugget-cloud | grep '<request-id>'
```
The request id is taken from a valid incoming `x-request-id` or `fly-request-id`, else generated,
and returned in the `x-request-id` response header. Ask users reporting a problem for it.
Fly keeps logs briefly; ship them elsewhere (Fly log shipper) for anything you need to keep.

**Health.** `/healthz` = process alive. `/readyz` = database reachable within 2 s (503 otherwise).

**Metrics.** Prometheus text at `GET /metrics` on the internal listener `NUGGET_METRICS_BIND`
(default `0.0.0.0:9091`), scraped by Fly via `[metrics] port = 9091, path = "/metrics"`. Not public.
View in Fly's Grafana (fly-metrics.net) or query Fly's managed Prometheus.

| Metric | Use |
|---|---|
| `heed_http_requests_total{route,status}` | Traffic and error rate per route |
| `heed_http_request_duration_seconds` | Latency histogram (p50/p95/p99) |
| `heed_rate_limited_total{scope}` | 429s by limiter (ip, user, ask, sync) |
| `heed_sync_runs_total{result}` | Sync health; rising failures = Composio or token trouble |
| `heed_gateway_cost_micros_total` | AI spend (micro-USD); compare with provider dashboards |
| `heed_db_pool_size`, `heed_db_pool_available` | Pool pressure; available near 0 = exhaustion |

Useful queries:
```promql
sum(rate(heed_http_requests_total{status=~"5.."}[5m])) / sum(rate(heed_http_requests_total[5m]))
histogram_quantile(0.95, sum by (le) (rate(heed_http_request_duration_seconds_bucket[5m])))
increase(heed_gateway_cost_micros_total[1d]) / 1e6        # USD per day
```

**Errors.** Optional `SENTRY_DSN` and `SENTRY_ENVIRONMENT`. 5xx responses and panics are reported
with route, status and request id only.

**Limits and tuning** (env vars, set with `fly secrets set` or `[env]` in `fly.toml`):

| Variable | Default | Notes |
|---|---|---|
| `DATABASE_POOL_MAX` | 8 | Per machine. Keep machines × this under the Supabase pooler limit |
| `DATABASE_POOL_TIMEOUT_SECS` | 5 | Waiting longer for a connection fails with 503, not a hang |
| `DATABASE_STATEMENT_TIMEOUT_MS` | 15000 | Set per transaction |
| `NUGGET_RATE_IP_PER_MIN` | 300 | In-process, **per machine** |
| `NUGGET_RATE_USER_PER_MIN` | 120 | Per machine |
| `NUGGET_RATE_ASK_PER_MIN` | 10 | Asks per user, per machine |
| `NUGGET_SHUTDOWN_GRACE_SECS` | 25 | Must stay below `kill_timeout = 30` |
| `NUGGET_CLIENT_IP_HEADER` | `fly-client-ip` | Header with the real client IP; set empty to use the TCP peer (only when not behind Fly's proxy, or the header is spoofable) |
| `NUGGET_METRICS_BIND` | `0.0.0.0:9091` | `off` disables the metrics listener; never route it publicly |

Fixed: `POST /v1/sync` at most once per 60 s per user (`NUGGET_SYNC_EVERY_SECS`), `GET /v1/export` once per 60 s; 429 carries `Retry-After`. Bodies over
64 KiB get 413 (webhooks: 256 KiB). Timeouts: 60 s for Ask, 30 s for everything else. The sync
scheduler claims due connections with a lease (`nugget_private.claim_due_connections`), so several
machines never sync the same connection at once.

## 10. Common alerts and what to do

**`/readyz` failing / machines unhealthy**
1. `fly status`, `fly logs` for database errors (auth failure, TLS, timeout).
2. Supabase status page and Dashboard → Reports (CPU, connections). Paused or over-limit project?
3. Password just rotated? Check `DATABASE_URL` matches (section 4.3).
4. If Supabase is down, there is nothing to fail over to: post a status note and wait.

**5xx spike**
1. `fly logs | grep '"status":5'`, group by route; Sentry for the request ids.
2. Just deployed? Roll back (section 3) first, investigate after.
3. One route only: check its dependency (Ask → AI provider; connections/sync → Composio).

**Composio Hobby cap reached** (sync errors from Composio, usage paused)
Usage **pauses for everyone**: no syncs, no new connections. Upgrade the project to Pro in the
Composio dashboard (takes effect immediately), then trigger syncs to catch up. Prevention: move to
Pro at about 70% of the monthly 100k tool calls.

**AI quota / 429s from Anthropic or OpenAI**
- Provider 429 or "credit balance" errors: check the console's rate tier and spend limit; raise the
  limit or top up. If only Anthropic is affected and `OPENAI_API_KEY` is set, the gateway fallback
  covers Asks; otherwise Asks fail until fixed.
- Users seeing `429 quota_exceeded`: that is our per-plan monthly quota working as designed. Check
  `usage_events` for the user if they dispute it.
- Our own limiter (`heed_rate_limited_total{scope}`) high for one scope: probably a loop in a client
  or abuse; find the request ids in logs.

**DB pool exhaustion** (`heed_db_pool_available` at 0, 503s after 5 s waits)
1. Look for slow queries: Supabase Dashboard → Query Performance; statement timeout is 15 s.
2. Check the sync pass is not hogging connections (large backfills).
3. Raise `DATABASE_POOL_MAX` only if machines × pool stays under the pooler limit; otherwise
   upgrade the Supabase compute size.

**Sync backlog** (many connections overdue, `heed_sync_runs_total{result="error"}` rising)
```sql
select count(*) filter (where next_sync_at < now() - interval '1 hour') as overdue,
       count(*) filter (where last_error is not null) as erroring
  from public.connections where status = 'active';
select toolkit, left(last_error, 120), count(*) from public.connections
 where last_error is not null group by 1, 2 order by 3 desc;
```
Causes: Composio cap or outage, expired Google consent (user must reconnect), one huge mailbox.
Add a machine (`fly scale count 2`) only if passes are slow; leases stop double syncing.
