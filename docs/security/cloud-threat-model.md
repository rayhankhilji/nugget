# Heed Cloud threat model and review

Status: review of the merged M1 cloud code (base `claude/inspiring-wozniak-z3gg3n` at `fa553526`) plus the fixes on `sec/cloud`. Date: 2026-11-02. Owner: SEC1.

**Scope:**
- Supabase Auth, Postgres with RLS (`supabase/`)
- `crates/nugget-cloud` (axum on Fly.io)
- Composio
- the AI gateway
- Stripe (in flight)
- the Tauri desktop app with `crates/nugget-cloud-client`

See [architecture-cloud.md](../architecture-cloud.md) for the design. The operational side is in [the runbook](../ops/runbook.md) and [the pre-launch checklist](../ops/prelaunch-checklist.md).

**Severity:**
- **Critical:** cross-user data exposure or account takeover, exploitable now.
- **High:** cross-user impact or a cost or availability failure for everyone.
- **Medium:** a single-user impact, or a defence-in-depth gap on a likely path.
- **Low:** hardening.

**Status:**
- **Fixed:** on `sec/cloud`, with a regression test.
- **Open:** needs work; the owner is named.
- **Accepted:** a known residual risk, with the reason given.

No Critical issue was found. Tenant isolation (RLS, plus the `nugget_service` role subject to RLS) holds under every test, including the new schema-wide checks.

## Assets and trust boundaries

| Asset | Where | Who must never get it |
|---|---|---|
| Users' mail, events and derived chunks, entities, claims and messages | Postgres (`public.*`) | Other users; the public; LLM providers beyond the Ask/extraction context |
| Google OAuth grants | Composio (we hold only connected-account ids) | Anyone but the owning user's server-side calls |
| Supabase refresh token | OS keychain (`nugget/cloud-session`) | Other local apps, logs |
| Provider keys (Anthropic, OpenAI, Composio, service role, Stripe) | Fly secrets | Clients, logs, the repository |
| The AI and Composio budget | Metered in `usage_events`; Composio Hobby cap | A single abusive user (shared-fate cap) |

The trust boundaries are:
- desktop ↔ API (Bearer JWT);
- API ↔ Postgres (TLS, `nugget_service` role with RLS);
- API ↔ Composio, Anthropic and OpenAI (HTTPS, our keys);
- Composio and Stripe → API (signed webhooks);
- browser → desktop (deep link: **untrusted**, because any web page or app can open it);
- synced content → LLM (**untrusted**, because anyone can email the user).

## Findings

| # | Sev | Area | Finding | Status |
|---|---|---|---|---|
| 1 | High | Rate limits | There were no per-IP or per-user limits. Every unauthenticated request cost a call to Supabase `/auth/v1/user`. A user could loop `POST /v1/sync` and `GET /v1/connections`, each of which calls Composio. On the Composio Hobby plan, hitting the shared cap **pauses ingest for all users**. | **Fixed:** token buckets per IP (`Fly-Client-IP`), per user, per user for Ask (10 a minute), and a `/v1/sync` cooldown (60 s). Rejections return 429 with `Retry-After`. Tests: `ratelimit.rs`, `api/tests/security.rs` (`asks_and_syncs_are_rate_limited_per_user`, `ip_limit_uses_the_proxy_header_and_spares_health_checks`). |
| 2 | High | Cost, shared fate | The Composio cap is still shared. Background sync for a user with very heavy mail is bounded only by `MAX_PAGES` per pass, and Composio tool calls do not count against the user's quota. | **Open (SEC1/AR1):** add a per-user daily tool-call budget (`usage_events.kind = 'tool'`), alert at 70% of the Composio cap, and move to Composio Pro before launch (checklist). |
| 3 | Medium | Prompt injection | Synced text was wrapped in `<source>` tags, but only the exact string `</source>` was escaped. `</SOURCE>`, `</source >` or a fake `<source n="9">` let an email close its source and pose as instructions or as another numbered source. Titles and authors could also carry newlines. | **Fixed:** `ask::neutralise_tags` (case- and space-insensitive) and one-line attributes. Tests: `injected_tags_in_any_form_are_neutralised`. |
| 4 | Medium | Prompt injection (residual) | Content can still steer an answer's wording; this is inherent to LLMs. M1 has no tool use, so injection cannot cause actions yet. | **Accepted for Ask.** For actions, **Open (CL4):** see [Notes for in-flight work](#notes-for-in-flight-work). |
| 5 | Low | Output handling | Citation `url`s came from synced data (Calendar `htmlLink`). The desktop's `open_url` classifies URLs, but the API passed through any scheme. | **Fixed:** only plain `https` URLs without credentials are cited (`ask::safe_link`, test `only_https_links_are_cited`). |
| 6 | Medium | RLS completeness | A plain foreign-key check bypasses RLS, so a service bug could attach a chunk, claim or message to **another user's** item, entity or conversation. A's export would then include B's text through a join. | **Fixed:** composite same-user foreign keys (migration `20261102130000`). Test: `supabase/tests/hardening.sql`. |
| 7 | Medium | RLS completeness | Supabase's default privileges grant anon and authenticated everything on new tables and functions in `public`. Any later migration that forgot its `revoke` would expose a table through the Data API. | **Fixed:** default privileges are revoked. `hardening.sql` now asserts, for **every** table in `public`, that RLS is on, a policy exists, anon has no grant and authenticated cannot write. It also asserts that no view is readable without `security_invoker`. It runs on every `run.sh`, so it covers the tables other workers add. |
| 8 | Low | Functions | `nugget_private.current_user_id()` had no pinned `search_path` and was executable by `PUBLIC` (the schema's usage grant made that unreachable). | **Fixed:** `search_path` is pinned, and nothing in `nugget_private` is executable by `PUBLIC`. `hardening.sql` checks that every `SECURITY DEFINER` function pins `search_path` and that `nugget_private` functions are not executable by the API roles. |
| 9 | Low | Data API | `authenticated` could `DELETE` its own `connections` rows directly. That deleted the items but left the Google grant alive at Composio, and the next listing re-added the connection. | **Fixed:** revoked. Disconnecting goes through `DELETE /v1/connections/{id}`, which revokes at Composio first. |
| 10 | Medium | Sync scheduling | `due_connections` did not claim rows. Two Fly machines, or a pass slower than the 5-minute tick, would sync the same connection twice (double Composio cost and racing cursors). The pass was also sequential: 20 connections per 5 minutes. | **Fixed:** `nugget_private.claim_due_connections` uses `FOR UPDATE SKIP LOCKED` plus a 15-minute lease, and syncs 4 connections at a time. Tests: `hardening.sql` (lease) and `concurrent_scheduler_passes_never_sync_a_connection_twice`. The load test reports capacity. |
| 11 | Medium | Webhooks | The HMAC and the 5-minute timestamp check were correct, constant-time, and accepted several signatures. A captured delivery could still be **replayed** within the 5 minutes. The impact was low, because a webhook only marks a connection due. | **Fixed:** a replay guard keyed on the delivery id; replays get 200 with no effect. Tests: `replays_are_detected`, `replayed_webhooks_are_not_acted_on`. **Accepted:** the guard is per machine, so a replay to a second machine is possible, and it can only mark a connection due. |
| 12 | Low | Auth cache | Verified tokens were cached for 60 s by SHA-256 hash, even past the token's own `exp`. | **Fixed:** the cache entry never outlives `exp` (`cache_never_outlives_the_token`). **Accepted:** a signed-out or revoked token can still work for up to 60 s. Account deletion purges the user's cache entries. |
| 13 | Medium | Limits | There was no explicit body limit (axum's default is 2 MB), no request deadline, and the DB pool had no wait timeout or statement timeout: overload produced hung requests. | **Fixed:** 64 KiB bodies (256 KiB for webhooks), deadlines of 30 s (60 s for Ask), and a pool wait/create/recycle timeout of 5 s that returns 503. `statement_timeout` is 15 s per transaction. Test: `oversized_bodies_are_refused`. |
| 14 | Medium | Export | `GET /v1/export` builds the user's whole corpus as one JSON value in memory. A heavy user can push a 512 MB machine into memory exhaustion (OOM), and it was repeatable. | **Fixed (partly):** limited to once a minute per user (`export_is_limited_to_once_a_minute`). **Open (SEC1, M2):** stream the export, or write it to Storage `exports/<uid>/` and return a signed URL. |
| 15 | Low | Quota | Quota is checked before the call and recorded after it. That is a race: concurrent Asks can overshoot the monthly quota. | **Accepted:** the Ask rate limit (10 a minute) bounds the overshoot to about 10 answers, roughly $0.01 on Free and $0.2 on Pro. `docs/ops/loadtest.md` measures it. Reserve-then-settle is planned with streaming (CL3). |
| 16 | Low | Error leakage | Composio and Supabase Auth error text was returned to clients. | **Fixed:** those messages are logged, and the client gets a generic message. |
| 17 | Medium | PKCE and deep link | The flow is sound: S256, a verifier that lives only in memory, single use, a 10-minute TTL, and a strict callback parser. Because the scheme can be claimed by any app, an intercepted code is useless without the verifier. However, **a forged code consumed the pending sign-in**: any web page could open `nugget://auth/callback?code=x` and cancel a real sign-in. | **Fixed:** a refused code no longer ends the pending sign-in (`a_forged_code_does_not_cancel_the_real_sign_in`). |
| 18 | Low | `heed://` move | The callback parser accepted only `nugget://`. | **Fixed:** the parser accepts `heed://` and `nugget://` (`CALLBACK_SCHEMES`). **Open (rename owner):** `deeplink.rs` dispatch, the `tauri.conf.json` schemes and the Supabase redirect allow-list still use `nugget://`. During the transition, allow-list exact URLs only, never wildcards. |
| 19 | Info | Keychain | The refresh token is in the OS keychain and the access token is in memory only. `Debug` output is redacted. If the keychain fails, the session lasts only for that launch; it never falls back to a file. | OK. |
| 20 | Info | Secrets | The desktop bakes in only `NUGGET_SUPABASE_URL`, `NUGGET_SUPABASE_ANON_KEY` and `NUGGET_CLOUD_URL`, all public by design. Provider and service-role keys are read only by the service. `Config`'s `Debug` output hides them. | OK. Checklist: Fly secrets only, and secret scanning on the repository. |
| 21 | Info | SSRF | The service fetches only fixed base URLs from config. Account ids in paths come from Composio, and toolkit slugs are checked against an allow-list. No user-controlled URL is fetched. | OK today. **Open (CL4):** keep it so for apps and actions. |
| 22 | Medium | Logging | The service wrote no request logs and no request ids. Sync errors were logged with upstream text. | **Fixed:** one JSON line per request with `request_id`, method, route template, status and latency. It contains no user id, IP, email, token, prompt or body. Sync errors are logged as a flag; the text goes only to the user's own `connections.last_error`. Error reports carry only the route, status and request id. |
| 23 | Medium | GDPR deletion | `DELETE /v1/me` revokes Composio first, then deletes the auth user, which cascades. Three copies remain outside that path:<br>• backups (7 days);<br>• provider logs (Anthropic and OpenAI, up to 30 days);<br>• from billing onwards, Stripe. | **Accepted:** disclose backups and provider logs in the privacy policy, and re-apply deletions after a restore ([runbook §5 and §7](../ops/runbook.md)). **Stripe:** handled on `ar/cloud` at `57b0d9bf`, reviewed by SEC1. `DELETE /v1/me` deletes the Stripe customer before the auth user, and aborts if that fails. |
| 24 | Info | GDPR export | `GET /v1/export` covers every user table. Embeddings and chunks are left out because they are derived. | OK. New tables (billing, cards, actions) must be added to the export. |
| 25 | Medium | Database role | `set local role nugget_service` drops privileges, but if `DATABASE_URL` logs in as `postgres`, anything that could run `reset role` would regain them. All queries are parameterised, so this is not reachable today. | **Open (founder):** log in as a dedicated `nugget_service` LOGIN role (checklist). |
| 26 | Low | Rate limits | The limits are in-process, so with N machines a client gets N times the limit. | **Accepted:** the AI quota is global and is the hard cost limit. |
| 27 | Medium | Backups | Supabase Pro keeps daily backups for 7 days; PITR is a paid add-on. No restore has been rehearsed. A restore into a new project changes the URL and keys that are baked into the desktop. | **Open (founder):** rehearse a restore (checklist) and decide on PITR before paid launch. |
| 28 | Info | Dependencies | `cargo deny check` passes: advisories, bans, licences and sources are all OK. The two documented ignores are tauri-only. | OK. Run it in CI on every PR. |
| 29 | Low | Supabase Auth settings | OTP and magic-link rate limits, email enumeration, the redirect allow-list and leaked-password protection are dashboard settings, not code. | **Open (founder):** see the checklist. |
| 30 | Medium | Availability (load test) | The 30 s deadline (finding 13) cut off long `/v1/sync` requests mid-run, so the cursor was never recorded and the connection stayed due. That happened for every user's first sync with embeddings under load, and for one large mailbox even with no load. | **Fixed:** the sync runs detached (`an_abandoned_sync_request_still_records_its_result`). **Closed on `ar/cloud` at `70ce3c42` (AR1):** embeddings are batched (up to 100 inputs per call). `/v1/sync` answers 202 after 20 s while the sync continues, and a per-connection run lease (`syncing_until`) stops a request and the scheduler running in parallel. This lands here with the next integration merge. See `docs/ops/loadtest.md`. |
| 31 | Low | Search (load test) | Vector search filters by `user_id` after ordering by distance. On a large `chunks` table the planner may switch to the HNSW index, and pgvector 0.6 then filters afterwards, so a user can get few or no candidates. This is a quality and availability issue, not a leak, because RLS still applies. | **Open (CL2):** use pgvector ≥ 0.8 with `hnsw.iterative_scan`, or over-fetch, or partition by user. |
| 32 | Medium | Billing devices (load test) | `register_device` counted and then inserted with no lock. Concurrent first requests from a new device failed with a duplicate-key 500, and concurrent new devices could slip past the plan's device limit. | **Fixed:** a per-user transaction advisory lock plus an upsert (`concurrent_first_requests_from_a_new_device_all_succeed`). |

## Notes for in-flight work

- **AR1 (Stripe billing):** reviewed at `ar/cloud` `57b0d9bf`; every item below is addressed. A trial merge with `sec/cloud` passes `hardening.sql` (including the billing tables) and all nugget-cloud tests. Two notes:
  - The signature check re-serialises `t` instead of using the raw header value. That is harmless with Stripe's plain-digit timestamps.
  - Device ids come from the client, so the device limit is a fair-use limit, not a security control.

  The requirements I sent:
  - new tables must pass `hardening.sql` and grant explicitly;
  - the plan is changed only by verified webhooks;
  - Stripe signatures are checked on the raw body, with a 300 s tolerance, and processing is idempotent on `event.id`;
  - the route needs `DefaultBodyLimit` raised to 256 KiB;
  - user↔customer lookups go only through a `nugget_private` definer function, and ids are set server-side;
  - redirect URLs come from config only;
  - `DELETE /v1/me` deletes the Stripe customer first, and billing rows are added to the export;
  - no customer data in logs, and generic error text;
  - no plan caching.
- **CL2 (knowledge, search, cards):**
  - `/v1/search` must use `db::tsquery` (it strips tsquery operators) and the RLS-scoped `Db::scoped`.
  - `cards.item_ids` is a `uuid[]` with no foreign key, so check that every id belongs to the user before storing it.
  - Claims now need `item_id`, `subject_id` and `object_id` to belong to the same user; the foreign keys enforce it.
  - Extraction prompts must wrap source text with `ask::neutralise_tags`.
  - New tables must pass `hardening.sql`.
- **CL3 (gateway streaming):**
  - Keep `neutralise_tags`, the one-line attributes and `safe_link` on the streaming path.
  - The `deadline` middleware covers the response head only. A streamed body must bound its own total time and close when the client disconnects.
  - Record usage even when the client disconnects mid-stream, and use reserve-then-settle to close finding 15.
  - Never log prompt or completion text.
- **CL4 (actions and apps):**
  - Every action proposed from model output must go through `nugget-policy`, with Send-class actions set to `Confirm`. Execute only `POST /v1/actions/{id}/approve`, made by the owning user.
  - Store the exact tool and arguments that were previewed, and execute exactly those.
  - Allow-list tool slugs. Never execute a tool name or URL taken from synced content or model output without the allow-list.
  - Do not fetch arbitrary URLs. If a feature needs to, resolve and block private, link-local and metadata addresses after DNS, allow https only, cap redirects, size and time.
  - Rate-limit approvals.

## Residual risk summary

Before launch, the items that matter are:
- **2:** Composio shared cap and a per-user tool budget.
- **25:** a dedicated DB login role.
- **27:** a rehearsed restore.
- **23:** privacy-policy disclosures.
- the AR1 billing items.

Everything else is fixed or accepted above.
