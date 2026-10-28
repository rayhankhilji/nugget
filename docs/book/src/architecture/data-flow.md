# Data flow

## From a change to a search result

```text
connector ──Change──▶ bounded queue ──▶ worker (in order, per source)
 (sync / watch)                           1. extract  (blocking thread, timeout, size caps)
                                          2. PII scan (deterministic; counts only)
                                          3. triage   (local triage-tier model, optional)
                                          4. chunk    (structure-aware, deterministic boundaries)
                                          5. embed    (embedding tier, batched; local by default)
                                          6. store    (item + chunks + version in one transaction)
            checkpoint(cursor) ─────────▶ cursor saved only after every change before it is stored
```

1. **Detect.** A connector emits `Upsert` or `Delete` changes. File sources watch the file system;
   mail and calendar sources poll with delta tokens (Gmail history ids, Calendar sync tokens, CalDAV
   sync tokens, IMAP UIDs and mod-sequences) or listen with IMAP IDLE. Each item carries a **version**
   (modification time and size, ETag, mod-sequence), so unchanged items are skipped cheaply.
2. **Extract.** `nugget-extract` turns bytes into text plus structure: title, sections (heading paths),
   links, tags and metadata. Every input is treated as hostile: size caps, a timeout, and a panic guard.
3. **Scan and triage.** A deterministic scan looks for card numbers, SSNs, IBANs and secrets; any
   finding marks the item **sensitive**. If a local triage model is available, it adds keep, junk or
   sensitive and a short summary. Cloud models never triage.
4. **Chunk.** The chunker splits by structure (target 400 tokens, at most 512, overlap 48), with the
   heading path as context.
5. **Embed.** The embedding tier vectorises chunks in batches. Cloud embedders are skipped unless
   allowed, and never used for sensitive items. If no embedder is available, the item is stored
   text-only and embedded later by a background job.
6. **Store.** The item, its chunks and its version are written in one SQLCipher transaction, so a
   crash never leaves an item without chunks. The source's cursor is saved only after every change
   before it is stored, so a crash never skips data.

Changes of one source are applied in order. A `sync` with no cursor while items already exist is a
**reconciling** pass: items that are gone from the source are deleted. That is how deletes made while
Nugget was not running are caught.

**Measured:** a file operation becomes searchable in 0.6 s at the 95th percentile on the standard
benchmark workload, of which 0.3 s is the watcher debounce
([Benchmarks](benchmarks.md)).

## From a question to an answer

```text
query ──▶ embed query (local by default) ──▶ hybrid search (BM25 + vector, rank fusion)
      ──▶ group by item ──▶ optional recency boost ──▶ MMR diversity
      ──▶ search: one hit per item
      ──▶ Ask: choose provider ─▶ pack passages under budget with [n] headers
                                 (drop sensitive items if the provider is cloud)
              ─▶ stream answer (chat.delta / chat.done) ─▶ egress row with the cited items
```

The fusion depth follows the number of results requested, not the over-fetched candidate count. This
came out of the benchmark: deeper lists let passages that were mediocre in both lists outrank a strong
match from one of them, and cost about 8 points of recall@10.

## Models

```text
request (tier) ──▶ Router ──▶ [configured on-device and LAN providers, in config order]
                          ──▶ [managed runtime: local-chat / local-embedding]
                          ──▶ [configured cloud providers, in config order]
                   each must have proved the needed capabilities by probe;
                   network errors and rate limits fall through to the next
```

The managed runtime starts `llama-server` on demand and stops it after idle minutes. Every request to
a non-on-device provider records an egress event with its feature and item references.

## Knowledge

```text
ingest commits ──▶ knowledge task (debounced; also periodic and at startup)
   ──▶ nugget-knowledge: deterministic extractors + optional LLM extractor (quotes verified)
   ──▶ entity resolution ──▶ claims with evidence, mentions ──▶ graph tables in the store
   ──▶ knowledge.changed notification
   ──▶ graph change feed ──▶ nugget-vault: re-render changed pages; watcher merges your edits back
```

The daemon runs extraction on its own task: at startup, shortly after ingest commits changes, and
every `interval_minutes`. Extraction is incremental: each source's item versions are diffed against a
ledger, re-extraction of an item first retracts its old evidence, and items that disappear are
retracted. The LLM extractor uses local providers unless you allow otherwise, and every claim it
returns must quote the source verbatim. While the power policy is constrained, runs make no model
requests; the deterministic extractors still keep the graph fresh.

## MCP

```text
AI app ──stdio──▶ nuggetd mcp --client <id> ──IPC socket──▶ nuggetd MCP server
AI app ──HTTP (127.0.0.1, bearer token)────────────────────▶ nuggetd MCP server
   token check ─▶ client scopes (sources, tools, include_sensitive) ─▶ rate limit
   ─▶ read-only tool over the store, graph and retriever ─▶ audit entry (no content)
```

The stdio bridge holds no state: it relays MCP lines over the daemon's IPC socket, and every check
happens in the daemon. See [MCP tools and scopes](../reference/mcp.md).

## Wave W features

> **Status:** being wired in (wave W). Double Tap, the agent and policy engine, proactive cards and
> plugins will plug into the daemon's features seam (`crates/nuggetd/src/features/`); none is
> registered yet. See the [architecture overview](overview.md).

## Startup and shutdown

**Startup:** resolve paths; take the single-daemon lock and bind the socket; load or create
`config.toml`; load the store key and open the store; open the model store; build providers and the
router (the runtime's slots included); start ingest over `sources.toml`; build the knowledge
service and vault, and the MCP server (with its loopback HTTP listener when enabled); start the
knowledge task and installed local models; start the registered features; serve IPC. The audit chain is
verified in the background; until it finishes, `status` reports it as not checked yet.

**Shutdown** (SIGINT, SIGTERM, or Ctrl+C on Windows): stop accepting connections and close them (open
chat streams get `chat.done` first) and stop background tasks; shut down the features, then the MCP
server; shut down ingest; shut down the runtime's servers; flush the egress writer.
