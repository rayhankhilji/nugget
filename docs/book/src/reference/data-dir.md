# Data directory

`nuggetd` keeps everything in three per-user directories: **data**, **config** and **cache**.

## Where they are

| OS | Data | Config | Cache |
|---|---|---|---|
| Linux | `~/.local/share/nugget` (or `$XDG_DATA_HOME/nugget`) | `~/.config/nugget` (or `$XDG_CONFIG_HOME/nugget`) | `~/.cache/nugget` (or `$XDG_CACHE_HOME/nugget`) |
| macOS | `~/Library/Application Support/dev.Nugget.Nugget` | `~/Library/Application Support/dev.Nugget.Nugget` | `~/Library/Caches/dev.Nugget.Nugget` |
| Windows | `%LOCALAPPDATA%\Nugget\Nugget\data` | `%APPDATA%\Nugget\Nugget\config` | `%LOCALAPPDATA%\Nugget\Nugget\cache` |
| With `NUGGET_HOME=DIR` or `--home DIR` | `DIR/data` | `DIR/config` | `DIR/cache` |

These names come from before the app was renamed from Nugget to Heed, and are kept so an update
finds your existing data: the folders above, the vault folder `~/Nugget Vault`, the keychain
service `dev.nugget.Nugget`, the Windows task "Nugget Daemon" and the `NUGGET_*` environment
variables.

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
  vault/                   the vault, only with --home or NUGGET_HOME (see below)
```

## The vault

The knowledge graph is rendered as Markdown into the vault, outside the data directory:

| Install | Vault |
|---|---|
| Normal | `~/Nugget Vault` |
| With `NUGGET_HOME=DIR` or `--home DIR` | `DIR/data/vault` |
| `[vault] path` set | That directory (see [config.toml](config.md#vault)) |

Pages are `<Kind>/<Name> (<id>).md`; `.nugget/` holds merge bases and conflict sidecars. Pages are
written atomically and never through a symlink. The vault is **plain, unencrypted text**: keep it out of
synced folders if that matters to you.

## The store

`nugget.db` is one SQLCipher (AES-256) database: items and chunks with FTS5 and `sqlite-vec` indexes,
sync cursors, the egress ledger, the hash-chained audit log, the knowledge graph, and MCP clients
(keyed token hashes, never tokens). Its 256-bit key is in the OS keychain (service
`dev.nugget.Nugget`), or in `store.key` when no keychain is usable or `NUGGET_KEYCHAIN=off`.

Not in the store: **credentials** (OS keychain only; with `NUGGET_KEYCHAIN=off`, source credentials
are held in memory and lost at exit) and **your original files** (read in place, never modified).

## Backups

Use `nuggetd backup` ([Backup and export](../guide/backup.md)). For a manual copy, stop the daemon and
copy the data and config directories **and** the store key (keychain entry or `store.key`). Models can
be re-downloaded; the vault can be re-rendered.
