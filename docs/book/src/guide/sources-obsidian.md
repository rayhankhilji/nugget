# Obsidian

The `obsidian` source indexes an Obsidian vault: every note, plus PDF and image attachments
(`include_attachments`, on by default). It works like a [folder source](sources-files.md) (no
sign-in, no network, read-only), except:

- **Notes are read as Markdown**: frontmatter goes into metadata, headings become sections, and
  `[[wikilinks]]` and `#tags` are extracted.
- **Links open in Obsidian** (`obsidian://open?...`).
- There is one root: the vault folder.

## Add a vault

On **Sources**, choose **Add source → Obsidian vault** and pick the folder containing `.obsidian/`.

`.obsidian/` and `.trash` are hidden, so they are skipped. To skip other folders, add exclude patterns
or a `.nuggetignore` file.

Don't add the **Heed vault** (the pages Heed renders from its [knowledge graph](knowledge.md)) as
an `obsidian` source. To read it in Obsidian, open it as a separate vault.

All settings: [Connector settings → `obsidian`](../reference/connectors.md#obsidian).
