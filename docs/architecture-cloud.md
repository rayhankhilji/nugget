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
- **Actions** (M3) are sending mail and creating events. The agent proposes a tool call, and `nugget-policy` assesses it: Send-class actions are always `Confirm`. The proposal is stored as a `pending_actions` row (added in M3) with a preview. The desktop shows the existing confirmation UI, and only `POST /v1/actions/{id}/approve` executes `tools/execute`. Each step is written to the hash-chained action log. Composio never acts without that approval.
- **Pricing** (composio.dev/pricing, for signups after 2026-08-15):
  - Hobby: $0, with 100k tool calls and 50k trigger events a month. Usage *pauses* at the cap.
  - Pro: $29/month, with the same allowance as a credit, then $0.0003 per call and $0.003 per trigger event.
  - **The decision: start on Hobby** (confirmed by the founder). It covers about 30–60 active users. Move to Pro before the cap, because a pause stops ingest for everyone.

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

### Streaming model endpoint for the desktop (`/llm/v1`)
The desktop's AI features (ghost-text autocomplete, Double Tap, quick ask, chat and the agent) run in nuggetd, through `nugget-infer`. When the user is signed in they use Heed's own model endpoint, `crates/nugget-cloud/src/llm`:
- **Shape: Anthropic-Messages-compatible**, `POST /llm/v1/messages` (streaming or not, tools, images, `cache_control`) and `GET /llm/v1/models`. We chose it over an OpenAI-compatible endpoint because Anthropic is the primary provider. The primary path forwards the checked body and relays the provider's bytes unchanged. That means no translation on the latency-critical path, and prompt caching (breakpoints, `max_tokens: 0` prefix warming), tool use and images keep their exact semantics. `nugget-infer` already speaks this API. Only the OpenAI fallback is translated (`llm/openai.rs`), and it is used when Anthropic is unset, or fails or times out before the first byte.
- **Auth:** the user's Supabase access token, as `Authorization: Bearer` (or `x-api-key`, so a stock Anthropic client works with base URL `{api}/llm`). It is checked like every `/v1` route. If our own provider key is refused, the client gets a 502, never a 401, because a 401 would sign the user out.
- **Tiers, not models:** `model` is `heed-fast` (ghost text, triage, light work) or `heed-smart` (drafting, asks, the agent). The service picks the real model from the tier and the user's entitlements (Free: smart is served by the fast model). Entitlements are read through one function, `llm::entitlements()`, which billing replaces.
- **Metering and quotas:** each call writes one `usage_events` row (`kind = 'llm'`, `route` such as `heed-fast:anthropic`, input, output and prompt-cache tokens). The cost charges cache reads at 0.1× and cache writes at 1.25× input. Before sending, the month-to-date cost is checked against the quota (`429`, `code: quota_exceeded`). Entitlements and usage are cached per user for 15 s, and costs metered in that window are added locally. A keystroke's request therefore makes no database round trip, and the quota can be overshot only by requests already in flight (at most `max_concurrent` per user: 4 on Free, 8 on Pro).
- **Limits:** the body is capped at 8 MB (413), with at most 4,000 messages. `max_tokens` is capped per tier and plan. Only known fields are forwarded: server tools, MCP servers, containers, service tiers and betas are dropped or refused. Timeouts are 60 s to the provider's headers, 90 s of silence mid-stream (the stream then ends with an `error` event) and 15 min overall.
- **Cancellation:** a client that disconnects drops the provider request at once, so generation stops. What was relayed is metered: output tokens are estimated from the relayed text when the provider never reported them.
- **Prompt caching:** client breakpoints pass through. A request with none gets one on its system prompt, so the stable prefix (rules, voice) is cached across a user's keystrokes. OpenAI gets a `prompt_cache_key` per user and tier.
- **Logging:** user id, tier, model, route, status, token counts, cost and timings. Never prompt or answer text, and never a provider error body (provider errors are relayed to the client, not logged).

### Desktop routing (nuggetd)
- **Token handoff:** the shell keeps the session (`nugget-cloud-client`: refresh token in the keychain, access token in memory). It hands nuggetd the **short-lived access token over the IPC socket** (`cloud.llm.session`). That socket is owner-only (a 0700 directory and a peer uid check). The token goes over at sign-in, just inside each refresh margin (`next_handoff_in`, at most every 5 min), and on every daemon (re)connect. Sign-out withdraws it. nuggetd keeps it in memory only: never on disk, in config, logs or the audit log. It refuses tokens that are expired or valid for more than 24 h, and `http` URLs off this machine. We chose IPC over a config file because a file would put a bearer token on disk (and in backups) for its whole lifetime, and an hour-long token gains nothing from surviving a restart.
- **Routing:** `nugget_infer::cloud` builds `heed-cloud-fast` (tiers Triage and Light) and `heed-cloud-smart` (Synthesis and Agentic) on **one shared connection pool**. The token is read at each request, so a refresh never drops the warm connection. Both are routed **first** on their tiers, and local models stay behind them as the fallback. A provider error, the quota answer (429), a refused token (401, which also drops the token) or an expired token falls through to the local model at once (no client-side retries). When signed out, the providers leave the router. Ghost text (`draft.begin` with `mode: continue`) routes by `[draft] continue_tier` (default `light`, so the fast tier). Replies route by `[draft] tier`.
- **Egress and sensitivity, unchanged in meaning:** the cloud providers are `Locality::Cloud`. So the egress ledger lists every request under the Heed host. Every rule that decided what may go to a cloud model now decides what may go to Heed cloud:
  - passages marked sensitive are withheld, and the draft engine refuses outright if one slips through;
  - the field and thread text the PII scan flags is withheld;
  - screenshots are sent only with `allow_cloud_screenshots`;
  - read-ahead sends no content to the cloud (it only opens the connection);
  - triage of local items and embeddings stay on local models.
- **Safety checks on cloud output:** the draft engine's per-sentence checks (grounding, injected links and contact details, credentials) run on the cloud stream exactly as on a local one.

### Ghost-text latency
Measured end to end in `llm::tests::desktop_provider_end_to_end_time_to_first_token`. The test drives the desktop's real provider against the real endpoint over HTTP, with a provider double that answers its first token at once. Cold time to first token is about 15 ms, which includes a new connection and the uncached auth and usage reads. Warm requests run at typing cadence, after a prewarm on the connection that prewarm opened: p50 about 4 ms, p95 about 20 ms, and no new connection. That is the endpoint's own overhead plus the client's. Two fixes came out of measuring:
- `TCP_NODELAY` on the service listener. Without it, Nagle held the first token for about 40 ms.
- The streaming client reads a finished response to its end. Before this, every request closed its connection and the next keystroke paid for a new one.

What keeps the warm path short:
- HTTP/2 keep-alive pooling on both hops: desktop→Fly, and Fly→Anthropic (`/llm/v1/models` also warms the service's provider connection when it has been idle).
- A connection prewarm when a supported field gets focus (`draft.read_ahead` → bodiless `GET /llm/v1/models`, at most every 20 s) and on the trigger (`draft.prewarm`).
- Cached auth and entitlements.
- A minimal prompt for `continue`: the small prompt caps and at most 3 retrieved passages, as for local models, and an output of a few words.
- A cached system-prompt prefix.

**Expected real-world time to first token, warm** (estimate, not measured against the live provider): network round trip to Fly `lhr` (about 10–20 ms from the UK, about 80 ms from the US east coast), plus the endpoint (a few ms), plus Anthropic's time to first token for the fast model on a short, prefix-cached prompt (typically 250–500 ms). That is **about 0.3–0.5 s in the UK and 0.4–0.6 s from the US**. A cold start adds TCP and TLS (2–3 round trips) and an auth check against Supabase (about 20–50 ms), so about 0.1–0.2 s more. A fallback to OpenAI adds the failed attempt's time. If the cloud fails, the local model answers within the same request.

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
  - Their *retrieval and LLM calls* switch to the cloud: `nugget-infer`'s `cloud` providers call `/llm/v1/messages` with the user's JWT (see "Desktop routing" above), and retrieval calls `/v1/search`.
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
- **Egress to LLM providers. In plain terms:** the text of retrieved emails and events, and the user's question, are sent to Anthropic (or OpenAI) for every Ask and every extraction. When the user is signed in, the desktop's AI features also send their prompts to Heed cloud, and from there to Anthropic (or OpenAI). That covers the text around the caret for ghost text, the field and thread for Double Tap, the question for quick ask and chat, and the agent's context. Only what the existing cloud rules allow is sent: passages marked sensitive are withheld, text the PII scan flags is withheld, screenshots go only with `allow_cloud_screenshots`, and local triage and embeddings stay local. Each request is in the desktop's egress ledger under the Heed host. Prompts are never logged by the service. Both providers' API terms say they do not train on API data by default, and both retain it for a limited period for abuse monitoring. The privacy policy and the onboarding screen must say this. A setting to opt out of extraction is planned for M2.
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
