# Data directory

`nuggetd` keeps everything in three per-user directories: **data**, **config** and **cache**.

## Where they are

| OS | Data | Config | Cache |
|---|---|---|---|
| Linux | `~/.local/share/nugget` (or `$XDG_DATA_HOME/nugget`) | `~/.config/nugget` (or `$XDG_CONFIG_HOME/nugget`) | `~/.cache/nugget` (or `$XDG_CACHE_HOME/nugget`) |
| macOS | `~/Library/Application Support/dev.Nugget.Nugget` | `~/Library/Application Support/dev.Nugget.Nugget` | `~/Library/Caches/dev.Nugget.Nugget` |
| Windows | `%LOCALAPPDATA%\Nugget\Nugget\data` | `%APPDATA%\Nugget\Nugget\config` | `%LOCALAPPDATA%\Nugget\Nugget\cache` |
| With `NUGGET_HOME=DIR` or `--home DIR` | `DIR/data` | `DIR/config` | `DIR/cache` |

On macOS, data and config share one directory.

## Layout

```text
<config>/
  config.toml              daemon configuration (reference/config.md); 0600 on Unix
  sources.toml             configured sources and their settings (no secrets)
  probes.json              saved provider capability probes

<data>/
  nugget.db                the encrypted store (SQLCipher), plus nugget.db-wal and nugget.db-shm
  store.key                only when no OS keychain is usable (0600 on Unix)
  ipc/
    nuggetd.sock           the IPC socket (macOS, Linux)
    nuggetd.lock           held while a daemon runs; one daemon per data directory
  logs/
    nuggetd.<date>.log     daemon log, JSON lines, one file per day, 14 kept
    llama-server-chat.log, llama-server-embedding.log
                           local model server output (API keys redacted)
    launchd.log            service output on macOS
  models/
    blobs/sha256/<hex>     installed model files, content-addressed
    staging/               downloads in progress (wiped on hash mismatch)
    slots.json             active and previous version of each model
    runtimes/              verified llama-server builds
  runtime/
    chat.pid, embedding.pid
                           running local model servers (orphans are cleaned up at start)
```

The **vault** (rendered Markdown pages of the knowledge graph) is not written by the daemon yet; its
location will be configurable when it is.

## What is in the store

`nugget.db` is one SQLCipher (AES-256) database. It holds:

- items (one per source document) and their chunks, with full-text (FTS5) and vector (`sqlite-vec`)
  indexes;
- per-source sync cursors;
- the egress ledger;
- the hash-chained audit log;
- the knowledge graph tables (entities, claims, evidence, mentions, a change feed);
- metadata such as the embedding dimension.

The key is 256 random bits, kept in the OS keychain (service `dev.nugget.Nugget`), or in `store.key`
when no keychain is usable or `NUGGET_KEYCHAIN=off`.

## What is not in it

- **Credentials**: in the OS keychain only (see [Connector settings](connectors.md) for the account
  names). With `NUGGET_KEYCHAIN=off`, source credentials are held in memory and lost at exit.
- **Your original files**: Nugget reads them in place and never modifies them.

## Backups

Copy the data and config directories while the daemon is stopped. The store is useless without its key:
back up the keychain entry too, or the `store.key` file if you use one, and keep it as safe as the data
itself. Model files can be re-downloaded and do not need a backup.
