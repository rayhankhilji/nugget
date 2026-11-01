# Data flow

## From a change to a search result

```text
connector ──Change──▶ bounded queue ──▶ worker (in order, per source)
 (sync / watch)          1. extract  (size caps, timeout, panic guard)
                         2. PII scan (deterministic: cards, SSNs, IBANs, secrets → sensitive)
                         3. chunk    (structure-aware)
                         4. embed    (embedding tier, batched; local unless allowed)
                         5. store    (item + chunks + version in one transaction)
            checkpoint(cursor) ──▶ saved only after every change before it is stored
            background: local triage labels junk or sensitive after indexing; never removes from search
```

Items carry a **version** (mtime and size, ETag, mod-sequence), so unchanged items are skipped. The
chunker splits by structure (target 400 tokens, at most 512, overlap 48). Items stored without a vector
are embedded later. A sync with no cursor **reconciles**: items gone from the source are deleted, which
catches deletes made while Nugget was off. Measured: a file change is searchable in 0.6 s at p95, half
of it the watcher debounce ([Benchmarks](benchmarks.md)).

## From a question to an answer

```text
query ─▶ (follow-up? rewrite locally) ─▶ embed ─▶ hybrid search (BM25 + vector, rank fusion)
      ─▶ group by item ─▶ optional recency boost and rerank ─▶ MMR
      ─▶ search: one hit per item
      ─▶ Ask: choose provider ─▶ pack passages with [n] headers (drop sensitive if cloud)
             ─▶ stream answer (chat.delta / chat.done) ─▶ egress row with the cited items
```

Fusion depth follows the number of results requested; deeper fusion cost about 8 points of recall@10.

## Models

The router tries configured on-device and LAN providers, then the managed runtime (`local-chat`,
`local-embedding`), then cloud providers, each in config order. A provider must have proved the needed
capability by probe; network errors and rate limits fall through to the next.

## Knowledge and MCP

After ingest commits (debounced, also periodically and at start), deterministic and optional LLM
extractors produce entities and claims; an LLM claim is kept only if its quote is found verbatim.
Re-extraction retracts an item's old evidence first. Changes feed the vault renderer and the
`knowledge.changed` notification. MCP requests (stdio bridge or loopback HTTP) pass token, scope and
rate-limit checks in the daemon before any read-only tool runs ([MCP](../reference/mcp.md)).

## Startup and shutdown

Startup: paths, single-daemon lock and socket, config, store key and store, models, providers and
router, ingest, knowledge, vault, MCP, local models, features, then IPC. The audit chain is verified in
the background (`status` says "not checked yet" until then). Shutdown on SIGINT or SIGTERM reverses
this: close connections (streams get `chat.done`), features, MCP, ingest, model servers, egress writer.
