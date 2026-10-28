# Folders and files

The `files` source indexes one or more folders and keeps them up to date with a native file-system
watcher (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows).

## Add a folder

1. On **Sources**, choose **Add source → Folder**.
2. Pick one or more folders.
3. Optionally list file types to include (leave empty for the defaults) and patterns to exclude.

The folder must exist when you add it. If it later disappears (an unmounted drive, say), its items are
kept until it comes back.

## What gets indexed

By default, every file type Nugget can extract text from:

- **Documents and text**: Markdown, plain text, reStructuredText, AsciiDoc, Org, TeX, CSV, TSV, JSON,
  YAML, TOML, XML, HTML, Jupyter notebooks
- **Office and PDF**: PDF, DOCX, XLSX
- **Mail and calendar files**: `.eml`, `.ics`
- **Source code** in about 50 languages
- **Images** (PNG, JPEG, WebP), through OCR, only when OCR models are installed
  (see [`NUGGET_OCR_MODELS`](../reference/environment.md))

These are always skipped, whatever the settings say:

- version-control metadata (`.git`, `.hg`, `.svn`, ...)
- dependency and build output (`node_modules`, `target`, `.venv`, `__pycache__`, ...)
- caches and OS trash (`.cache`, `.Trash`, `$RECYCLE.BIN`, ...)
- OS junk and editor temporaries (`.DS_Store`, `Thumbs.db`, `*.swp`, `~$*`, ...)
- hidden files, unless `include_hidden` is set
- files over 50 MiB, unless `max_file_bytes` is raised
- symbolic links, unless `follow_symlinks` is set (loops are detected)

`.gitignore` and `.nuggetignore` files are honoured in every directory. Add a `.nuggetignore` (same
syntax as `.gitignore`) to keep a folder's private corners out of the index.

## Exclude patterns

Patterns match case-insensitively:

- without a `/`, a pattern matches a name at any depth: `drafts`, `*.log`;
- a leading `/` anchors it to the root: `/archive`;
- a trailing `/` matches directories only: `build/`.

## How fast is it?

A change becomes searchable in about half a second: most of that is the 300 ms watcher debounce
(`debounce_ms`). As a safety net, the source also re-checks everything every hour (every 15 minutes on
Windows, whose watcher can drop events silently): `rescan_interval_secs`.

All settings: [Connector settings → `files`](../reference/connectors.md#files).
