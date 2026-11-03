# Folders and files

The `files` source indexes folders and keeps them current with the OS file watcher. It needs no
sign-in, makes no network requests, and never writes to your folders.

On **Sources**, choose **Add source → Folder**, pick one or more existing folders, and optionally list
file types to include and patterns to exclude. If a folder later disappears (an unmounted drive), its
items are kept until it comes back.

## What gets indexed

- **Text and documents**: Markdown, plain text, reStructuredText, AsciiDoc, Org, TeX, CSV, TSV, JSON,
  YAML, TOML, XML, HTML, Jupyter notebooks
- **Office and PDF**: PDF, DOCX, XLSX
- **Mail and calendar files**: `.eml`, `.ics`
- **Source code** in about 50 languages
- **Images** (PNG, JPEG, WebP) via OCR, only when OCR models are installed
  (see [`HEED_OCR_MODELS`](../reference/environment.md))

Always skipped: version-control metadata, dependency and build output (`node_modules`, `target`,
`.venv`, ...), caches and trash, OS junk and editor temporaries. Also skipped unless you change the
setting: hidden files (`include_hidden`), files over 50 MiB (`max_file_bytes`) and symlinks
(`follow_symlinks`).

`.gitignore` and `.nuggetignore` files (same syntax) are honoured in every directory.

## Exclude patterns

Case-insensitive. Without a `/`, a pattern matches a name at any depth (`drafts`, `*.log`); a leading
`/` anchors it to the root (`/archive`); a trailing `/` matches directories only (`build/`).

A change is searchable in about half a second (`debounce_ms`, 300 ms). The source also rescans hourly,
every 15 minutes on Windows (`rescan_interval_secs`).

All settings: [Connector settings → `files`](../reference/connectors.md#files).
