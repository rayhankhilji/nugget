# Search and Ask, with citations

## Search

Type into **Search** and you get one result per item (file, message, event or note): its title, the
best-matching passage, and a link that opens the original (the file, the Obsidian note, or the message
in your mail client or web app, when its source provides a link).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/screenshots/search-dark.png">
  <img src="../images/screenshots/search-light.png" alt="The Search page with results for &quot;Tidewater budget&quot;: each hit shows its title, the best-matching passage and the source it came from" loading="lazy">
</picture>

How a query is answered:

1. **Hybrid matching.** The query runs through full-text search (BM25 over every chunk and a separate
   title index) and, when an embedding model is available, vector search over chunk embeddings. The
   two ranked lists are fused with reciprocal-rank fusion.
2. **One hit per item.** Chunks are grouped by item and the best chunk represents it.
3. **Diversity.** Maximal marginal relevance (MMR) re-ranks the top results so near-duplicates do not
   crowd out everything else.

With no embedding model, search is **full-text only**, and the Indexing widget says so. Items indexed
before an embedder was available are embedded in the background once one is (at startup, when the
local embedding server becomes ready, and every 10 minutes).

Embeddings are computed locally or on your network by default. Search never sends your query to a
cloud embedding provider unless you set
[`retrieve.allow_cloud_embedding`](../reference/config.md#retrieve).

## Ask

**Ask with context** (on the Search page) answers a question from your own data:

1. Nugget picks the model first: by default the first provider that serves the `synthesis` tier.
2. It retrieves the most relevant passages and packs them into a token budget, each under a numbered
   header such as `[1] Q3 planning (files-1)`. Neighbouring chunks are added while the budget allows.
3. The model is told to answer only from those passages, to cite them as `[1]`, `[2][3]`, to say so
   when the passages do not contain the answer, and to treat the passages as data, not instructions.
4. The answer streams in. Each `[n]` links to its source.

The **Chat** page asks the same way when **Use my sources** is on, and keeps the conversation:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/screenshots/chat-dark.png">
  <img src="../images/screenshots/chat-light.png" alt="The Chat page: an answer about Tidewater Billing with [1] and [2] citations, and the numbered list of sources it was given" loading="lazy">
</picture>

### What the citations promise, and what they do not

- Every `[n]` refers to a real passage that was sent to the model, and the list of sources sent is
  shown with the answer.
- The model decides where to put citations. Nugget does not yet check that each sentence is supported
  by the passage it cites (citation-accuracy scoring is planned for the `memory` benchmark).

### Sensitive items and cloud models

If the model chosen is a **cloud** model, items marked **sensitive** are left out of the context, and
the answer reports how many were withheld. An item is sensitive when the deterministic scan found a
payment card number, a US Social Security number, an IBAN or something that looks like an API key or
private key, or when local triage flagged it. Items whose sensitivity is unknown are treated as
sensitive. With an on-device or local-network model, nothing is withheld.

Every Ask sent to a non-local model is recorded in the [egress ledger](privacy.md) with feature `ask`
and the list of items included.

## From other programs

- [`search`](../reference/ipc.md#search) returns hits.
- [`retrieve.context`](../reference/ipc.md#retrieval) returns the packed, numbered passages and their
  citations, without calling a model.
- [`retrieve.ask`](../reference/ipc.md#retrieval) does the whole Ask flow and streams the answer.
- Over MCP, the `search` and `get_context` tools do the same for Claude Desktop, Claude Code, Cursor
  and other apps, limited to the sources each app is allowed to see (see [MCP](mcp.md)).

The same budgeted, cited retrieval feeds [Double Tap](double-tap.md) drafts.
