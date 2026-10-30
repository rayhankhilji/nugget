# Backup, restore and export

Everything Nugget knows lives in one encrypted database on your computer. This page explains how to
keep a copy of it, how to bring it back (on the same computer or a new one, on any OS), how to take
everything out in open formats, and what Nugget does if the database is ever damaged.

## Backups

Open **Status → Backup and export** in the app (or use `nuggetd backup` on the command line).

1. **Set a backup passphrase.** Pick something you will remember, at least 8 characters. Nugget
   uses it to lock a copy of the database key that goes into every backup.
2. **Pick a folder.** An external drive, a NAS share or a folder your own cloud-sync tool watches all
   work. The backup is encrypted before it is written, so the folder does not need to be private.
3. **Pick a schedule** (manual, daily or weekly) and **how many backups to keep** (default 7; older
   ones are deleted after a new one succeeds).

Then press **Back up now**, or wait for the schedule. Nugget keeps working while it backs up.

### What is in a backup

Each backup is a folder named `nugget-backup-<date and time>` holding three files:

| File | What it is |
|---|---|
| `store.nbk` | A consistent copy of the whole store (items, search indexes, the knowledge graph, the audit log and egress ledger), plus your `config.toml`, your backup settings and the Markdown vault. Encrypted with AES-256 (SQLCipher) under the store key. |
| `key.json` | The store key, encrypted under your backup passphrase (Argon2id, then XChaCha20-Poly1305). |
| `manifest.json` | When, from which Nugget version and OS, how many items, and a SHA-256 of each file. No secrets and no content. |

The copy is taken with SQLite's own online copy, never by copying the live file, so it is consistent
even while sources are syncing. A folder without `manifest.json` is an unfinished backup and is
ignored.

**Not in a backup:** account passwords and sign-in tokens for your sources (Gmail, Microsoft 365,
Slack and so on). They stay in your OS keychain and never leave it. After restoring on a new
computer, reconnect those sources from the **Sources** page; their items are already there, and the
next sync continues from where the backup left off. Downloaded models are not included either; the
app downloads them again.

### The key, and what happens if you lose it

The database is encrypted with a random 256-bit key held by your OS keychain (or a `0600` key file
when no keychain is usable). A backup can be opened in two ways:

- **On the same computer**, with the key that is already in the keychain. No passphrase needed.
- **Anywhere else**, with your backup passphrase, which unlocks the copy of the key in `key.json`.

If you forget the passphrase **and** lose the computer (or its keychain), your backups cannot be
opened. Nobody can recover them: not you, not the Nugget project, not anyone else. There is no
master key and no reset. Write the passphrase down somewhere safe.

Changing the passphrase applies to backups made from then on; older backups still open with the
passphrase they were made with.

## Restore

In **Status → Backup and export**, pick a backup from the list and press **Restore…**, or run:

```console
nuggetd restore --from "/Volumes/Backup/nugget-backup-20261030T091500Z"
```

(stop the daemon first; the command refuses to restore under a running one). On a computer where the
key is not in the keychain you are asked for the backup passphrase; `--passphrase-file` or the
`NUGGET_BACKUP_PASSPHRASE` environment variable work for scripts (on Windows the command does not
prompt, so use one of them). On the command line, `nuggetd backup set-passphrase` sets the
passphrase and `nuggetd backup --to DIR` backs up into another folder once.

Before anything is replaced, Nugget checks the backup: the SHA-256 of every file, that the key
opens it, SQLite's integrity check, the audit log's hash chain, and that the search and vector
indexes agree with the items. A backup that fails any check is not restored.

Your current data is **never deleted**. The database in use is renamed to
`nugget.db.pre-restore-<date and time>` next to the original, and if the restored backup has a
different key, the old key is kept in `store.key.replaced-<date and time>` so that copy stays
readable. Delete them yourself once you are happy with the restore.

When you restore in the app, Nugget restarts itself to finish. A restored audit log is older than
the one it replaces, so Nugget records the restore in the audit log and re-anchors the log's head;
this is why the audit check does not report the rollback as tampering.

Backups restore across operating systems: a backup made on macOS restores on Windows or Linux and
the other way round. File-source folders point at paths on the old computer; edit or re-add those
sources on the **Sources** page.

## Export everything

**Export everything** (or `nuggetd export --to DIR`) writes all your data in open formats to a new
folder `nugget-export-<date and time>`:

| File | Format |
|---|---|
| `items.jsonl` | One JSON object per line per item: ids, source, title, dates, URL, the source's metadata, and the full text. |
| `graph.json` | The knowledge graph: entities with aliases and identifiers, claims with their evidence, and mentions. |
| `vault/` | The Markdown vault, one page per entity. |
| `audit.csv` | The audit log, with each entry's hash chain fields. |
| `egress.csv` | The egress ledger: every request that left this computer. |

The export streams, so it works for hundreds of thousands of items without needing much memory.

**An export is not encrypted.** It is meant for taking your data elsewhere; keep it somewhere safe,
and delete it when you are done.

## If the database is damaged

At every start Nugget checks the database before using it. If the check fails (for example after a
disk error), Nugget:

1. **does not write to it**: it starts with an empty, temporary store so the app still opens;
2. **keeps the file exactly as it was**, so nothing more is lost;
3. shows a banner in the app offering to **restore from your latest backup**.

Restoring moves the damaged file aside (`nugget.db.corrupt-<date and time>`), never deletes it. If you
have no backup, keep the damaged file and ask for help on the issue tracker: SQLite can often recover
most of a damaged database.

## Crashes and power loss

Nugget writes through SQLite's write-ahead log with every multi-row change in one transaction, so a
crash or power cut loses at most the change that was being made, never earlier ones, and indexing
picks up where it stopped without duplicates. This is tested in CI by killing the daemon hundreds of
times at random points while it indexes, extracts knowledge and writes the vault, and checking the
database, the audit chain and the indexes after every kill.
