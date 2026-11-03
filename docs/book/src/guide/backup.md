# Backup, restore and export

Everything Heed knows lives in one encrypted database on your computer. Back it up, restore it on
any computer and OS, or export it in open formats.

## Backups

Open **Status → Backup and export** (or use `nuggetd backup`):

1. **Set a backup passphrase** (at least 8 characters). It locks a copy of the database key inside
   every backup.
2. **Pick a folder**: an external drive, NAS or cloud-synced folder is fine, since backups are
   encrypted. Folders inside Heed's own data, settings or vault folders are refused.
3. **Pick a schedule** (manual, daily or weekly) and **how many to keep** (default 7). Only this
   computer's backups are ever deleted, so several computers can share a folder.

Then press **Back up now** or wait for the schedule.

Each backup is an encrypted `nugget-backup-<date and time>` folder holding the store, your
`config.toml` and the vault.

**Not included:** source passwords and sign-in tokens (they stay in your OS keychain; reconnect those
sources after restoring on a new computer) and downloaded models (downloaded again).

**If you forget the passphrase and lose the computer, your backups cannot be opened by anyone.**
There is no master key. On the same computer, a backup opens with the key already in the keychain.
Changing the passphrase affects only new backups.

## Restore

Pick a backup in **Status → Backup and export** and press **Restore…**, or stop the daemon and run:

```console
nuggetd restore --from "/Volumes/Backup/nugget-backup-20261030T091500Z"
```

You are asked for the passphrase when the key isn't in this computer's keychain; for scripts (and
always on Windows) use `--passphrase-file` or `HEED_BACKUP_PASSPHRASE`. `nuggetd backup
set-passphrase` sets the passphrase and `nuggetd backup --to DIR` makes a one-off backup elsewhere.

- The backup is fully verified first; a backup that fails any check is not restored.
- Your current data is **never deleted**: it is kept as `nugget.db.pre-restore-<date and time>` (and
  its key as `store.key.replaced-<date and time>` if different). Delete them when you are happy.
- Restoring in the app restarts Heed. An interrupted restore is finished at the next start.
- Across operating systems: re-point file-source folders on the **Sources** page. A vault path that
  isn't valid on the new OS is commented out, and a restored `config.toml` that won't load is kept
  as `config.toml.restored-<date and time>` instead.

## Export everything

**Export everything** (or `nuggetd export --to DIR`) writes a `nugget-export-<date and time>` folder:

| File | Format |
|---|---|
| `items.jsonl` | One JSON object per item: ids, source, title, dates, URL, metadata and full text |
| `graph.json` | The knowledge graph: entities, claims with evidence, mentions |
| `vault/` | The Markdown vault |
| `audit.csv` | The audit log |
| `egress.csv` | The egress ledger |

**An export is not encrypted.** Keep it safe and delete it when you are done.

## If the database is damaged

Heed checks the database at every start (a full check after an unclean shutdown, otherwise in the
background once a day). If it is damaged or this computer's key can't open it, Heed leaves the file
untouched, starts with an empty temporary store (no syncing, no vault updates, no backups), and shows
a banner offering to **restore from your latest backup**. The damaged file is moved aside as
`nugget.db.corrupt-<date and time>`, never deleted. With no backup, keep it and ask on the issue
tracker; SQLite can often recover most of it.
