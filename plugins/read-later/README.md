# Read-later exports (`dev.nugget.read-later`)

A connector that imports the articles you saved to a read-later service, from that service's own
export file. No network, no account: it reads the file (or folder of files) you pick, through the
host's read-only `fs` interface.

## Settings

```json
{ "export": "/Users/ada/Downloads/ril_export.html", "skip_archived": false }
```

- `export`: absolute path of an export file, or of a folder holding export files (searched up to 8
  levels deep for `.html`, `.htm` and `.csv`, dot-folders skipped). It is the manifest's file root
  (`capabilities.file_roots = ["export"]`).
- `skip_archived` (optional): leave out Pocket's "Read Archive" / `archive` items and Instapaper's
  `Archive` folder.

## Formats

| Export | Shape | Read as |
|---|---|---|
| Pocket `ril_export.html` | `<h1>Unread</h1><ul><li><a href="…" time_added="…" tags="a,b">Title</a>`, then `<h1>Read Archive</h1>` | folder from the heading, tags split on `,`, `time_added` in Unix seconds |
| Pocket CSV (2024 on, `part_000000.csv`) | `title,url,time_added,cursor,tags,status` | tags split on `\|`, status `unread`/`archive` named like the HTML export |
| Instapaper CSV | `URL,Title,Selection,Folder,Timestamp` (newer exports add `Tags`) | `Selection` is your highlight; `Folder` is `Unread`, `Archive`, `Starred` or your folder; tags as a JSON list or separated by `,`/`;`/`\|` |
| Instapaper HTML, Netscape bookmarks (Pinboard, Raindrop, browsers) | `<a href>` items under `<h1>`–`<h3>` headings, `ADD_DATE`, `TAGS`, `<DD>` description | same as Pocket HTML, the `<DD>` text as the selection |

Any other CSV with a `url` (or `link`) column works too: `title`, `time_added`/`timestamp`/
`created`/`saved_at`/`date`, `tags`/`labels`, `folder`/`status`, `selection`/`excerpt`/`note` are
picked up by name, ignoring case and order. Times may be Unix seconds, milliseconds or ISO 8601.

The parsers never fail on malformed input; they skip what they cannot read. HTML `<script>` and
`<style>` bodies and comments are ignored.

## What it does

- **One `text/markdown` document per article**, keyed by its normalised URL: scheme and host
  lowercased, default port, credentials, fragment and tracking parameters (`utm_*`, `fbclid`,
  `gclid`, `mc_cid`, …) dropped. The same article saved twice, or in two exports, is one document:
  tags are merged and the earliest save time wins. Links that are not `http(s)` are skipped.
- **Content** is a short card: the title, the URL, the save date, folder and tags, and the
  highlighted selection as a quote. The article itself is not fetched (this plugin has no hosts);
  index the page with Nugget's own web tools if you want its text.
- **Metadata:** `source` (`pocket`, `instapaper`, `bookmarks`, `csv`), `export_file`, `folder`,
  `tags`, `added_at`, `selection`. The document's `modified_at` is the save time and its `url` the
  original URL.
- **Passes.** The cursor holds a fingerprint of the export files (paths, sizes, modification
  times). When nothing changed, a pass does nothing. Otherwise the whole export is read, articles
  whose version changed are sent, and articles no longer in the export are deleted, but only if
  every export file was found and read: an unreadable file or subfolder, a folder nested too
  deeply or more than 1000 export files means nothing is deleted and the fingerprint is not
  recorded, so the next pass tries again. Credentials in saved links (`user:pw@`) are removed,
  and save times outside years ±9999 are left out. Re-exporting from the service and
  replacing the file is the way to update.

## Building and testing

```sh
cd plugins
cargo test-native -p nugget-read-later   # fixtures in tests/fixtures, in-memory and temp-dir roots
cargo build --release -p nugget-read-later
```
