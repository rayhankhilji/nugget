# Heed Cloud architecture

Status: design plus M1 build in progress, on branch `ar/cloud`. Date: 2026-11-02.

Heed moves from local-first to **cloud-first SaaS**:
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
 Google) → heed://auth/callback             │     Composio     Anthropic /
                                            ▼     (OAuth,      OpenAI
                                   Supabase Postgres  tools,    (LLM, embeddings)
                                   + pgvector + Storage triggers)
```

### Supabase (Pro plan)
- **Auth:** email magic link and Google sign-in in M1. Apple sign-in waits until M4, because it needs an Apple Developer account ($99/yr). The desktop app uses the PKCE flow through the system browser, and the redirect URL is `heed://auth/callback`. In the Supabase dashboard, add it under Auth → URL Configuration → Redirect URLs (the app still accepts `nugget://auth/callback` from before the rename to Heed, but new setups only need `heed://auth/callback`). Supabase issues the JWTs. The cloud service checks each token by calling `GET /auth/v1/user`, and caches the result for 60 seconds per token hash. Because of this, the service works whichever JWT signing scheme the project uses (legacy HS256 or asymmetric keys).
- **Postgres + pgvector.** The schema is in `supabase/migrations/`. Every table has a `user_id` column and Row-Level Security enabled:
  - `profiles`: created by a trigger on `auth.users`. It records when the user was last active.
  - `billing_customers`, `subscriptions` and `devices` are described in [Plans and billing](#6-plans-and-billing).
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
1. **Sync loop.** Every minute it claims due connections with a lease (`nugget_private.claim_due_connections`). The cadence comes from the plan (see section 6) while the user is active, and is once a day otherwise. A user can also start a sync with `POST /v1/sync`. That waits up to 20 s, then answers `202 {running: true}` while the import continues in the background.
   - Each connection has a run lease (`syncing_until`), so a request and the scheduler never sync the same connection at once.
   - Progress (`syncing`, `items_synced`, `backfill_done`) is in `GET /v1/connections`, which the desktop polls while an import runs.
   - A long Gmail backfill resumes where it stopped, one minute later.
2. **Fetch.** It calls Composio tools as that user: `GMAIL_FETCH_EMAILS` with an `after:` query built from the cursor, and `GOOGLECALENDAR_EVENTS_LIST` with `timeMin`/`timeMax` and `updatedMin`.
3. **Normalise.** Each email or event becomes an `items` row. HTML is stripped, quoted replies are trimmed, and the body is capped at 64 KB. If the content hash has not changed, the item is skipped, so re-syncs cost nothing.
4. **Chunk and embed.** Text is split into chunks of about 1,200 characters with overlap. Chunks get embeddings only if an embedder is configured (see the AI gateway). Full-text search always works.
   - Unchanged items are skipped with one hash lookup per page.
   - All new chunks on a page are embedded together, at most 100 inputs per provider call, not one call per item.
5. **Extract** (M2). `nugget-knowledge` extraction runs on new items, using the gateway's cheap model, and writes to `entities` and `claims`. See [Intelligence](#intelligence-m2m3).

### Intelligence (M2/M3)
Knowledge, cards, the brief and search reuse the local crates as libraries rather than porting them. Each job loads one user's recent items (180 days, at most 4,000, plus future events) into an **in-memory `nugget_store::Store` snapshot** (`nugget-cloud/src/snapshot.rs`), with the metadata shape the local pipeline writes:
- Mail gets `mime: message/rfc822`, `extracted` headers (from, to, cc, subject, Message-ID, In-Reply-To, References, List-Unsubscribe, Precedence, Auto-Submitted) and the Gmail `labels` and `thread_id`. Calendar events get the local Google connector's `source` shape (start/end objects, attendees and organizer with emails and responses). Ingest stores these fields since M2; older event rows are upgraded on the fly.
- **Privacy labels** come from `nugget_ingest::rule_labels`: the PII scan, connector flags and the sensitivity rules, as the local pipeline applies them with no local triage model (`assume_unflagged_safe`). A rule hit makes the item *confirmed* sensitive; everything else is not sensitive. The local model-based triage (junk and sensitive labels from a local LLM) does **not** run in the cloud.
- Item keys are the Postgres item ids, so every piece of evidence maps back to an `items` row.

The jobs (`intel.rs`), per user:
1. **Knowledge.** `nugget-knowledge` runs on the snapshot: deterministic extractors, people and project resolution, and the quote-gated LLM extractor. The model is the gateway's fast model through `llm_bridge.rs`, a `nugget_infer::Provider` that is metered in `usage_events`, held to the quota, and cached in `llm_cache` by prompt hash, so an unchanged item is never paid for twice. Each run sends at most 40 uncached requests; the rest wait for later runs. Confirmed-sensitive items never reach the model (the crate's own gate). When the month's quota is spent, the run is deterministic only. The graph is projected into `entities` (stable ids, keyed by the "me" entity, then the strongest identifier, then kind and name) and `claims` (active claims with every evidence item and quote).
2. **Cards.** `nugget-proactive` runs on the same snapshot and graph: unanswered mail, commitments and follow-ups, meeting prep, renewals; its scoring model and the learned per-sender, per-domain and per-kind feedback with decay; the multilingual text rules and `LocalText` keys. The state lives in `proactive_state` as JSON, saved with a compare-and-swap on `updated_at` (a refresh never loses a dismissal made while it ran), and every save projects the cards into `cards`. Cards go to the desktop as the daemon's `CardView` (`nugget_proactive::view`), with quotes withheld on sensitive cards.
3. **Brief.** From 07:00 in the user's time zone (`profiles.timezone`, sent by the desktop as `tz`), `nugget-proactive`'s morning brief plus two to five sentences of prose with `[n]` citations from the fast model. Sensitive cards stay in the structured brief but never go to the model. It is generated once per local day and kept in `briefs`; without a model or quota the brief has no prose.

**Hardening** (SEC1):
- Vector queries widen the HNSW scan per transaction (`hnsw.ef_search`, and `hnsw.iterative_scan` on pgvector ≥ 0.8; threat model finding 31).
- `cards.item_ids` holds only ids of the user's own items (checked on every save).
- Every prompt built here (extraction, the brief, reply drafts) passes item text through `ask::neutralise_tags`.
- `/v1/export` includes `briefs`, `proactive_state`, `intel_runs` and `llm_cache`.
- Card refreshes share the ask rate limit, and card-accept proposals share the actions limit.

**Plans.** Every endpoint below needs a live subscription (`plans::entitlement`; 402 `subscription_required` otherwise), cards need the plan's `needs_you_inbox` and the brief its `daily_brief` (403 `not_in_plan`). Model calls go through the plan's `Allowance` (`Gateway::choose`, fast tier): past the hard cap, extraction and the brief's prose are skipped and search drops to full text. The snapshot also honours the plan's `history_days`.

**Scheduling.** Every 5 minutes, `nugget_private.intel_due` lists subscribed users whose items changed since their last run, or whose last run is more than 6 hours old (cards depend on the clock), and `nugget_private.brief_due` lists subscribed users past 07:00 local with no brief for today. Both are `security definer` and callable only by `nugget_service`.

**Endpoints** (each in its own module under `api/`):
- `POST /v1/search {q, sources?, person?, from?, to?, limit?, tz?}`: full text (`tsvector`) and pgvector hits fused with `nugget_retrieve::rank::convex_fuse`, grouped per item, normalised, recency-boosted and diversified with MMR. A date phrase in `q` ("last week") becomes the range when `from`/`to` are unset. `facets` (sources, people) and `timeline` (by day for ranges up to 62 days, else by month) count the items that match the words and filters; vector-only matches rank results but are not counted. With no words it browses newest first.
- `GET /v1/cards?tz=`, `POST /v1/cards/refresh`, `POST /v1/cards/seen {ids}`, `POST /v1/cards/{id}/accept|done`, `POST /v1/cards/{id}/dismiss {reason, note?}` (`not_relevant`, `already_handled`, `wrong_info`, `not_important`, `other`), `POST /v1/cards/{id}/snooze {until}`.
- `GET /v1/brief?tz=`: today's brief (generating it if due), else the latest earlier one, else null.

The desktop shell exposes them as `cloud_search`, `cloud_cards`, `cloud_cards_refresh`, `cloud_cards_seen`, `cloud_card_action` and `cloud_brief`; `src/cloud.ts` has typed wrappers.

**Card actions** (`card_actions.rs`): a card's `draft_reply` maps to a `reply_mail` in the app the message came from, and its `create_event` to a calendar `create_event`. An action is `supported` only when CL4's actions have a tool for it **and** the user has that app connected. `POST /v1/cards/{id}/accept {action?, body?}` proposes it through the same path as `POST /v1/actions` (validated, assessed by `nugget-policy`, previewed, idempotent per card and action) and returns `{card, action}`. A reply's text is the user's (`body`), or is drafted by the fast model from the card's evidence within the plan's allowance. Sensitive cards are never drafted (422 `body_required`). Nothing is sent until `POST /v1/actions/{id}/approve` with the preview's digest.

Not done yet: the local LLM judge that enriches cards is not used; snapshots are rebuilt on every run, which is fine at M2 volumes but should become incremental before large mailboxes.

## 2. Composio
- **Identity.** The Composio `user_id` is the Supabase user UUID. One Composio project serves all users, and the API key lives only in the server environment (`COMPOSIO_API_KEY`). The REST base URL is `https://backend.composio.dev/api/v3.1`, with the header `x-api-key`. `v3` is frozen but still supported.
- **Auth configs.** These are created once per toolkit in the Composio dashboard, using Composio-managed OAuth for Gmail and Google Calendar. Their ids go in `COMPOSIO_AUTH_CONFIGS`, for example `gmail=ac_…,googlecalendar=ac_…`.
- **Connecting an account:** `POST /v1/connections {toolkit}` → the service calls Composio to create a connected-account link with `callback_url = heed://connections/callback` (`NUGGET_CONNECT_CALLBACK` overrides it; the app also accepts the pre-rename `nugget://connections/callback`) → the desktop opens `redirect_url` in the system browser → after consent, Composio redirects to the deep link → the desktop calls `GET /v1/connections`. That call refreshes statuses from Composio and records each ACTIVE account in `connections`, then the first sync runs.
- **Triggers or polling.** Composio's Gmail and Calendar triggers are themselves polling-based, with a 2–15 minute floor. They cost **$0.003 per event** on the paid tier, ten times the price of a tool call ($0.0003). A user receiving 1,000 emails a month would cost $3 in trigger events alone. **M1 therefore polls with tool calls**, adaptively as described above, which costs about 1,500–3,000 calls per active user per month. `POST /v1/webhooks/composio` is implemented: it verifies the signature, and for an event it only marks that connection due (it does not ingest the payload). This lets us switch triggers on per toolkit later, for example for low-volume apps, without a second ingest path.
- **Actions** are described under [Actions](#actions) below.
- **Pricing** (composio.dev/pricing, for signups after 2026-08-15):
  - Hobby: $0, with 100k tool calls and 50k trigger events a month. Usage *pauses* at the cap.
  - Pro: $29/month, with the same allowance as a credit, then $0.0003 per call and $0.003 per trigger event.
  - **The decision: start on Hobby** (confirmed by the founder). With paid plans' sync cadence it covers development and roughly the first 10–15 subscribers. Move to Pro before the cap, because a pause stops ingest for everyone.

## 2a. Apps

`GET /v1/apps` is the Connections catalogue. Each entry has the slug, name, logo, `logo_url`, categories, `syncs`, the action kinds, `sync_mode`, `connectable` (this deployment has a Composio auth config for it) and `connected`.

The desktop CSP is `img-src 'self' data:`, so the desktop cannot load remote images. `logo` is therefore always a `data:` URI:
- The service fetches it from Composio's `GET /toolkits` (`meta.logo`). When that call fails, it uses `https://logos.composio.dev/api/<slug>`, which serves every slug but is not documented.
- The fetch is SSRF-guarded:
  - https only, with no proxy;
  - at most 3 redirects, each re-checked;
  - a DNS resolver that refuses loopback, private, link-local and metadata (169.254.169.254, `fd00:ec2::254`), CGNAT and other non-public addresses, so the check applies to the address actually connected to;
  - IP-literal hosts are checked the same way.
- It is capped at 32 KB and 5 seconds, and cached (24 hours, or 1 hour after a failure).
- Rasters are accepted only when their magic bytes match.
- SVG is rebuilt from an allowlist: no scripts, styles, `foreignObject`, images, event handlers, DTDs, or references outside the document.
- Anything else gets a generated monogram.

`logo_url` keeps the remote URL, for the website. `/v1/connections` serves its `available[].logo` the same way.

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
| `PATCH /v1/actions/{id}` | `{input}`: edit the editable fields → new preview and a NEW digest |
| `POST /v1/actions/{id}/snooze` | `{until}`: hide it until then (RFC 3339; null shows it again) |

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
- Proposing and approving need a live subscription (`402`), and share a per-user rate limit (`NUGGET_RATE_ACTION_PER_MIN`, default 10, `429 rate_limited`). Listing, status, preview and cancel stay open.
- Approval is scoped to the owner: another user gets `404`, even with the digest. Only the stored call runs; fields in the approval body other than `digest` are ignored. A stored call that no longer matches its digest is refused (`412`) and audited.
- Tool slugs come only from a fixed table (`actions::tool_for`, and each adapter's declared read tools). No tool name or URL is ever taken from synced content, model output or a request.
- Same-user foreign keys (`20261102120002`): an action can only point at its own user's connection, and an audit entry only at its own user's action.

**Audit.** Every step is appended to `action_audit`, a per-user SHA-256 hash chain (`prev_hash`, `hash`) without message bodies:
- `proposed`, `denied`, `approval_rejected`, `approved`, `succeeded`, `failed`, `cancelled`, `expired`.

Neither the service role nor the user can update or delete the chain. The GDPR export includes both tables.

**Clients.** `nugget-cloud-client` has typed methods for these routes, and the desktop has the Tauri commands `cloud_apps`, `cloud_actions_list`, `cloud_action_propose`, `cloud_action_preview`, `cloud_action_status`, `cloud_action_approve` and `cloud_action_cancel`, wrapped thinly in `cloud.ts`. The confirmation UI belongs to the design work.

**Editing.** `PATCH /v1/actions/{id}` takes `{"input": {...}}` with only the fields an action of that kind may change:

| Kind | Editable |
|---|---|
| send_mail | to, cc, bcc, subject, body |
| reply_mail | to, body |
| create_event, update_event | title, start, end, timezone, attendees, location, description |
| post_message | text |
| create_page | title, markdown |

Where an action goes (thread, event, channel, parent page) is fixed at proposal. An edit:
- is merged into the stored input (`null` removes an optional field);
- is re-validated and re-planned like a proposal, and the event diff is re-run;
- is re-assessed by `nugget-policy`. An edit the policy denies leaves the action `denied` and unsendable;
- gets a new preview and a new digest, so an approval of the old preview is refused (`412 digest_mismatch`);
- bumps `revision`.

Like proposing, editing needs a live subscription and counts against the action rate limit. It is audited as `edited`, with the changed field names and the old and new digests, but no bodies. Retrying the original proposal with its idempotency key returns the edited action rather than a conflict (`proposed_digest`). Actions proposed before editing existed have no stored `input` and `editable: []`; they can be approved, cancelled or proposed again, not edited.

**Snoozing.** `POST /v1/actions/{id}/snooze {"until"}` hides a pending action from `GET /v1/actions` until then, unless `?include=snoozed` is passed. Status, preview and approve still work on a snoozed action. `until` is **capped at the action's expiry**: snoozing never extends an action's life, so an approval is never more than 24 hours from its preview. Snoozing again with `null` shows it at once, and a due snooze clears itself. Snooze and un-snooze are audited. Migration `20261102140001_action_edits.sql` adds `input`, `revision`, `proposed_digest` and `snoozed_until` (checked `<= expires_at`) to `pending_actions`; RLS and the same-user keys are unchanged.

**Citation authors.** Citations (`/v1/ask`, the brief), search results and card evidence carry `author`: a display name only (`crate::authors::display_name`). It is the name part of a `From` header (`Ann Lee <ann@x.com>` → `Ann Lee`), a Drive owner's or a Teams display name, or a Slack profile name. It is null for a bare address, a platform id (a Slack `U…` id, a UUID) or a sensitive card, so an email address is never returned. Invisible and control characters are dropped, angle brackets are neutralised, and the name is cut at 80 characters. Citations keep `snippet`.

## 3. AI gateway
All model calls go through `nugget-cloud`'s gateway module. No provider key is ever sent to a client.
- **Interface:** `Llm::complete(user, tier, messages) -> (text, usage)` and `Embedder::embed(texts)`.
  - Anthropic Messages is the default, when `ANTHROPIC_API_KEY` is set. OpenAI Chat is the fallback, when only `OPENAI_API_KEY` is set.
  - Embeddings use OpenAI `text-embedding-3-small`, 1536 dimensions, at $0.02 per million tokens. Embeddings are on only when `OPENAI_API_KEY` is set. With only an Anthropic key, retrieval is full-text only, which is still useful, and the doc and the `/v1/me` endpoint say so.
- **Metering:** every call writes a row to `usage_events`, with tokens and cost taken from a price table in code.
- **Model choice:** before each Ask, `Gateway::choose` picks the model from the user's plan. The rules are in [Plans and billing](#6-plans-and-billing):
  - Ask uses the plan's default model, or the one the request names.
  - A smart request falls back to the fast model once the smart allowance or the fair-use budget is used up, and the response says `downgraded: true`.
  - Past the hard cap, Ask returns `429 quota_exceeded`.
  - Tool-call costs are ours to absorb and do not count against the cap.
- **Models.** Prices per million tokens are from the official Anthropic page, 2026-11. Model ids are configuration (`NUGGET_MODEL_FAST`, `NUGGET_MODEL_SMART`), not code.
  - The fast model is Claude Haiku 5.5, at $0.10 in and $0.50 out. One Ask, at 7.8k tokens in and 520 out (the 4.7+ tokenizer's +30% included), costs $0.00104.
  - The smart model is Claude Sonnet 5.5, at $2 in and $10 out. The same Ask costs $0.0208.
  - Prompts over 100k tokens are billed at 5× on Haiku 5.5. The gateway packs at most 24k tokens of context, so that rate is never reached.

## 4. Desktop app changes
- **Sign-in:**
  1. A new crate, `crates/nugget-cloud-client`, builds the PKCE `authorize` URL (for Google; Apple is added in M4) or sends the magic-link request (`POST /auth/v1/otp` with a code challenge). The system browser opens it.
  2. Supabase redirects to `heed://auth/callback?code=…`.
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
- **Service environment:** `DATABASE_URL` (the `nugget_service` role through the pooler), `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_ROLE_KEY`, `COMPOSIO_API_KEY`, `COMPOSIO_AUTH_CONFIGS`, `ANTHROPIC_API_KEY` and/or `OPENAI_API_KEY`, `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, `STRIPE_PRICE_HEED_MONTHLY`, `STRIPE_PRICE_HEED_ANNUAL`, `STRIPE_PRICE_HEED_MAX_MONTHLY`, `STRIPE_PRICE_HEED_MAX_ANNUAL` and `NUGGET_PUBLIC_URL`. Optional: `COMPOSIO_WEBHOOK_SECRET`, `DATABASE_CA_CERT`, `NUGGET_MODEL_FAST`, `NUGGET_MODEL_SMART`, `NUGGET_PLANS_FILE`, `NUGGET_APP_SCHEME` (`heed` after the rename) and `NUGGET_SITE_ORIGINS`. That last one lists the website origins, comma-separated, for CORS and billing return URLs; when it is unset, CORS is off.
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

## 6. Plans and billing
**No free plan.** The service grants an entitlement only while a subscription is `active`, `trialing` or `past_due`. `past_due` keeps access while Stripe retries the payment. `canceled`, `unpaid` and `incomplete` give none. Without an entitlement, a signed-in user gets `402 subscription_required` from connect, sync and Ask, and sync stops.

**Plans live in config.** `crates/nugget-cloud/plans.toml` is embedded at build time, and `NUGGET_PLANS_FILE` can point at an edited copy. The founder changes prices, limits and features there without touching code. The file is validated at startup, so unknown keys, impossible caps and duplicate Stripe prices are refused. Stripe price ids come from the environment.

| | **Heed**, £39/mo or £390/yr | **Heed Max**, £99/mo or £990/yr |
|---|---|---|
| Answer model | fast (Haiku 5.5), plus 300 smart asks/mo | smart (Sonnet 5.5) by default, 900 smart asks/mo |
| Fair-use AI budget, then hard cap | $8, then $12 | $18, then $25 |
| Connected apps | 10 | unlimited |
| Sync while active | every 30 min | every 5 min for mail and calendar, 15 min for other apps |
| History | 12 months, pruned on sync | all, with a 3-year first backfill |
| Devices | 1 | 3 |
| Ghost text, Needs-you inbox, daily brief, actions with approval | yes | yes |
| Multi-step agent tasks, computer use, priority support | no | yes |

- **Trial and annual:** both plans have a 14-day trial with a card required (Checkout uses `payment_method_collection=always`), one trial per user. Annual is 10× monthly.
- **Idle users:** someone who has not used Heed for 12 hours syncs once a day, whatever their plan.
- **Feature flags:** these are returned in `/v1/me` as `entitlements`. The server enforces models, quotas, connections, devices, sync and history now. Agent tasks, computer use and the inbox are gated on the desktop until they move server-side in M2 and M3.

**Stripe.**
- `POST /v1/billing/checkout {plan, interval}`:
  - creates the customer once, with an idempotency key, and records it in `billing_customers`;
  - opens a subscription Checkout with the trial;
  - returns `{url}`. The desktop opens it in the browser.
- `POST /v1/billing/portal` returns the customer-portal URL, for upgrades, downgrades, cancellation, invoices and cards.
- `GET /v1/billing/preview?plan=&interval=` previews moving the live subscription to that plan and interval.
  - Stripe's `POST /v1/invoices/create_preview` replaces the removed `invoices/upcoming`. It is called with the subscription's item, the new price, `proration_behavior=create_prorations` (Stripe's and the portal's default) and `proration_date` = now.
  - The response is `{plan, interval, amount_due (minor units), currency, proration_date (RFC 3339)}`.
  - It is read only: nothing changes in Stripe.
  - Errors: `409 no_subscription`, `400 unknown_plan` or `bad_interval`, and `404 billing_disabled` when `NUGGET_BILLING=off`, like the other billing routes.
- Both send the browser back to `GET /billing/done`, which hands over to `nugget://billing/done`. The website can instead pass `return_url`, which must be on an origin in `NUGGET_SITE_ORIGINS`; anything else gets `400 bad_return_url`, so the endpoints cannot be used as an open redirect. The API sends CORS headers for those origins only, and allows no credentials, since auth is a bearer token.
- `POST /v1/webhooks/stripe`:
  - Verification: `Stripe-Signature` must be HMAC-SHA256 over `t.body`, within 5 minutes.
  - Events handled: `checkout.session.completed`, `customer.subscription.created`, `.updated` and `.deleted`, and `invoice.payment_failed` and `invoice.paid`.
  - Every handled event makes the service **fetch the subscription from Stripe** and store it, so duplicates and out-of-order delivery converge on Stripe's current state.
  - Event ids are recorded in `nugget_private.stripe_events`, and a repeat is acknowledged without work.
  - A failure leaves the event unrecorded, and Stripe retries. Causes include an unknown price id, or a customer we cannot tie to a user through our own metadata.
- `subscriptions` and `billing_customers` are readable by their owner under RLS and written only by the service.

**Devices.** The desktop sends a random `X-Heed-Device` id. A new device beyond the plan's limit gets `403 device_limit_reached`. Requests without the header, such as from the website or API tools, never take a seat. Only devices seen in the last 30 days count, and a user can delete their own device rows. The ids are client-generated, so this is a fair-use limit, not DRM.

**Cost and margin per user per month.** Assumptions:
- The app is active about 10 hours a day. Composio tool calls cost $0.0003.
- Ghost text is 1.5k tokens in and 40 out on Haiku. The daily brief is 20k in and 800 out. One agent task is 10 Sonnet steps of 15k in and 500 out, about $0.35.
- Extraction and embedding cost $0.0005 per item.
- Net revenue is the price less 20% VAT, less Stripe's 1.5% + 20p.
- Light user: 1,000 items, 3 apps, 5 asks a day, 50 ghost completions a day, and on Max 8 agent tasks a month.
- Heavy user: 5,000 items, 10 apps (15 on Max), 30 asks a day, 300 ghost completions a day, and on Max 60 agent tasks a month.

| User | AI (model) | Composio calls | Cost (£) | Net revenue | Gross margin |
|---|---|---|---|---|---|
| Heed light | $1.61 | 1,820 ($0.55) | £1.63 | £31.71 | 95% |
| Heed heavy | $10.97 | 6,100 ($1.83) | £9.69 | £31.71 | 69% |
| Max light | $6.75 | 8,420 ($2.53) | £7.02 | £80.81 | 91% |
| Max heavy | $43.82 uncapped, so it hits the $25 cap | 22,900 ($6.87) | £24.14 | £80.81 | **70%** |

The hard caps bound the worst case. That is £10.45 for Heed (67%) and £24.12 for Max (70%), which is how the Max caps were sized. A heavy Max user hitting the cap still gets fast answers until the $25 hard cap.

**Fixed costs and break-even:**
- Supabase Pro is $25 and Fly about $4.
- Composio Pro is $29. It is needed once paying users exist: a heavy Max user alone makes about 23k calls a month, and Hobby stops at 100k.
- That totals about £44 a month, which **two Heed subscriptions cover**. The founder's £99/month infrastructure ceiling holds until about 40–50 active subscribers. From there, Supabase compute and Composio overage grow with users and are paid by revenue.

## 7. Waitlist
`POST /v1/waitlist {email, source?}` takes sign-ups from the website while Heed Cloud is not open.

- **No account, no auth.** The route is limited to `NUGGET_SITE_ORIGINS` by CORS, capped at 1 KiB
  of body, and rate-limited to `NUGGET_RATE_WAITLIST_PER_HOUR` (default 5) per client IP.
- **Nothing to enumerate.** Every well-formed JSON body gets the same empty `202`, whether the
  address is new, already listed or invalid. Invalid addresses are not stored. Only malformed JSON
  gets `400`.
- **Validation.** Addresses are trimmed and lowercased. Each must be at most 254 characters, with
  one `@`, a dot in the domain, and no spaces or control characters.
- **Storage.** Rows go into `public.waitlist (email citext primary key, created_at, source,
  ip_hash)`. `ip_hash` is a SHA-256 of the IP salted with `NUGGET_WAITLIST_SALT`, a per-deploy
  secret. Without a salt, no IP is kept.
- **Privacy.** RLS is on. `anon` and `authenticated` have no access at all, and `nugget_service`
  may only insert.
- **Deletion on request.** Support removes an address with
  `select nugget_private.waitlist_forget('person@example.com')`, as the database owner.
  `DELETE /v1/me` also removes the entry for the account's own address.
- **Self-hosted.** The route works there too, but it is harmless: nothing reads the list.

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
- **M1b: billing (now, before launch).** Paid plans only, set in `plans.toml`. Stripe Checkout with a 14-day trial, the customer portal and webhooks. Plan entitlements gate model choice, quotas, sync cadence, history, connections and devices. Built against test doubles; see section 6.
- **M4: hardening and scale.**
  - Triggers for low-volume toolkits.
  - Sign in with Apple. This needs an Apple Developer account, a Services ID and a key, and the provider enabled in Supabase.
  - Local files uploaded as chunks, MCP backed by the cloud, and application-level encryption review.
  - An offline-mode decision, and load tests.
