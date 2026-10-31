# Obsidian

The `obsidian` source indexes an Obsidian vault: every Markdown note, plus PDF and image attachments
(`include_attachments`, on by default). It works like a [folder source](sources-files.md), with three
differences:

- **Notes are read as Markdown**: frontmatter goes into the item's metadata, headings become sections
  for chunking, and `[[wikilinks]]` and `#tags` are extracted.
- **Links open in Obsidian.** Search results link to `obsidian://open?vault=…&file=…`, so clicking one
  opens the note in Obsidian. The plain `file://` URL is kept in the item's metadata.
- **One root**: the vault folder.

**Access.** No sign-in and no network: the source reads the vault folder only, and never changes
your notes.

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

## Checking Obsidian with a real vault

The vault connector is tested against temporary vaults; this checklist confirms it with your own in
about 10 minutes. A copy of your vault, or a small test vault, is easiest. If a step fails, see
[Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Add the vault.** Expected: no sign-in; the source switches to `syncing`, then `watching`.
2. **First sync.** Expected: one item per note, plus one per PDF or image attachment. Nothing from
   `.obsidian/` is indexed.
3. **Edit.** Change a note in Obsidian. Expected: the new text is searchable within about a second.
   Frontmatter fields appear in the item's details, and `#tags` and `[[wikilinks]]` are extracted.
4. **Rename and move.** Rename a note in Obsidian (which also updates links to it in other notes),
   and move another to a different folder. Expected: the old names disappear, the new ones appear,
   and the notes whose links Obsidian rewrote are re-indexed.
5. **Delete.** Delete a note. Expected: its item disappears (Obsidian's own `.trash` folder is hidden,
   so it is not indexed).
6. **Unicode.** Create a note named `Notizen über Kuchen 🍰 日本語`. Expected: the title shows exactly
   that, and clicking the result opens the note in Obsidian (the `obsidian://` link encodes the name).
7. **Large attachment.** Add a PDF over 50 MiB. Expected: it is skipped and does not appear; there is
   no "Not indexed" placeholder. An image is indexed by its OCR text only when OCR models are
   installed.
8. **Lost access.** Rename or unmount the vault folder. Expected: the items stay until it comes back,
   then syncing resumes.
9. **Egress.** Expected: the **Privacy** page shows no `sync:obsidian` traffic at all.
