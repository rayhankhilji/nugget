# Golden backup (format 1)

`nugget-backup-*/` is a real backup made on Linux by `nuggetd`'s own backup code (test
`write_golden_fixture` in `tests/backup.rs`). The test `golden_backup_restores_on_this_os` restores
it on every CI OS, proving a backup made on one OS restores on another.

- Store key: 32 bytes of `0x09` (test-only).
- Backup passphrase: `golden fixture passphrase`.
- Contents: 5 items mentioning "zephyrine", one audit entry, the test's `config.toml`.
- `key.json` is sealed with tiny Argon2id parameters (8 KiB, 1 pass) so the test is fast; real
  backups use 64 MiB and 3 passes.

The files are byte-exact (`-text` in the root `.gitattributes`): editing any of them breaks the
manifest checksums, which is the point. Regenerate only when the format changes:

```console
NUGGET_WRITE_BACKUP_FIXTURE=1 cargo test -p nuggetd --test backup -- --ignored write_golden_fixture
```
