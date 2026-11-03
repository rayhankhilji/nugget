# Data directory

`nuggetd` keeps everything in three per-user directories: **data**, **config** and **cache**.

## Where they are

| OS | Data | Config | Cache |
|---|---|---|---|
| Linux | `~/.local/share/heed` (or `$XDG_DATA_HOME/heed`) | `~/.config/heed` (or `$XDG_CONFIG_HOME/heed`) | `~/.cache/heed` (or `$XDG_CACHE_HOME/heed`) |
| macOS | `~/Library/Application Support/app.heed.Heed` | `~/Library/Application Support/app.heed.Heed` | `~/Library/Caches/app.heed.Heed` |
| Windows | `%LOCALAPPDATA%\heed\Heed\data` | `%APPDATA%\heed\Heed\config` | `%LOCALAPPDATA%\heed\Heed\cache` |
| With `HEED_HOME=DIR` (or `NUGGET_HOME`) or `--home DIR` | `DIR/data` | `DIR/config` | `DIR/cache` |

The desktop app's own folders (window state, settings, the window's web storage) are named after
its app ID, `app.heed.desktop`: on macOS `~/Library/Application Support/app.heed.desktop` and
`~/Library/WebKit/app.heed.desktop`, on Linux `~/.local/share/app.heed.desktop` and
`~/.config/app.heed.desktop`, on Windows `%APPDATA%\app.heed.desktop` and
`%LOCALAPPDATA%\app.heed.desktop`. Secrets are in the OS keychain under the service
`app.heed.Heed`.

### Moving from the names before the rename

Heed was called Nugget before. The first launch of this version moves what an earlier version
left:

| Before | Now | How |
|---|---|---|
| Data and config folders (`~/.local/share/nugget`, `~/.config/nugget`; `dev.Nugget.Nugget` on macOS; `Nugget\Nugget` on Windows) | The folders above | Copied, each file checked against the original, then renamed into place with a marker file, `.heed-migrated.json`. The cache is not copied; it is rebuilt. Models are hard-linked where the disk allows, so they take no extra space |
| The app's folders, `dev.nugget.desktop` (and macOS `~/Library/WebKit/dev.nugget.desktop`) | `app.heed.desktop` | The same, before the window opens |
| Keychain service `dev.nugget.Nugget` (API keys, account sign-ins, the store key) and `nugget` (the Heed Cloud session) | `app.heed.Heed` | Each entry is copied the first time it is read, and read back before it is used |
| Login entries `dev.nugget.desktop.autostart` and `dev.nugget.nuggetd` | `app.heed.desktop.autostart`, `app.heed.nuggetd` | Open at login is rewritten at launch; the background service when it is installed again |
| `NUGGET_*` environment variables | `HEED_*` | Both work; `HEED_*` wins when both are set |

The old folders and keychain entries are never deleted: once you have checked that Heed works,
delete them yourself, or run `packaging/scripts/remove-nugget-data.sh`, which lists both. A
folder that already has the marker is not moved again, and a new folder that holds data the move
did not put there is kept as it is. While a daemon of the earlier version is still running on the
old folders, Heed uses them as they are and moves them on a later start. On macOS the keychain
may ask once whether Heed may read an entry the earlier version saved.

Some names are kept for now: the vault folder `~/Nugget Vault`, the Windows task "Nugget Daemon",
the store file `nugget.db`, and the program names `nuggetd` and `nugget-desktop`.

On macOS, data and config share one directory.

## Layout

```text
<config>/                  config.toml (0600), sources.toml (no secrets), probes.json

<data>/
  nugget.db                the encrypted store (SQLCipher), plus nugget.db-wal and nugget.db-shm
  store.key                only when no OS keychain is usable (0600 on Unix)
  ipc/                     nuggetd.sock, nuggetd.lock (one daemon per data directory),
                           desktop-owned.json (the daemon the app started and stops on quit)
  logs/                    daemon (JSON lines, 14 days), desktop, llama-server and launchd logs
  models/                  content-addressed model files, staging, slots.json, runtimes/
  runtime/                 pid files of running local model servers
  plugins/, plugin-catalog/ installed WebAssembly plugins and the plugin catalog
  vault/                   the vault, only with --home or HEED_HOME (see below)
```

## The vault

The knowledge graph is rendered as Markdown into the vault, outside the data directory:

| Install | Vault |
|---|---|
| Normal | `~/Nugget Vault` |
| With `HEED_HOME=DIR` or `--home DIR` | `DIR/data/vault` |
| `[vault] path` set | That directory (see [config.toml](config.md#vault)) |

Pages are `<Kind>/<Name> (<id>).md`; `.nugget/` holds merge bases and conflict sidecars. Pages are
written atomically and never through a symlink. The vault is **plain, unencrypted text**: keep it out of
synced folders if that matters to you.

## The store

`nugget.db` is one SQLCipher (AES-256) database: items and chunks with FTS5 and `sqlite-vec` indexes,
sync cursors, the egress ledger, the hash-chained audit log, the knowledge graph, and MCP clients
(keyed token hashes, never tokens). Its 256-bit key is in the OS keychain (service
`app.heed.Heed`), or in `store.key` when no keychain is usable or `HEED_KEYCHAIN=off`.

Not in the store: **credentials** (OS keychain only; with `HEED_KEYCHAIN=off`, source credentials
are held in memory and lost at exit) and **your original files** (read in place, never modified).

## Backups

Use `nuggetd backup` ([Backup and export](../guide/backup.md)). For a manual copy, stop the daemon and
copy the data and config directories **and** the store key (keychain entry or `store.key`). Models can
be re-downloaded; the vault can be re-rendered.
