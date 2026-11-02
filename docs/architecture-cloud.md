# Nugget Cloud architecture

Status: design plus M1 build in progress, on branch `ar/cloud`. Date: 2026-11-02.

Nugget moves from local-first to **cloud-first SaaS**:
- Users have accounts.
- Their mail, events and docs are indexed in our cloud.
- The AI runs server-side with our provider keys.

The desktop app stays the only client. Everything that needs the local OS stays on the desktop: Double Tap, quick ask, the hotkey, accessibility and computer use. We drop the claim that "data never leaves your machine". The [Security](#security) section says what does leave and where it goes.

## 1. Components

```
 Desktop (Tauri)                         Cloud
 ┌───────────────────────────┐   HTTPS   ┌──────────────────────────────┐
 │ nugget-desktop (UI)       │──────────▶│ nugget-cloud (Rust, axum)     │
 │ nuggetd: Double Tap, a11y,│  Bearer   │  /v1/ask  /v1/connections     │
 │ hotkey, overlay, agent    │   JWT     │  /v1/sync /v1/export /v1/me   │
 │ keychain: refresh token   │           │  /v1/webhooks/composio        │
 └────────────┬──────────────┘           │  AI gateway + metering        │
              │ browser sign-in          └──┬──────────┬───────────┬─────┘
              ▼                             │ SQL      │ HTTPS     │ HTTPS
 Supabase Auth (magic link,  ◀──────────────┤          ▼           ▼
 Google) → nugget://auth/callback           │     Composio     Anthropic /
                                            ▼     (OAuth,      OpenAI
                                   Supabase Postgres  tools,    (LLM, embeddings)
                                   + pgvector + Storage triggers)
```

### Supabase (Pro plan)
- **Auth:** email magic link and Google sign-in in M1. Apple sign-in waits until M4, because it needs an Apple Developer account ($99/yr). The desktop app uses the PKCE flow through the system browser, and the redirect URL is `nugget://auth/callback` (allow-listed in Auth → URL Configuration). Supabase issues the JWTs. The cloud service checks each token by calling `GET /auth/v1/user`, and caches the result for 60 seconds per token hash. Because of this, the service works whichever JWT signing scheme the project uses (legacy HS256 or asymmetric keys).
- **Postgres + pgvector.** The schema is in `supabase/migrations/`. Every table has a `user_id` column and Row-Level Security enabled:
  - `profiles`: plan (`free` or `pro`), created by a trigger on `auth.users`.
  - `connections`: one row per Composio connected account (toolkit, status, sync cursor).
  - `items`: one email, event or doc (source, external id, title, author, time, body, metadata, content hash). Unique on `(user_id, source, external_id)`, so ingest is idempotent.
  - `chunks`: text, a generated `tsvector` (the `simple` config, so it works in any language), and `embedding vector(1536)`, which can be null. It has a GIN index on the `tsvector` and an HNSW cosine index on the embedding.
  - `entities` and `claims`: the knowledge graph. Every claim points to its source item, so it carries provenance.
  - `cards`: proactive cards. `conversations` and `messages`: Ask history, with citations stored as JSON.
  - `usage_events`: one row per LLM, embedding or tool call (tokens and cost in micro-USD). This table is the metering ledger.
- **RLS model:**
  - The `authenticated` role (a user's own JWT through PostgREST) may only `select` and `delete` rows where `user_id = auth.uid()`. It cannot write. All writes go through the service, so quotas cannot be bypassed.
  - The service connects as a dedicated role, `nugget_service`. It does **not** bypass RLS. Each request runs in a transaction that starts with `set_config('nugget.user_id', <uid>, true)`, and the policies for `nugget_service` compare against that setting. A missing `where user_id = …` in service code therefore returns nothing, never another user's rows.
  - Lookups that must cross users go through `security definer` functions in the `nugget_private` schema. Only `nugget_service` may execute them. There are two: finding which user owns a Composio account (for webhooks), and listing connections that are due for sync.
- **Storage:** a private bucket, `exports`, holds data exports for GDPR. A user can read only objects under their own `<uid>/` prefix. Attachments are not stored in M1.

### Cloud service: a small Rust service, not Edge Functions
**Decision: `crates/nugget-cloud`, an axum binary on Fly.io (shared-cpu-1x, region `lhr`).** It talks to Postgres directly, through the Supabase pooler, using `tokio-postgres` over rustls. TLS is verified against Supabase's root CA (`DATABASE_CA_CERT`) or the OS trust store. Deployment files: `crates/nugget-cloud/Dockerfile` and `fly.toml`.

| | Supabase Edge Functions | Rust service on Fly.io |
|---|---|---|
| Cost | Included in Pro (2M invocations) | about $2–4/month (256–512 MB machine, which is $2.19 per 30 days at 256 MB) |
| Limits | 2 s of CPU and 400 s wall clock per request, 256 MB | none that matter here |
| Reuse | Deno/TS, so the Rust crates could only be reused through WASM | reuses `nugget-retrieve`, `nugget-knowledge`, `nugget-policy` and `nugget-draft` directly |
| Background sync | needs pg_cron plus chained invocations | an in-process tokio scheduler |
| Tests | Deno, which our toolchain does not have | `cargo test` against local Postgres, as in this repo |

The Rust service costs about £2–3 a month and lets us keep a single language, the existing ranking, extraction and policy code, and long ingest jobs. Edge Functions remain an option for tiny glue code, such as an auth hook, if one is ever needed. Hetzner CX23 (€5.49/month since 2026-06-15) is the fallback host. Its smallest plans have sometimes been unavailable to order.

### Ingest workers
These run inside `nugget-cloud`, as a tokio task per process:
1. **Sync loop.** Every 5 minutes it asks `nugget_private.due_connections()` for connections that are due. Gmail and Calendar are synced every 30 minutes while the user's desktop app is active (it sends a heartbeat), and once every 24 hours otherwise. A user can also run a sync on demand with `POST /v1/sync`.
2. **Fetch.** It calls Composio tools as that user: `GMAIL_FETCH_EMAILS` with an `after:` query built from the cursor, and `GOOGLECALENDAR_EVENTS_LIST` with `timeMin`/`timeMax` and `updatedMin`.
3. **Normalise.** Each email or event becomes an `items` row. HTML is stripped, quoted replies are trimmed, and the body is capped at 64 KB. If the content hash has not changed, the item is skipped, so re-syncs cost nothing.
4. **Chunk and embed.** Text is split into chunks of about 1,200 characters with overlap. Chunks get embeddings only if an embedder is configured (see the AI gateway). Full-text search always works.
5. **Extract** (M2). `nugget-knowledge` extraction runs on new items, using the gateway's cheap model, and writes to `entities` and `claims`.

## 2. Composio
- **Identity.** The Composio `user_id` is the Supabase user UUID. One Composio project serves all users, and the API key lives only in the server environment (`COMPOSIO_API_KEY`). The REST base URL is `https://backend.composio.dev/api/v3.1`, with the header `x-api-key`. `v3` is frozen but still supported.
- **Auth configs.** These are created once per toolkit in the Composio dashboard, using Composio-managed OAuth for Gmail and Google Calendar. Their ids go in `COMPOSIO_AUTH_CONFIGS`, for example `gmail=ac_…,googlecalendar=ac_…`.
- **Connecting an account:** `POST /v1/connections {toolkit}` → the service calls Composio to create a connected-account link with `callback_url = nugget://connections/callback` → the desktop opens `redirect_url` in the system browser → after consent, Composio redirects to the deep link → the desktop calls `GET /v1/connections`. That call refreshes statuses from Composio and records each ACTIVE account in `connections`, then the first sync runs.
- **Triggers or polling.** Composio's Gmail and Calendar triggers are themselves polling-based, with a 2–15 minute floor. They cost **$0.003 per event** on the paid tier, ten times the price of a tool call ($0.0003). A user receiving 1,000 emails a month would cost $3 in trigger events alone. **M1 therefore polls with tool calls**, adaptively as described above, which costs about 1,500–3,000 calls per active user per month. `POST /v1/webhooks/composio` is implemented: it verifies the signature, and for an event it only marks that connection due (it does not ingest the payload). This lets us switch triggers on per toolkit later, for example for low-volume apps, without a second ingest path.
- **Actions** are described under [Actions](#actions) below.
- **Pricing** (composio.dev/pricing, for signups after 2026-08-15):
  - Hobby: $0, with 100k tool calls and 50k trigger events a month. Usage *pauses* at the cap.
  - Pro: $29/month, with the same allowance as a credit, then $0.0003 per call and $0.003 per trigger event.
  - **The decision: start on Hobby** (confirmed by the founder). It covers about 30–60 active users. Move to Pro before the cap, because a pause stops ingest for everyone.

## 2a. Apps

`GET /v1/apps` is the Connections catalogue. Each entry has the slug, name, logo, categories, `syncs`, the action kinds, `sync_mode`, `connectable` (this deployment has a Composio auth config for it) and `connected`. Logos come from Composio's `GET /toolkits` (`meta.logo`). When that call fails, they come from `https://logos.composio.dev/api/<slug>`, which serves every slug but is not documented.

| App | Toolkit | Syncs | Mode | Actions |
|---|---|---|---|---|
| Gmail | `gmail` | `GMAIL_FETCH_EMAILS` | poll | send, reply |
| Google Calendar | `googlecalendar` | `GOOGLECALENDAR_EVENTS_LIST` (`updatedMin`, `showDeleted`) | poll | create, update (`PATCH_EVENT`) |
| Google Drive | `googledrive` | `FIND_FILE` backfill, then `GET_CHANGES_START_PAGE_TOKEN` / `LIST_CHANGES`; Docs text via `DOWNLOAD_FILE` (`text/plain`) | poll | none |
| Outlook | `outlook` (mail and calendar) | `OUTLOOK_LIST_MESSAGES` (`response_detail: full`), `OUTLOOK_LIST_EVENTS` | poll | send, reply, create event |
| Slack | `slack` | `SLACK_LIST_ALL_CHANNELS`, `SLACK_FETCH_CONVERSATION_HISTORY` | poll | post (`SLACK_SEND_MESSAGE`, `markdown_text`) |
| Microsoft Teams | `microsoft_teams` | `CHATS_GET_ALL_CHATS`, `LIST_USER_CHAT_MESSAGES` | poll (Composio has no Teams triggers) | none |
| Notion | `notion` | `NOTION_SEARCH_NOTION_PAGE`, `NOTION_GET_PAGE_MARKDOWN` | trigger (`NOTION_PAGE_CREATED`, `NOTION_PAGE_CONTENT_UPDATED`) | create page |

Tool names and inputs were checked against Composio's toolkit pages (`docs.composio.dev/toolkits/<slug>`) on 2026-11-02. Composio documents outputs only as `data`. The parsers therefore read the upstream APIs' own field names (Gmail, Calendar, Drive v3, Slack, Notion, Graph), accept a few other spellings, and skip what they cannot read; they never fill it in. Each adapter's module comment says what is verified and what is defensive.

**Adapters** (`crates/nugget-cloud/src/apps/`). There is one `SyncAdapter` per app, so adding an app means writing one file. An adapter gets a `Tools` handle bound to one connected account. That handle:
- allows only the adapter's declared read tools,
- enforces the app's rate limit (a per-account token bucket),
- caps the number of calls per sync,
- meters every call.

The adapter returns a `Pull` containing:
- items to upsert; unchanged items are skipped by content hash,
- explicit deletes (trashed, cancelled or archived items, and Slack tombstones),
- complete listings of time windows. A stored item in a fully listed window that is missing from the listing is deleted.

An empty listing never deletes anything. A rate-limited sync keeps its cursor and retries within 15 minutes.

**Rate limits.** Slack throttles non-Marketplace apps, which includes Composio's managed app, to 1 `conversations.history` call a minute and 15 messages a page. Each Slack sync therefore reads 3 channels, least recently synced first, and resumes long histories from the page cursor. A Slack app of our own on the Marketplace would lift this limit. The other apps run well under their upstream limits: Notion about 3 a second, Graph 10,000 per 10 minutes, Google per-user quotas.

**Triggers versus polling, and cost.** A trigger event costs $0.003 on Composio Pro, ten times a tool call ($0.0003). With Composio-managed OAuth it costs $0.005 against $0.0005, and the free allowance is 10K events and 20K calls. So:
- High-volume apps (mail, chat) poll.
- Notion uses webhook triggers, because pages change rarely. Triggers are enabled once per connection, and only when `COMPOSIO_WEBHOOK_SECRET` is set. They mark the connection due. A daily poll is the safety net.
- Gmail's and Drive's triggers are polling-based on Composio's side anyway, so they would add cost without saving anything.

A webhook is accepted only with a valid signature, and only if its `metadata.user_id` (when present) matches the account's owner.

**Not synced yet:**
- Slack thread replies and user display names (authors are user ids).
- Teams channel messages.
- Notion pages deleted for good. Archived and trashed pages are handled.
- Drive files other than Google Docs are indexed by name only. Docs text is used when Composio returns it inline; `downloaded_file_content`'s shape is not documented.

## Actions

The actions are:
- send mail (Gmail, Outlook)
- reply in a thread (Gmail, Outlook)
- create an event (Google Calendar, Outlook)
- update a Google Calendar event
- post a Slack message
- create a Notion page

They all go through `pending_actions` (migration `20261102120001_apps_actions.sql`) and the `/v1/actions` routes:

| Route | |
|---|---|
| `POST /v1/actions` | `{kind, toolkit, input, idempotency_key}` → the action, `pending` |
| `GET /v1/actions[?status=]` | recent actions |
| `GET /v1/actions/{id}` | status |
| `GET /v1/actions/{id}/preview` | the dry run: fields, a before/after diff (an update compares against the synced event), and the digest |
| `POST /v1/actions/{id}/approve` | `{digest}`: runs it once |
| `POST /v1/actions/{id}/cancel` | |

**Proposing.** A proposal goes through these steps:
1. The input is validated: addresses, ids, lengths, one-line subjects, end after start.
2. It is turned into the exact Composio call.
3. `nugget-policy` assesses the call. Every action is Send-class, so the answer is Confirm, or Deny under a deny rule. A denied action is stored as `denied` and never runs. Nothing is ever auto-approved.

The call has some explicit settings:
- Calendar events set `create_meeting_room: false`.
- `send_updates` is `none` when there are no attendees.
- Whenever invitations will be emailed, the preview says so.

**Approving.**
- The digest is SHA-256 over the account, the tool and the canonical arguments. An approval must echo it, or it is refused (`412 digest_mismatch`).
- `pending` becomes `executing` under a row lock. Approving twice, approving concurrently, or proposing again with the same idempotency key never sends twice. The same key with a different call is a `409`.
- A failed send is not retried. Actions expire after 24 hours.

**Audit.** Every step is appended to `action_audit`, a per-user SHA-256 hash chain (`prev_hash`, `hash`) without message bodies:
- `proposed`, `denied`, `approval_rejected`, `approved`, `succeeded`, `failed`, `cancelled`, `expired`.

Neither the service role nor the user can update or delete the chain. The GDPR export includes both tables.

**Clients.** `nugget-cloud-client` has typed methods for these routes, and the desktop has the Tauri commands `cloud_apps`, `cloud_actions_list`, `cloud_action_propose`, `cloud_action_preview`, `cloud_action_status`, `cloud_action_approve` and `cloud_action_cancel`, wrapped thinly in `cloud.ts`. The confirmation UI belongs to the design work.

## 3. AI gateway
All model calls go through `nugget-cloud`'s gateway module. No provider key is ever sent to a client.
- **Interface:** `Llm::complete(user, tier, messages) -> (text, usage)` and `Embedder::embed(texts)`.
  - Anthropic Messages is the default, when `ANTHROPIC_API_KEY` is set. OpenAI Chat is the fallback, when only `OPENAI_API_KEY` is set.
  - Embeddings use OpenAI `text-embedding-3-small`, 1536 dimensions, at $0.02 per million tokens. Embeddings are on only when `OPENAI_API_KEY` is set. With only an Anthropic key, retrieval is full-text only, which is still useful, and the doc and the `/v1/me` endpoint say so.
- **Metering:** every call writes a row to `usage_events` (tokens and cost from a price table in code). Before each Ask, the gateway checks the user's month-to-date cost against their plan's quota and returns `429 quota_exceeded` when it is spent.
- **Models per tier** (prices per million tokens from the official Anthropic page, 2026-11):

| Tier | Ask / answer model | Extraction, cards | Monthly AI quota |
|---|---|---|---|
| Free | Claude Haiku 5.5 ($0.10 in / $0.50 out) | Haiku 5.5 | $0.75 of usage |
| Pro | Claude Sonnet 5.5 ($2 / $10) | Haiku 5.5 | $8 of usage |

Model ids are configuration (`NUGGET_MODEL_FAST`, `NUGGET_MODEL_SMART`), not code. Note that prompts over 100k tokens are billed at 5× on Haiku 5.5. The gateway caps the context it packs at 24k tokens, so that tier is never reached.

**Cost per active user per month.** Assumptions: 1,000 new emails and events a month, 10 asks a day, a 6k-token context and 400 output tokens per answer. The Claude 4.7+ tokenizer counts about 30% more tokens for the same text, and that is included below.

| Line | Free (Haiku) | Pro (Sonnet asks) |
|---|---|---|
| Embeddings (about 2M tokens) | $0.04 | $0.04 |
| Extraction (M2: 1,000 × 2.6k in / 400 out) | $0.46 | $0.46 |
| Asks (300 × 7.8k in / 520 out) | $0.31 | $6.24 |
| Composio tool calls (about 3k, Pro tier) | $0.90 | $0.90 |
| **Total** | **about $1.70 (£1.30)** | **about $7.60 (£5.80)** |

The first month adds about $0.5 for backfilling 90 days of history. Fixed costs:
- Supabase Pro: $25. Its $10 of compute credit covers the default Micro instance.
- Fly.io: about $3.
- Composio: $0 on Hobby, $29 on Pro.

That is **about $28 a month (£21) before users**, or $57 (£43) on Composio Pro. At £1.30 per active free user:
- The **£39 budget supports about 13 active free users** on Composio Hobby.
- **£99 supports about 40 users on Composio Pro, or 60 on Hobby.**

Asks are the variable that matters. Prompt caching and the 24k cap keep them bounded, and Pro pricing has to recover Sonnet's cost.

## 4. Desktop app changes
- **Sign-in:**
  1. A new crate, `crates/nugget-cloud-client`, builds the PKCE `authorize` URL (for Google; Apple is added in M4) or sends the magic-link request (`POST /auth/v1/otp` with a code challenge). The system browser opens it.
  2. Supabase redirects to `nugget://auth/callback?code=…`.
  3. `deeplink.rs` validates the link and hands the code over, and the client exchanges it with `grant_type=pkce`. The verifier lives only in memory and is valid for 10 minutes.
  4. The **refresh token** is stored in the OS keychain (entry `nugget/cloud-session`, using the same `keyring` backends as `nugget-store`). The access token is kept only in memory and refreshed 60 seconds before it expires. Signing out revokes the session (`POST /auth/v1/logout`) and deletes the keychain entry.
- **Cloud API client:** the same crate provides a typed client for `/v1/*`. The Tauri commands are `cloud_sign_in`, `cloud_session`, `cloud_sign_out`, `cloud_connections_list`, `cloud_connect`, `cloud_sync` and `cloud_ask`.
- **UI:** a Sign-in screen gates the app. A Connections page reuses the existing `ui.tsx` components (`PageHeader`, `Badge`, `EmptyState`). The markup stays structural, because DS1 owns the visual redesign.
- **What nuggetd keeps doing locally:**
  - Double Tap, the overlay, quick ask, the global hotkey, a11y capture, computer use and the agent loop all stay local, because they need OS access.
  - Their *retrieval and LLM calls* switch to the cloud: `nugget-infer` gets a `cloud` provider that calls the gateway with the user's JWT, and retrieval calls `/v1/search`.
  - Screen and a11y text is sent to the cloud only as part of a query the user makes, and is never indexed.
  - The local store becomes a cache (settings, Double Tap history, the action log). It is no longer the corpus.

## 5. Reuse and retire

| Crate | Fate |
|---|---|
| `nugget-retrieve` | **Reused in the cloud:** `rank::convex_fuse`, `group_by_item` and MMR fuse Postgres full-text and pgvector hits; `pack` builds the context. |
| `nugget-knowledge`, `nugget-graph` | **Reused in M2:** extraction and entity resolution, with a Postgres-backed store. |
| `nugget-proactive` | **Reused in M3:** generators, scoring and judging, writing to `cards`. |
| `nugget-draft`, `nugget-voice` | **Reused in M3:** drafting server-side, with grounding on cloud retrieval. |
| `nugget-policy` | **Reused** on both sides: assessing and confirming Composio actions, plus the hash-chained log. |
| `nugget-a11y`, `-hotkey`, `-computer`, `-agent`, `-browser` | **Kept on the desktop.** |
| `nugget-infer` | Kept. Gains a `cloud` provider, and local runtimes become an opt-in "offline mode" rather than the default. |
| `nugget-connector-*` (google, imap, caldav, microsoft, saas, messaging), `nugget-oauth` | **Retired** after M2, replaced by Composio. `nugget-connector-files` stays for local files (M4: upload chunks). |
| `nugget-runtime`, `nugget-models` (local model download and serving) | **Retired as the default.** They stay behind the offline-mode flag until usage data says to delete them. |
| `nugget-store` (SQLCipher) | Narrowed to a local cache and settings. The corpus moves to Postgres. |
| `nugget-mcp`, `nugget-plugin` | Kept. MCP tools are backed by cloud search in M4. |

## Security
- **Isolation:** RLS on every table, with the `nugget_service` role subject to RLS too (see above). `supabase/tests/rls.sql` checks that user A cannot read, write or delete user B's rows, both as `authenticated` and as `nugget_service`.
- **Service environment:** `DATABASE_URL` (the `nugget_service` role through the pooler), `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_ROLE_KEY`, `COMPOSIO_API_KEY`, `COMPOSIO_AUTH_CONFIGS`, `ANTHROPIC_API_KEY` and/or `OPENAI_API_KEY`. Optional: `COMPOSIO_WEBHOOK_SECRET`, `DATABASE_CA_CERT`, `NUGGET_MODEL_FAST`, `NUGGET_MODEL_SMART`.
- **Secrets** live only in the cloud environment's settings (Fly secrets for the service, and Composio's and Supabase's dashboards). They are never committed and never sent to clients. The desktop ships only `SUPABASE_URL`, `SUPABASE_ANON_KEY` (public by design and protected by RLS) and the API base URL. `SUPABASE_SERVICE_ROLE_KEY` is used only by the service, for the admin user deletion call.
- **Tokens at rest:**
  - Google OAuth tokens are held by Composio, encrypted by them. We never see them.
  - Supabase refresh tokens on the desktop are kept in the OS keychain.
  - Supabase Postgres disks are encrypted with AES-256 at rest. Encrypting item bodies in the application (with per-user keys from a KMS) is deliberately not done in M1, because it would block full-text search inside Postgres. It is revisited in M4.
- **Egress to LLM providers. In plain terms:** the text of retrieved emails and events, and the user's question, are sent to Anthropic (or OpenAI) for every Ask and every extraction. Both providers' API terms say they do not train on API data by default, and both retain it for a limited period for abuse monitoring. The privacy policy and the onboarding screen must say this. A setting to opt out of extraction is planned for M2.
- **Webhooks:** Composio webhooks are verified with HMAC-SHA256 over `id.timestamp.body` (the Standard Webhooks scheme), and a timestamp more than 5 minutes off is rejected.
- **GDPR (UK GDPR / DPA 2018):**
  - **Export:** `GET /v1/export` returns all of the user's rows as JSON. The desktop's "Download my data" saves it.
  - **Deletion:** `DELETE /v1/me` deletes the Composio connected accounts first; if that fails, nothing is deleted and the user can retry. It then deletes the auth user through the admin API, which cascades to every table. M1 stores nothing in Storage. From M4, exports in Storage must be deleted explicitly, because Storage objects do not cascade.
  - Retention: items are kept until the user disconnects or deletes their account. Disconnecting a toolkit deletes its items.
  - Data processors to list in the privacy policy and DPAs: Supabase, Fly.io, Composio, Anthropic and OpenAI.
  - The Supabase region should be eu-west-2 (London) and the Fly region `lhr`.

## Migration plan
- **M1: cloud Ask over mail and calendar.** This is "sign in, connect Gmail and Google Calendar via Composio, ask a question and get a cited answer from cloud-indexed mail". It includes:
  - The schema and RLS, the auth and cloud clients, Sign-in, the Connections page, Gmail and Calendar sync, and `/v1/ask` with citations, metering and quotas.
  - Exit: one real user completes the flow end to end on macOS.
- **M2: knowledge and search parity.**
  - Extraction into `entities` and `claims`, and hybrid search (`/v1/search`) for the Search page.
  - nuggetd retrieval for Double Tap and quick ask switches to the cloud, and the `cloud` provider is added to `nugget-infer`.
  - The local connectors are retired, and export and deletion are added to the UI.
- **M3: proactive cards and actions.**
  - Cards generated in the cloud, and server-side drafts.
  - Composio actions (send mail, create event) behind `pending_actions` and the desktop confirmation UI.
  - More toolkits: Slack, Notion and Drive.
- **M4: hardening and scale.**
  - Billing (Stripe) tied to `profiles.plan`, and triggers for low-volume toolkits.
  - Sign in with Apple. This needs an Apple Developer account, a Services ID and a key, and the provider enabled in Supabase.
  - Local files uploaded as chunks, MCP backed by the cloud, and application-level encryption review.
  - An offline-mode decision, and load tests.
