# M2: Knowledge graph spec

**Goal:** turn indexed items into a **typed, cited knowledge graph**:
- people, organisations, projects, commitments, preferences, events, places and topics, with relations between them;
- **every fact traceable to the source passages it came from**;
- the graph is rendered as a Markdown vault the user can read and edit;
- the graph is exposed to other AI tools over **MCP**.

**Exit criteria** (PLAN.md §5):
- 100% of rendered vault facts carry at least one citation, checked by the `bench knowledge` suite and by vault validators.
- Claude Desktop, Claude Code and Cursor can connect to Nugget over MCP and answer from it. The connection is verified by an MCP client integration test and documented setup.
- User edits to vault pages survive re-renders (three-way merge tests).
- CI is green.

**What we're beating** (`docs/research/sentient-os-teardown.md` §2.5):
- Sentient's vault is free-text written by an LLM, with no provenance, no validation and no entity store.
- Its links resolve by file-name stem only, and renames break them.
- Its MCP mirror puts a password in the URL and decrypts the vault on their server.

The M0 and M1 ground rules still apply. Two privacy rules are new:
- **Extraction stays local.** Extraction by an LLM uses OnDevice/LocalNetwork providers by default. Cloud extraction is opt-in per source and is never applied to sensitive items.
- **MCP is local.** It is loopback/stdio only, with per-client scoped tokens, revocation, an audit entry per call and a recorded egress-ledger entry for any remote client. In M2 there is no remote MCP at all.

## Architecture
```
store items/chunks ──▶ nugget-knowledge (extract → resolve → claims with provenance) ──▶ graph tables (nugget-store v4)
                                                                                         │
                     nugget-vault (render pages ⇄ three-way merge with user edits) ◀─────┤
                     nugget-mcp (tools/resources over stdio + loopback HTTP) ◀────────────┤
                     desktop Knowledge page + graph view ◀── nuggetd IPC ◀───────────────┘
```

## Data model (contract, written before wave 1)
- **Entity:** `{ id (stable, never reused), kind, name, aliases[], attributes (JSON), created_at, updated_at, merged_into? }`
  - Kinds: `person, organization, project, commitment, preference, event, place, topic`.
- **Claim:** `{ id, subject entity, predicate, object (entity | literal), text, confidence, status (active | superseded | retracted | user_asserted), valid_from?, valid_to? }`
  - Every claim has **one or more `Evidence { item, chunk_id, quote span? }`**.
  - A claim with no evidence can exist only as `user_asserted`, whose evidence is the vault edit itself.
- **Mention:** `entity ↔ chunk`, with a span and confidence. This is what the graph view and "where is this person mentioned" use.
- **Merging and splitting.** Entity merges keep the old id as an alias through `merged_into`, so links and MCP references never break.

## Work packages, wave 1 (parallel)
| WP | Crate(s) | Summary |
|---|---|---|
| **M2-A** | `nugget-store` (schema v4, graph module) and `nugget-graph` | Graph storage and API: entities, claims, evidence, mentions; alias, email and phone indexes; merge and split; neighbourhood queries; provenance queries ("why do we believe X"); a change feed for the vault and MCP |
| **M2-B** | `nugget-knowledge` | Extraction pipeline. **Deterministic extractors** for mail headers (people and orgs by address and domain), calendar attendees and events, Markdown frontmatter, wikilinks and tags, and contacts. **LLM extractor** through `Tier::Synthesis`, local by default, with a JSON schema for entities, relations and claims with quote spans. **Entity resolution** by exact identifiers, then alias and normalised name, then embedding similarity, then optional LLM adjudication. Incremental: re-extract when items change and retract claims whose evidence was deleted |
| **M2-C** | `nugget-vault` | Renders the graph to Markdown: one page per entity, YAML frontmatter (`id`, `kind`, `aliases`), facts with footnote citations linking to sources, and `[[id-stable links]]`. **Three-way merge** of the last render, the user's file and the new render. User-written blocks are preserved, and user edits to facts become `user_asserted` claims. A vault watcher and validators (links resolve, frontmatter schema, citation coverage) |
| **M2-D** | `nugget-mcp` | MCP server via the official Rust SDK (`rmcp`, license to be checked), on stdio plus streamable HTTP bound to loopback. **Tools:** `search`, `get_context` (cited), `get_entity`, `find_entities`, `get_neighbourhood`, `get_item`, `recent_changes`. **Resources:** vault pages. Per-client tokens with scopes (which sources and tools), revocation, an audit entry per call, and rate limits. A `KnowledgeBackend` trait so the daemon can wire it up in wave 2 |
| **M2-E** | `apps/desktop` | **Knowledge** page: entity browser, entity detail with cited facts ("why?" opens the evidence), merge and split, and an edit-in-vault button. **Graph view**: force layout on canvas, filter by kind and source, neighbourhood expansion. **Connected AI apps** page: create, revoke and scope MCP clients, with copy-paste setup for Claude Desktop, Claude Code and Cursor |
| **M2-F** | `bench` | A `knowledge` suite on a labelled fixture corpus, measuring entity and relation precision and recall, resolution accuracy, citation coverage (must be 100%), re-render merge fidelity and extraction throughput, plus an MCP round-trip latency check |

## Wave 2
- **M2-G `nuggetd` integration:** run knowledge extraction after ingest commits, vault render and watch, and the MCP server (`nuggetd mcp` stdio bridge subcommand plus a loopback HTTP listener); the new IPC methods; MCP client install helpers (`nuggetd mcp install --client claude-desktop|claude-code|cursor`).
- **Review:** security reviewers (MCP auth and scoping, vault path safety, prompt injection from source content into extraction) and correctness reviewers, then fixes.

## Contract gaps found in wave 1 (to resolve in wave 2)
- **`GraphStore` changes:**
  - Add `update_claim` (confidence and validity patches).
  - Add a distinct user-rejected status, so a fact the user rejected isn't revived when its source is re-extracted.
  - Add entity cleanup or deletion for entities left without claims or mentions.
  - Document that `add_claim` keeps the higher confidence.
- **Store changes:** add `items_changed_since` with deletion tombstones, plus a cheap `item_dates`. Knowledge currently diffs `item_versions` per source on every run.
- **MCP:** per-identifier provenance, because aliases and identifiers are hidden when any of the entity's sources is out of scope. Also revisit `is_sensitive` treating a missing flag as sensitive.
- **Desktop:** a source filter on `GraphRequest`, entity split, and undoing a user-asserted fact.
