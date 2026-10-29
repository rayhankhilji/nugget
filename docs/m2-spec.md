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

## As built (M2-G and wave W follow-ups)

### Daemon wiring (M2-G1)
- **Extraction** runs in `nuggetd` on its own task (`crates/nuggetd/src/knowledge.rs`): at startup,
  after ingest commits (debounced), every `[knowledge] interval_minutes` (which also retries pending
  LLM extraction) and again while a run leaves planned items behind. While the power policy is
  constrained, runs make no LLM requests; the deterministic extractors still keep the graph fresh.
  Changed entity ids are broadcast as `knowledge.changed`.
- **Vault.** After every run, merge, split or claim-status change, pages of entities changed in the
  graph's change feed are re-rendered; the high-water mark is kept in the store together with the
  vault root, so moving the vault re-renders everything. The watcher turns user edits into
  `user_asserted` claims, and deleted fact lines into rejections. Default location `~/Nugget Vault`
  (`[vault] path`).
- **MCP** (`crates/nuggetd/src/mcp.rs`): loopback streamable HTTP on `127.0.0.1:7319`
  (`[mcp] port`; `nugget_mcp::http::DEFAULT_PORT` is the same) and the `nuggetd mcp --client <id>`
  stdio bridge, which holds no state: it authenticates through the daemon over the IPC socket, so
  scopes, revocation, rate limits and audit are enforced in one place. Clients (keyed token hashes
  only) and the HMAC server key live in the encrypted store; every call is an audit entry
  (`mcp:<client id>`).
- **Install helpers:** `nuggetd mcp install --client claude-desktop|claude-code|cursor` writes (or,
  for Claude Code, prints) a stdio config. Setup snippets are built entirely by
  `nugget_mcp::setup_snippets`; the daemon passes `NUGGET_HOME` through `SnippetOptions::env`.
- **IPC:** `knowledge.entities`, `knowledge.entity`, `knowledge.graph`, `knowledge.merge`,
  `knowledge.split`, `knowledge.claim.status`, `knowledge.status`, `mcp.clients.list|create|revoke`
  and the `knowledge.changed` notification (method table in `nugget_core::ipc`).

### Contract gaps closed (M2-G2 and W7)
- **`GraphStore`** gained `update_claim`, `split`, `prune_orphans`, `apply_batch`,
  `entity_provenance` (per-alias and per-identifier source items) and the sticky
  `ClaimStatus::Rejected`. Store schema v5 adds `item_changes` with deletion tombstones,
  `items_changed_since`, `item_dates` and `entity_detail_sources`; knowledge runs are incremental
  over the change log and write one batch per item.
- **Claim statuses over IPC** are `active`, `superseded`, `retracted` (the fact lost its evidence),
  `rejected` (the user rejected it; it stays rejected when its source is re-extracted) and
  `user_asserted`. `knowledge.claim.status` accepts `rejected` (`retracted` is kept as an older
  spelling of it), `active` (restore; a fact without sources comes back as `user_asserted`),
  `user_asserted` and `unasserted` (undo the user's assertion: `active` if sources support it, else
  `retracted`).
- **`knowledge.split`** (`SplitRequest { entity, claims, name, kind?, aliases, identifiers }`)
  moves facts, aliases and identifiers to a new entity and returns its `EntityRef`.
- **`GraphRequest.source`** filters the graph view to edges whose claim has evidence from that
  source and nodes mentioned in or asserted by it (the centre always stays).
- **MCP per-identifier visibility:** the daemon implements `KnowledgeBackend::entity_detail_origins`
  from `entity_provenance` plus each item's sensitivity (a missing item counts as sensitive), so a
  scoped client sees exactly the aliases and identifiers its sources supplied.
- **MCP HTTP hardening:** the listener binds the configured port or nothing (a taken port is
  logged as an error and only stdio is offered; no snippet ever points at a port the daemon does
  not hold), and caps connections (64), header time (10 s, also closing idle keep-alives) and body
  time (30 s; bodies over 1 MiB get 413). The caps are `HttpConfig` fields, not yet exposed in
  `[mcp]`.
- **Desktop:** the Knowledge page rejects, restores and removes facts with undo, shows `rejected`,
  splits entities (`SplitDialog`) and filters the graph view by source.

### Knowledge quality (W7)
The `bench knowledge` suite found these bugs, now fixed:
- The Markdown extractor quoted invented lines (`people: <name>`) as evidence. Every frontmatter
  claim now quotes the line it was read from, verbatim, and values are read from every matching
  key. New keys: `org` on person notes (`works_at`), `members`/`member`/`team` on projects
  (`works_on`), `owner`/`lead` (`leads`, was `works_on`), `club` (`member_of`) and
  `reports_to`/`manager`.
- Resolution: identifiers listed in a person's or organisation's own note are trusted like a
  contact card's; an unambiguous name-only duplicate is folded into the entity with identifiers;
  organisations found by mail domain link to the same organisation by name when the domain spells
  the name (first word, then words or initials, in order); a one-word name from the model joins
  the one person on the same item whose name starts with it. After a merge only the newest value
  of a single-valued predicate (`works_at`, `has_title`, ...) stays active; a user-asserted value
  wins.
- The bench breaks resolution ties by entity name rather than by random id, so identical runs
  score identically.

Results on the fixture corpus (`cargo run -p nugget-bench -- knowledge --strict`, a CI gate since
W7): every target passes. Entity F1 0.900 → 0.966 (organisations 0.667 → 1.0), claim F1
0.921 → 0.963, B³ F1 0.924 → 0.991, unsupported claims 3.7% → 0, invalid vault citations 20 → 0,
and all five hard cases pass.

The exit criterion "a real MCP client works" is covered by `crates/nuggetd/tests/mcp_clients.rs`:
it runs `nuggetd mcp install` for Claude Desktop and Cursor, parses the generated config and
launches exactly what it says; does the same for the `claude mcp add` line Claude Code is given;
and connects over HTTP with the generic snippet's URL and header. Each client initializes, lists
tools and gets the indexed note back from `get_context`.

### Known gaps
- `claims.superseded_active` is 1: a hand-over of a multi-valued fact ("Priya works on Fennel",
  handed to Oskar) is not superseded. It needs either complete-list semantics for a project
  note's owners and members or a hand-over fact from the LLM extractor.
- `EXTRACTOR_VERSION` was not bumped, so duplicates already in an existing graph are cleaned up
  only when a related item is processed again.
- A split entity can be merged back by later re-extraction if it shares identifiers with the
  original.
- The source-filtered graph overview queries claims and mentions per scanned entity (up to 5000).
