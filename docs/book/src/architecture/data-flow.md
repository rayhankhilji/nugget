# Data flow

## From a change to a search result

```text
connector ──Change──▶ bounded queue ──▶ worker (in order, per source)
 (sync / watch)          1. extract  (size caps, timeout, panic guard)
                         2. PII scan (deterministic: cards, SSNs, IBANs, secrets → sensitive)
                         3. chunk    (structure-aware: target 400 tokens, max 512, overlap 48)
                         4. embed    (batched; local unless allowed; text-only now, embedded later)
                         5. store    (item + chunks + version in one transaction)
            checkpoint(cursor) ──▶ saved only after every change before it is stored
            background: local triage labels junk or sensitive; never removes from search
```

Versions (mtime and size, ETag, mod-sequence) let unchanged items be skipped. A sync with no cursor
reconciles, deleting items gone from the source. A file change is searchable in 0.6 s at p95
([Benchmarks](benchmarks.md)).

## From a question to an answer

```text
query ─▶ (follow-up? rewrite locally) ─▶ embed ─▶ hybrid search (BM25 + vector, rank fusion)
      ─▶ group by item ─▶ optional recency boost and rerank ─▶ MMR ─▶ search hits
      ─▶ Ask: choose provider ─▶ pack passages with [n] headers (drop sensitive if cloud)
             ─▶ stream answer (chat.delta / chat.done) ─▶ egress row with the cited items
```

**Models:** routed by tier in the order given in [`[[providers]]`](../reference/config.md#providers);
a provider must have proved the capability by probe, and errors fall through to the next.

**Knowledge:** after ingest commits (and periodically), extractors produce entities and claims; an LLM
claim needs a verbatim quote, and re-extraction retracts old evidence first. Changes re-render the
vault and emit `knowledge.changed`.

**Startup** ends with IPC (the audit chain is verified in the background); **shutdown** reverses it.
