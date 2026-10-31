# Obsidian vault (`dev.nugget.obsidian`)

A connector that imports the Markdown notes of an Obsidian vault (or any folder of Markdown
notes) through the host's read-only `fs` interface. It declares no hosts, secrets or key-value
store: the only thing it can reach is the folder you pick.

## Settings

```json
{ "vault": "/Users/ada/Documents/Notes", "exclude": ["Templates", "Archive/Private"] }
```

- `vault`: absolute path of the vault folder. It is the manifest's file root
  (`capabilities.file_roots = ["vault"]`), so the host resolves every read below it, refuses
  `..` and absolute paths, and hides symlinks that lead out of it.
- `exclude` (optional, at most 64): vault-relative folders to skip, matched ignoring case.

`validate` checks that the folder exists and is a folder.

## What it does

- **Walks the vault** in name order, skipping `.obsidian/`, `.trash/`, every other dot-folder and
  dot-file, the excluded folders, and anything that is not `*.md`. Nesting deeper than 64 folders
  is not followed (guards against symlink loops inside the vault); at most 100 000 notes.
- **One `text/markdown` document per note**, keyed by its vault-relative path (`Daily/2026-01-05.md`),
  with version `<mtime>:<size>`. Only notes whose version differs from what the store holds
  (`sink::known_items`) are read and sent, so an unchanged vault costs one directory walk.
- **Content** is the note without its front matter. **Title** is the front matter `title`, else the
  file name. **URL** is `obsidian://open?vault=<name>&file=<path>`, which opens the note in Obsidian.
- **Metadata:** `vault`, `path`, `folder`, `tags` (front matter `tags`/`tag` plus inline `#tags`,
  nested tags like `#work/meetings` kept whole, numbers-only tags ignored), `aliases`, `links`
  (`[[wikilinks]]` without `|alias` or `#heading`, plus relative `[text](Note.md)` links), `embeds`
  (`![[...]]`) and `properties` (other front matter fields as strings). Code blocks, inline code and
  indented code are not scanned.
- **Front matter** is read by a small, tolerant parser for the YAML Obsidian writes: `key: value`,
  quoted strings, `[inline, lists]`, block lists, `|` and `>` block scalars. Nested maps are skipped
  rather than misread. A file without a closing `---` has no front matter.
- **Deletes.** Notes the store holds that are gone are deleted on every pass, except under a
  folder this pass could not see (it failed to list or is nested too deeply): those keep their
  notes, so a transient error never wipes them. Reaching the note cap suspends deletes for the
  whole vault. A folder that lists exactly like one of its ancestors (a symlink back up the tree)
  is skipped, so loops do not import notes repeatedly. Modification times Nugget cannot store
  (beyond years ±9999) are left out of the document. A note that cannot be read (too large, vanished) is logged
  and keeps its stored version.
- **Cancellation** is checked per folder and per note.

There is no tool: tools cannot read files (the host denies `fs` outside a connector sync).

## Building and testing

```sh
cd plugins
cargo test-native -p nugget-obsidian   # in-memory vaults and a real temp-dir vault
cargo build --release -p nugget-obsidian
```
