# Cloud load test

Honest numbers for **Ask**, **sync scheduling** and the **AI gateway** at **100 concurrent users**.
The test runs the real `nugget-cloud` router, real Postgres 16 + pgvector and the real migrations,
with **fake** upstreams. It is **not production**: read "What these numbers do not tell us" before
quoting any of it.

## Setup

- **Machine:** cloud dev container, 4 vCPU (Intel Xeon), 15 GiB RAM. The load generator, the
  service and Postgres all share these 4 cores.
- **Postgres:** local 16.15, pgvector 0.6.0, trust auth, no TLS, about 0.1 ms round trip. The
  shim and every migration are applied to a fresh database on each run.
- **Service:** `AppState::new` + `router()` served on `127.0.0.1:<random>` with
  `into_make_service_with_connect_info`, `Limits::unlimited()` (except where stated), DB pool 8
  (the default) or 16. The client is reqwest over real HTTP/1.1.
- **Doubles (`examples/loadtest.rs`):**

| Double | Behaviour | Default latency |
|---|---|---|
| Auth | `tok-<uuid>` → that user | 0 |
| Composio | Gmail: 50 messages of ~2 KB per first sync (`LT_MSGS`, `LT_MSG_BYTES`, `LT_GMAIL_PAGES`), then 5 new per sync; Calendar: 30 events, then 5 | 300 ms per call |
| LLM | Answer of ~2 KB, cites `[1]`/`[2]` when sources exist; 7,800 in / 520 out tokens (Haiku 5.5: 1,040 µUSD) | 1,500 ms |
| Embedder | 1536-dim bag-of-words vectors | 100 ms per call |

- **Data:** 100 users (rows in `auth.users`, profiles from the trigger), each with Gmail and
  Calendar connected through `POST /v1/connections` + `GET /v1/connections`, then one
  `POST /v1/sync`: 8,000 items and 13,000 chunks in all. Questions use a 2,000-word synthetic
  vocabulary, so each question matches a few items per user, like real topics.

## How to run

```sh
LT_DATABASE_URL='postgres://postgres@127.0.0.1:54329/postgres?sslmode=disable' \
  cargo run --release -p nugget-cloud --example loadtest
```

The full run takes about 8 minutes and drops its database at the end. Knobs (env, all listed at the
top of the example): `LT_USERS` (100), `LT_DURATION_SECS` (60), `LT_CEIL_SECS` (20), `LT_LLM_MS`,
`LT_EMBED_MS`, `LT_COMPOSIO_MS`, `LT_POOLS` ("8,16"), `LT_EMBED_MODES` ("off,on"), `LT_ONLY_SEED`.

## Results (2026-11-02)

### Seeding: connect + first sync, all 100 users at once

| Embeddings | Connect (2 toolkits) | `/v1/sync` wall | Result | Throughput |
|---|---|---|---|---|
| off | 1.1 s | 8.5 s (p99 8.5 s) | 100 × 200 | 941 items/s, 1,530 chunks/s |
| on, pool 8 | 1.0 s | 30.0 s | **100 × 504** (deadline) | 159–185 items/s, 318–351 chunks/s |
| on, pool 16 | – | 30.0 s | 100 × 504 | 172 items/s, 338 chunks/s |
| on, embedder at 0 ms | – | 30.0 s | 100 × 504 | 184 items/s, 351 chunks/s |
| on, **1 user**, 1 page (80 items) | – | 9.7 s | 200 | 8 items/s |
| on, **1 user**, 5 Gmail pages (280 items) | – | 30.0 s | **504** | 237 items stored, then cut off |

With embeddings, the database is the limit at 100 users, not the embedder: chunk inserts with an
HNSW-indexed `vector(1536)` run about 4.5× slower than without vectors, and neither pool 16 nor
a zero-latency embedder helps. For one user, the limit is the embedder: one embedding call per item,
one after another, so 100 ms × items.

### Scenario A: Ask, 100 virtual users looping for 60 s

Overhead = latency minus the fake model time (LLM 1,500 ms, plus the 100 ms question embedding
when embeddings are on).

| Embeddings | Pool | Req/s | p50 / p95 / p99 / max ms | Overhead p50 / p99 ms | Errors | Pool saturated | DB tx/Ask |
|---|---|---|---|---|---|---|---|
| off | 8 | 63.1 | 1,527 / 1,767 / 1,793 / 1,849 | 27 / 293 | 0 | 9% of samples | 7 |
| off | 16 | 65.2 | 1,512 / 1,626 / 1,656 / 1,680 | 12 / 156 | 0 | 2% | 7 |
| on | 8 | 60.7 | 1,623 / 1,769 / 1,889 / 1,908 | 23 / 289 | 0 | 6% | 9 |
| on | 16 | 60.8 | 1,626 / 1,690 / 1,834 / 1,859 | 26 / 234 | 0 | 3% | 9 |

Ceiling with zero-latency fakes (the server and the database alone, 20 s): without embeddings,
620 req/s with pool 8 and 601 with pool 16 (p99 196 and 231 ms). With embeddings, 393 and 388
req/s (p99 329 and 409 ms). The pool was saturated in 100% of samples, and the CPU was the limit.

98–100% of answers carried citations, so retrieval returned sources. The vector search plan used
`chunks_user` with an exact sort, not HNSW, and returned the full 40 candidates for each of 10
sampled users.

### Scenario B: sync scheduler (`Ingest::run_due(20)` passes back to back)

All 200 connections were marked due. Each sync is incremental: 1 Composio call of 300 ms, 5 new
items, embeddings on.

| Machines | Passes | Mean pass | Drain 200 | Connections/min | Failed |
|---|---|---|---|---|---|
| 1 | 10 | 4.7 s | 47.1 s | 255 | 0 |
| 2 | 10 (5 each) | 4.7 s | 23.8 s | 503 | 0 |

The real loop (`main.rs`) runs **one pass of 20 every 300 s**. A pass takes about 5 s, so the tick
sets the capacity, not the work: **240 syncs/hour, 5,760/day per machine**. Demand from 100
*simultaneously active* users is 2 connections every 30 min, or **400 syncs/hour**. At that level a
single machine falls behind by about 160 syncs/hour, and active users get syncs about every 50 min
instead of every 30. Over a day (8 active hours, plus daily syncs while idle), demand is about 3,400
syncs, under 5,760. Two machines (480/hour) cover the peak.

### Scenario C: AI gateway

| What (zero-latency LLM, pool 8) | p50 / p95 / p99 / max ms |
|---|---|
| Quota check (`month_cost_micros`), 1 caller | 0.8 / 0.9 / 1.0 / 1.7 |
| Usage insert (`record_usage`), 1 caller | 1.2 / 1.9 / 2.3 / 2.3 |
| `Gateway::complete`, 1 caller | 2.0 / 2.8 / 3.0 / 4.1 |
| `Gateway::complete`, 100 callers (3,200 calls/s) | 30.8 / 36.8 / 39.2 / 42.1 |

**Quota race.** A free user is left with 500 µUSD of the 750,000 quota, less than one answer
(1,040 µUSD), then sends 20 concurrent Asks:

| Rate limits | Succeeded | Month cost after | Overshoot |
|---|---|---|---|
| none | 20 / 20 | 770,320 µUSD (102.7%) | 20,320 µUSD, about 20 answers |
| default (10 Asks/min) | 10 / 20 (10 × 429) | 759,910 µUSD (101.3%) | 9,910 µUSD, about 10 answers |

This confirms threat-model item 15: the check is racy, and only the Ask rate limit bounds it. The
bound is about 10 answers per minute per user, or $0.01 per burst on Free.

## Interpretation

**Bottlenecks found**

1. **Inline `/v1/sync` with embeddings runs past the 30 s request deadline.** It failed for all
   100 users at once, and for one idle user with a full 5-page Gmail backfill. When the deadline
   drops the future, `finish_sync` never runs. The cursor and `last_synced_at` are lost, the
   connection stays due, and the client gets a 504 even though some data was stored.
   **Fixed after this run:** `/v1/sync` now runs in a detached task, so the sync finishes and
   records its cursor even when the request times out (test
   `an_abandoned_sync_request_still_records_its_result`). The client still sees the 504; the
   first sync should move to the background with progress reporting (routed to the ingest owner).
2. **Embedding is one provider call per item, one after another** (`Ingest::store`). That costs
   100 ms × items for a single user, and many small requests count against OpenAI rate limits.
3. **Vector inserts are about 4.5× costlier than text-only inserts** in Postgres (HNSW
   maintenance). With 100 concurrent backfills the database is the ceiling (about 350 chunks/s
   here).
4. **The scheduler's fixed tick (20 connections per 300 s)** caps one machine at 240 syncs/hour,
   below the 400/hour that 100 concurrently active users need. `ar/cloud` changes this to
   `run_due(50)` every 60 s (up to 3,000/hour per machine), which fixes it once merged.
5. **Ask is model-bound.** Server overhead is about 25 ms at p50 and 150–300 ms at p99, from 7–9
   short transactions per Ask, each with about 5 round trips. The quota is checked twice per Ask
   (`ask::ask` and `Gateway::complete`). Pool 16 helps only the p99 tail (293 → 156 ms without
   embeddings). At the zero-latency ceiling, pool 16 is no better, because the 4 shared cores are
   the limit.

**What these numbers do not tell us**

- **Network latency to the Supabase pooler.** Here a round trip takes about 0.1 ms. Fly `lhr` to
  Supabase `eu-west-2` is probably 1–3 ms through Supavisor. An Ask makes about 45 round trips, so
  expect roughly +50–130 ms of overhead, and longer pool hold times (pool 8 saturates sooner).
- **Machine size.** Fly `shared-cpu-1x` is 1 shared vCPU with 256 MB to 1 GB of RAM. This test had
  4 dedicated vCPUs and a co-located database. The zero-latency ceilings (390–620 Asks/s) are an
  upper bound and will not hold there. The Postgres side is a Supabase instance with its own CPU
  and memory limits.
- **Real provider behaviour.** The fakes never fail, throttle or vary. The test does not model
  Composio and Gmail rate limits, Anthropic or OpenAI tokens-per-minute limits, 429s or 5xx, or
  latency tails. LLM time is a constant 1.5 s, and real Haiku answers vary with output length.
- **Data volume and history.** About 80–130 chunks per user, one month of usage rows, and
  embeddings from synthetic text. Larger mailboxes, a larger `chunks` table (where the planner may
  switch to HNSW with a post-filter on `user_id`) and real 1536-dim floats are not covered.
- **TLS, the edge proxy, and real auth** (Supabase `GET /auth/v1/user` with a 60 s cache) are not
  in the path.
