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
  vault/                   the vault, only with --home or NUGGET_HOME (see below)
```

Status: being wired in (wave W). Installed WebAssembly plugins will live in `<data>/plugins/`
(the store in `nugget-plugin` already uses that directory), and the agent's hash-chained action log
will also be kept under the data directory.

## The vault

The knowledge graph is rendered as Markdown into the **vault**, outside the data directory so you
can open it in any editor:

| Install | Vault |
|---|---|
| Normal | `~/Nugget Vault` |
| With `NUGGET_HOME=DIR` or `--home DIR` | `DIR/data/vault` |
| `[vault] path` set | That directory (see [config.toml](config.md#vault)) |

```text
<vault>/
  Person/Ada Example (ent_000042).md    one page per entity, under its kind
  Organization/…, Project/…, Topic/…
  .nugget/                              merge bases, conflict sidecars and the page manifest
```

The daemon writes pages atomically, never through a symlink, and watches the vault for your edits.
`[vault] enabled = false` turns rendering and watching off. The vault is plain, unencrypted text: it
is a readable copy of facts that are also in the store, so keep it out of folders you sync if that
matters to you.

## What is in the store

`nugget.db` is one SQLCipher (AES-256) database. It holds:

- items (one per source document) and their chunks, with full-text (FTS5) and vector (`sqlite-vec`)
  indexes;
- per-source sync cursors;
- the egress ledger;
- the hash-chained audit log;
- the knowledge graph tables (entities, claims, evidence, mentions, a change feed) and the
  extraction progress;
- MCP clients (names, scopes and keyed hashes of their tokens, never the tokens) and the key those
  hashes use;
- metadata such as the embedding dimension and the vault's render position.

The key is 256 random bits, kept in the OS keychain (service `dev.nugget.Nugget`), or in `store.key`
when no keychain is usable or `NUGGET_KEYCHAIN=off`.

## What is not in it

- **Credentials**: in the OS keychain only (see [Connector settings](connectors.md) for the account
  names). With `NUGGET_KEYCHAIN=off`, source credentials are held in memory and lost at exit.
- **Your original files**: Nugget reads them in place and never modifies them.

## Backups

Copy the data and config directories while the daemon is stopped. The store is useless without its key:
back up the keychain entry too, or the `store.key` file if you use one, and keep it as safe as the data
itself. Model files can be re-downloaded and do not need a backup. The vault is rendered from the
store, and edits you made in it are already merged into the store as facts you asserted, so it can be
rebuilt; back it up anyway if you keep your own notes in it.
