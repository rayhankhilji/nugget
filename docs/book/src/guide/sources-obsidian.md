# Obsidian

The `obsidian` source indexes an Obsidian vault: every Markdown note, plus PDF and image attachments
(`include_attachments`, on by default). It works like a [folder source](sources-files.md), with three
differences:

- **Notes are read as Markdown**: frontmatter goes into the item's metadata, headings become sections
  for chunking, and `[[wikilinks]]` and `#tags` are extracted.
- **Links open in Obsidian.** Search results link to `obsidian://open?vault=…&file=…`, so clicking one
  opens the note in Obsidian. The plain `file://` URL is kept in the item's metadata.
- **One root**: the vault folder.

## Add a vault

1. On **Sources**, choose **Add source → Obsidian vault**.
2. Pick the vault folder (the one that contains `.obsidian/`).

Obsidian's own `.obsidian/` settings folder is hidden, so it is skipped unless you set
`include_hidden`. To skip other folders (templates, an archive), add exclude patterns or a
`.nuggetignore` file.

## Nugget's own vault is different

Do not confuse this with the **Nugget vault**: the Markdown pages Nugget renders from its knowledge
graph (see [The knowledge graph and vault](knowledge.md)). If you want to read those in Obsidian, open
the Nugget vault folder in Obsidian as a separate vault. Do not add it as an `obsidian` source.

All settings: [Connector settings → `obsidian`](../reference/connectors.md#obsidian).
