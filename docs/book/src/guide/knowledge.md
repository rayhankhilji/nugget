# The knowledge graph and vault

> **Status: built, not yet running in the daemon.** The extraction pipeline (`nugget-knowledge`), the
> graph store and the vault renderer (`nugget-vault`) are implemented and tested as libraries, and the
> desktop's **Knowledge** page and graph view are built. `nuggetd` does not run extraction or render
> the vault yet, so the Knowledge page currently shows "not available". This page describes how they
> behave once wired in.

Search finds passages. The knowledge graph remembers **who and what** those passages are about: people,
organisations, projects, commitments, preferences, events, places and topics, and the facts that link
them. Every fact points back to the passages it came from.

## What is in the graph

- **Entities** have a kind, a name, aliases, identifiers (email addresses, phone numbers) and a
  **stable id** that is never reused. When two entities are merged, the old id keeps working: it
  redirects to the surviving entity, so links and references never break.
- **Facts** (claims) link an entity to another entity or to a value: "Sam works at Acme", "the launch
  is on 3 March". Each fact has a confidence and **evidence**: the item, the chunk and a short quote it
  came from. A fact without evidence can exist only if you wrote it yourself.
- **Mentions** record every place an entity appears, for "where is this person mentioned?".

The user is an entity too (marked `is_me`), recognised by the addresses in your sent mail.

## How facts are extracted

Extraction runs on items after they are indexed:

1. **Deterministic extractors**, always on: mail headers (people by address, organisations by
   corporate domain, who corresponded with whom), calendar events (attendees, organiser, location),
   Markdown (tags, frontmatter, wikilinks), and contacts.
2. **An optional LLM extractor** on the `synthesis` tier, local providers only by default. Every fact
   it proposes must carry a quote found **verbatim** in the source text; facts with invented quotes are
   dropped.
3. **Entity resolution** merges mentions of the same thing: identifiers first, then names and aliases,
   optionally embeddings; never across conflicting identifiers.

Sending items to a cloud model for extraction is opt-in per source, and never applies to sensitive
items.

When a source item is edited, its facts are re-extracted. When it is deleted, facts that relied only
on it lose their evidence and are retracted.

## The vault

The graph is rendered as a folder of Markdown pages you can read in any editor, including Obsidian:

```text
<vault>/README.md                          index of pages by kind
<vault>/<Kind>/<Name> (<short id>).md      one page per entity
<vault>/.nugget/                           Nugget's merge state (plain files)
```

A page looks like this:

```markdown
---
nugget_id: "ent_000001"
kind: "person"
aliases: ["Sam"]
updated: "2026-10-28T09:00:00Z"
---
# Samantha Lee

## Facts

### Works at
- Samantha works at [[Organization/Acme (ent_000002)|Acme]]. [^1]

## Related
- [[Organization/Acme (ent_000002)|Acme]]: works at

[^1]: [Lunch on Friday](https://mail.example/1): "Sam from Acme"

## Notes
```

(Invisible HTML comments around each region and at the end of each fact line are omitted above. They
let Nugget tell its regions and your edits apart; leave them in place.)

Links include the entity's short id, so renaming a person does not break links to them. Email
addresses and phone numbers are kept out of the pages by default, because the vault is plain text you
may sync elsewhere.

## Editing the vault

The vault is yours to edit. The encrypted store stays the source of truth, and your edits are merged
back into it with a three-way merge (the last render, your file and the new render):

| You do this | Nugget does this |
|---|---|
| Write anything outside the generated regions (for example under `## Notes`) | Keeps it, in your order, on every re-render |
| **Edit** a fact line | The old fact becomes `superseded`; your text becomes a new fact with status `user_asserted`, which is **yours**: it needs no source and shows a `(you)` tag |
| **Delete** a fact line | The fact becomes `retracted` |
| **Add** a bullet under a fact heading | A new `user_asserted` fact with that heading's relation (`note` outside any heading) |
| Add an alias in the frontmatter | The alias is added to the entity |
| Change a generated region that Nugget also changed | Your version is kept, and Nugget's is written next to it under `.nugget/conflicts/` for you to compare |

Removing a footnote citation is not treated as an edit. Before writing a page, Nugget re-reads your
file and retries if it changed, so a render never overwrites an edit you just made.

> **Known gap: deleted facts can come back.** A fact you delete is retracted, but if extraction later
> finds the same fact in the same or another source, the current graph store treats the new evidence
> as a reason to reactivate it. A separate "rejected by you" status that re-extraction cannot undo is
> planned (it is listed as a wave-2 contract gap in the M2 spec) and is not implemented yet. Until
> then, the dependable way to keep a wrong fact out is to edit it: your edited version is
> `user_asserted` and outranks extracted facts.

## In the desktop app

The **Knowledge** page lists entities by kind, shows each entity's facts with a **Why?** button that
opens the evidence, lets you merge duplicates, and opens the entity's vault page. The **graph view**
draws the neighbourhood of an entity, filtered by kind.

Over IPC: [`knowledge.*`](../reference/ipc.md#knowledge-graph). Over MCP: `find_entities`, `get_entity`
and `get_neighbourhood` (see [MCP tools](../reference/mcp.md)).
