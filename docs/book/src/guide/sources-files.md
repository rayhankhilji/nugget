# Folders and files

The `files` source indexes one or more folders and keeps them up to date with a native file-system
watcher (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows).

**Access.** No sign-in and no network: the source reads only the folders you pick, through the
operating system's normal file permissions, and never writes to them.

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

## Checking folders with a real folder

The folder connector is tested against temporary folders; this checklist confirms it on your own
machine in about 10 minutes. Use a test folder with a few dozen files. If a step fails, see
[Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Add the folder.** Expected: no sign-in; the source switches to `syncing`, then `watching`.
2. **First sync.** Expected: the item count matches the files of supported types, minus hidden files,
   files matched by `.gitignore` or `.nuggetignore`, and the always-skipped folders listed above.
3. **Edit.** Change a Markdown file and save it. Expected: the new text is searchable within about a
   second, without **Sync now**.
4. **Rename and move.** Rename one file and move another into a subfolder. Expected: the old paths
   disappear and the new ones appear, with no duplicates.
5. **Delete.** Delete a file, and then a whole subfolder. Expected: their items disappear.
6. **Unicode.** Create `Notizen über Kuchen 🍰 日本語.md` containing `Grüße`. Expected: the title shows
   exactly that name, searching for `Grüße` finds it, and its link opens the file.
7. **Large file.** Add a file over 50 MiB (for example a big `.txt`). Expected: it is skipped and does
   not appear at all; there is no "Not indexed" placeholder. A `debug` log line says
   "over max_file_bytes; skipping". Raising `max_file_bytes` above its size indexes it.
8. **Lost access.** Remove read permission from an indexed file (`chmod 000` on macOS or Linux) and
   change another file. Expected: syncing goes on, the unreadable file keeps its earlier indexed copy,
   and the log says "skipping unreadable path". Unmounting or renaming the whole folder also keeps its
   items until it comes back.
9. **Egress.** Expected: the **Privacy** page shows no `sync:files` traffic at all.
