//! Online backups: a consistent, encrypted copy of the live database plus the settings and vault
//! files a restore needs, in one SQLCipher file.
//!
//! [`Store::backup_into`] copies with `VACUUM INTO` on a connection of its own. SQLCipher writes
//! the copy encrypted with the source's key (checked after each copy: a plaintext header fails
//! the backup). The copy runs inside one read transaction of that connection: in WAL mode a
//! reader never blocks the writer, so ingest keeps going during a backup and the copy is the
//! snapshot taken when it started. (Running it on the writer connection would block every write
//! for the whole copy; a pooled reader is `query_only`, which `VACUUM INTO` refuses, and would
//! take one of the few pooled connections away from searches.) The only cost is that WAL
//! checkpoints cannot move past that snapshot until the copy ends, so the WAL may grow
//! meanwhile; the next checkpoint after the copy catches up.
//!
//! The copy then gets one extra table, `nugget_backup_files (path, content)`, holding files
//! (settings, vault) so they are encrypted with the data. A restore reads them with
//! [`read_backup_files`], drops the table with [`strip_backup_files`] and opens the file as a
//! store.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use nugget_core::{Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use unicode_normalization::UnicodeNormalization;

use crate::Store;
use crate::error::DbExt;
use crate::integrity::{ReadOnlyDb, sidecar};
use crate::key::{StoreKey, create_private_dir, sync_dir, sync_file};
use crate::store::{open_keyed, restrict_file_permissions, set_common_pragmas};

/// The table carrying files in a backup database.
const FILES_TABLE: &str = "nugget_backup_files";

/// What a backup holds (counted on the finished copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackupStats {
    /// Size of the backup file in bytes.
    pub bytes: u64,
    /// Items.
    pub items: u64,
    /// Chunks.
    pub chunks: u64,
    /// Live entities (not merged away, not pruned).
    pub entities: u64,
    /// Audit log entries.
    pub audit_entries: u64,
}

impl Store {
    /// Write an online, consistent, encrypted copy of this store to `dest` (which must not
    /// exist; its directory is created owner-only if missing), with `files` (`(relative path,
    /// content)`) stored in its `nugget_backup_files` table. File paths must be safe to write
    /// out on every OS (see [`read_backup_files`]).
    ///
    /// `dest` is first created exclusively (`O_EXCL`, `0600` on Unix), so an existing file or a
    /// planted symlink fails the backup instead of being followed. The copy is encrypted with
    /// this store's key, verified with `PRAGMA integrity_check`, and synced to disk before this
    /// returns; on any failure the files this call created are removed. Writers are not blocked
    /// meanwhile (see the module docs). `Error::Unsupported` for an in-memory store,
    /// `Error::Invalid` for an existing `dest` or a bad file path.
    pub fn backup_into(&self, dest: &Path, files: &[(String, Vec<u8>)]) -> Result<BackupStats> {
        let Some((path, key)) = self.file() else {
            return Err(Error::Unsupported(
                "an in-memory store cannot be backed up".into(),
            ));
        };
        validate_file_set(files.iter().map(|(name, _)| name.as_str())).map_err(Error::Invalid)?;
        let dest_str = dest.to_str().ok_or_else(|| {
            Error::Invalid(format!(
                "backup destination {} is not valid UTF-8",
                dest.display()
            ))
        })?;
        if let Some(dir) = dest.parent() {
            create_private_dir(dir)?;
        }
        let journal = sidecar(dest, "-journal");
        let journal_existed = fs::symlink_metadata(&journal).is_ok();
        create_exclusive(dest)?;
        let result = copy_store(path, key, dest_str).and_then(|()| finish_backup(dest, key, files));
        if result.is_err() {
            let _ = fs::remove_file(dest);
            if !journal_existed {
                let _ = fs::remove_file(&journal);
            }
        }
        result
    }

    /// After a deliberate restore of an older copy, make this log's current head the audit
    /// anchor, so [`Store::verify_audit_chain`] passes and appends are anchored again. The
    /// restore should append an audit entry (such as `backup.restore`) right after.
    ///
    /// The log's chain, head and seal must verify (ignoring the anchor), else
    /// `Error::Integrity` and nothing changes. Without `force`, the anchor is only replaced when
    /// it shows a genuine rollback: it names an entry **after** the log's last one. It is
    /// refused (`Error::Integrity`) when the anchored entry is in the log with a different hash
    /// (the log was rewritten, not rolled back), when the anchor is unreadable or not written
    /// for this key, and when the log is empty while an anchor exists. `force` replaces the
    /// anchor in all those cases (an explicit user decision). An anchor that already matches is
    /// just moved to the head. The new value is written durably before this returns (for an
    /// empty log, a value that anchors "no entries yet"). Without a configured anchor this only
    /// verifies the log.
    pub fn reset_audit_anchor(&self, force: bool) -> Result<()> {
        crate::audit::reset_anchor(self, force)
    }
}

/// Create `path` empty and exclusively (fails if anything, even a dangling symlink, is there),
/// owner-only.
fn create_exclusive(path: &Path) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(_) => {
            crate::acl::restrict_file(path);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::Invalid(format!(
            "backup destination {} already exists",
            path.display()
        ))),
        Err(e) => Err(e.into()),
    }
}

/// `VACUUM INTO` from a dedicated connection to the live database (see the module docs).
fn copy_store(path: &Path, key: &StoreKey, dest: &str) -> Result<()> {
    let src = open_keyed(path, key, OpenFlags::empty())?;
    set_common_pragmas(&src)?;
    src.execute("VACUUM INTO ?1", [dest])
        .db("copy store into backup")?;
    Ok(())
}

/// Add the files to the copy, verify and count it, and make it durable.
fn finish_backup(dest: &Path, key: &StoreKey, files: &[(String, Vec<u8>)]) -> Result<BackupStats> {
    restrict_file_permissions(dest);
    let mut head = [0u8; 16];
    {
        use std::io::Read;
        fs::File::open(dest)?.read_exact(&mut head)?;
    }
    if &head == b"SQLite format 3\0" {
        return Err(Error::Integrity(
            "the backup copy was written unencrypted; refusing to keep it".into(),
        ));
    }
    let mut conn = open_keyed(dest, key, OpenFlags::empty())?;
    conn.pragma_update(None, "journal_mode", "DELETE")
        .db("set backup journal mode")?;
    conn.execute_batch("PRAGMA synchronous = FULL; PRAGMA temp_store = MEMORY;")
        .db("configure backup")?;
    let tx = conn.transaction().db("begin backup files")?;
    tx.execute_batch(&format!(
        "CREATE TABLE {FILES_TABLE} (path TEXT PRIMARY KEY, content BLOB NOT NULL);"
    ))
    .db("create backup files table")?;
    {
        let mut ins = tx
            .prepare(&format!(
                "INSERT INTO {FILES_TABLE} (path, content) VALUES (?1, ?2)"
            ))
            .db("prepare backup file insert")?;
        for (name, content) in files {
            ins.execute(params![name, content])
                .db("store file in backup")?;
        }
    }
    tx.commit().db("commit backup files")?;

    let rows = {
        let mut stmt = conn.prepare("PRAGMA integrity_check").db("verify backup")?;
        stmt.query_map([], |r| r.get::<_, String>(0))
            .db("verify backup")?
            .take(20)
            .collect::<rusqlite::Result<Vec<_>>>()
            .db("verify backup")?
    };
    if rows != ["ok"] {
        return Err(Error::Integrity(format!(
            "backup copy failed its integrity check: {}",
            rows.join("; ")
        )));
    }
    let mut stats = counts(&conn)?;
    drop(conn);
    sync_file(dest)?;
    if let Some(dir) = dest.parent() {
        sync_dir(dir);
    }
    stats.bytes = fs::metadata(dest)?.len();
    Ok(stats)
}

fn counts(c: &Connection) -> Result<BackupStats> {
    c.query_row(
        "SELECT (SELECT count(*) FROM items), (SELECT count(*) FROM chunks),
                (SELECT count(*) FROM entities WHERE merged_into IS NULL AND deleted_at IS NULL),
                (SELECT count(*) FROM audit)",
        [],
        |r| {
            Ok(BackupStats {
                bytes: 0,
                items: r.get::<_, i64>(0)? as u64,
                chunks: r.get::<_, i64>(1)? as u64,
                entities: r.get::<_, i64>(2)? as u64,
                audit_entries: r.get::<_, i64>(3)? as u64,
            })
        },
    )
    .db("count backup")
}

/// Windows device names, reserved with any extension and in any case.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A file path as stored in a backup: relative and `/`-separated, and writable as is on every
/// OS: no empty, `.` or `..` segments, no backslash, colon, `<>"|?*` or control characters, no
/// segment ending in `.` or a space, no Windows device name (`CON`, `com1.txt`, ...).
fn validate_file_path(name: &str) -> std::result::Result<(), String> {
    let bad = |why: &str| Err(format!("backup file path {name:?} is not allowed: {why}"));
    if name.is_empty() {
        return bad("empty");
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return bad("contains a backslash, colon, one of <>\"|?* or a control character");
    }
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return bad("empty, `.` or `..` segment, or absolute");
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return bad("a segment ends with `.` or a space");
        }
        let stem = seg.split('.').next().unwrap_or(seg).trim_end();
        if RESERVED_NAMES.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
            return bad("a segment is a Windows device name");
        }
    }
    if !Path::new(name)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return bad("not a plain relative path");
    }
    Ok(())
}

/// How a path compares on case-insensitive, normalising file systems (macOS, Windows): NFC,
/// lowercased.
fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// [`validate_file_path`] for each path, and no two paths that would land on the same file
/// (equal after [`fold`]) or where one is a file and the other needs it as a directory.
fn validate_file_set<'a>(names: impl Iterator<Item = &'a str>) -> std::result::Result<(), String> {
    let mut folded = HashSet::new();
    let mut all = Vec::new();
    for name in names {
        validate_file_path(name)?;
        let f = fold(name);
        if !folded.insert(f.clone()) {
            return Err(format!(
                "backup file path {name:?} collides with another path (paths differing only in \
                 case or Unicode normalisation land on the same file)"
            ));
        }
        all.push((name, f));
    }
    for (name, f) in &all {
        for (i, _) in f.match_indices('/') {
            if folded.contains(&f[..i]) {
                return Err(format!(
                    "backup file path {name:?} needs {:?} as a directory, but it is a file",
                    &f[..i]
                ));
            }
        }
    }
    Ok(())
}

/// Read the files stored in the backup database at `path` (opened read-only with `key`, never
/// written), sorted by path. `Error::Integrity` when `key` does not open it, it is not a backup
/// (no files table), or a stored path is unsafe to write out on any OS: absolute, `..`,
/// reserved names or characters (see `validate_file_path`), or two paths that land on the same
/// file of a case-insensitive file system, or that need a file as a directory.
pub fn read_backup_files(path: &Path, key: &StoreKey) -> Result<Vec<(String, Vec<u8>)>> {
    if !path.exists() {
        return Err(Error::NotFound(format!("backup file {}", path.display())));
    }
    let db = ReadOnlyDb::open_verified(path, key)?;
    let exists: Option<i64> = db
        .conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [FILES_TABLE],
            |r| r.get(0),
        )
        .optional()
        .db("look up backup files")?;
    if exists.is_none() {
        return Err(Error::Integrity(format!(
            "{} is not a Nugget backup (it holds no backup files)",
            path.display()
        )));
    }
    let mut stmt = db
        .conn
        .prepare(&format!(
            "SELECT path, content FROM {FILES_TABLE} ORDER BY path"
        ))
        .db("read backup files")?;
    let files = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .db("read backup files")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .db("read backup files")?;
    validate_file_set(files.iter().map(|(name, _)| name.as_str())).map_err(Error::Integrity)?;
    Ok(files)
}

/// Turn a verified backup file into a live store file in place: drop its `nugget_backup_files`
/// table (a no-op when it has none). Read the files first with [`read_backup_files`]. The file
/// can then be opened with [`Store::open`] and the same key. `Error::Integrity` when `key` does
/// not open it.
pub fn strip_backup_files(path: &Path, key: &StoreKey) -> Result<()> {
    if !path.exists() {
        return Err(Error::NotFound(format!("backup file {}", path.display())));
    }
    crate::vec_ext::ensure_registered()?;
    let conn = open_keyed(path, key, OpenFlags::empty())?;
    conn.execute_batch(&format!(
        "PRAGMA synchronous = FULL; PRAGMA temp_store = MEMORY;
         DROP TABLE IF EXISTS {FILES_TABLE};"
    ))
    .db("drop backup files")?;
    drop(conn);
    sync_file(path)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A populated encrypted store, shared by the backup and integrity tests.

    use std::sync::{Arc, Mutex};

    use nugget_graph::{EntityKind, GraphStore, Mention, NewEntity};

    use crate::{AuditAnchor, AuditEntry, NewChunk, NewItem, Store, StoreKey, content_hash};

    pub(crate) const DIM: usize = 8;

    pub(crate) fn key() -> StoreKey {
        StoreKey::from_bytes([7u8; 32])
    }

    pub(crate) fn embedding(seed: usize) -> Vec<f32> {
        (0..DIM)
            .map(|d| (((seed * 31 + d * 17) % 23) as f32 - 11.0) / 7.0 + 0.01)
            .collect()
    }

    pub(crate) fn put_item(s: &Store, n: usize) {
        let text = format!("note {n} about the quarterly ledger and harbour {}", n % 7);
        let item = NewItem::new("files:test", format!("{n}.txt"), content_hash(&text))
            .with_title(format!("Note {n}"))
            .with_text(text.clone());
        s.upsert_item_with_chunks(
            &item,
            &[
                NewChunk::embedded(text.clone(), embedding(n * 2)),
                NewChunk::embedded(format!("second part {n} lighthouse"), embedding(n * 2 + 1)),
            ],
        )
        .unwrap();
    }

    /// Items with embedded chunks, a small graph, audit entries and a cursor.
    pub(crate) fn populate(s: &Store, items: usize) {
        for n in 0..items {
            put_item(s, n);
        }
        let g = s.graph();
        let mut e = NewEntity::new(EntityKind::Person, "Samantha Lee");
        e.aliases.push("Sam".into());
        let id = g.create_entity(&e).unwrap();
        g.create_entity(&NewEntity::new(EntityKind::Organization, "Harbour Co"))
            .unwrap();
        g.add_mention(&Mention {
            entity: id,
            item: crate::items::item_ref("files:test".into(), "0.txt".into()),
            chunk_id: None,
            surface: "Sam".into(),
            confidence: 0.9,
        })
        .unwrap();
        for n in 0..3 {
            s.append_audit(&AuditEntry {
                actor: "test".into(),
                action: format!("action.{n}"),
                target: "store".into(),
                detail: serde_json::json!({ "n": n }),
            })
            .unwrap();
        }
        s.set_cursor(
            &nugget_core::ids::SourceId::new("files:test"),
            serde_json::json!({"page": 3}),
        )
        .unwrap();
    }

    pub(crate) fn audit(s: &Store, action: &str) {
        s.append_audit(&AuditEntry {
            actor: "test".into(),
            action: action.into(),
            target: "store".into(),
            detail: serde_json::json!({}),
        })
        .unwrap();
    }

    /// An anchor kept in memory (shareable between stores, like one keychain entry).
    #[derive(Default, Clone)]
    pub(crate) struct MemAnchor(pub(crate) Arc<Mutex<Option<String>>>);

    impl AuditAnchor for MemAnchor {
        fn load(&self) -> std::result::Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn store(&self, value: &str) -> std::result::Result<(), String> {
            *self.0.lock().unwrap() = Some(value.to_owned());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use nugget_graph::GraphStore;

    use super::test_support::*;
    use super::*;
    use crate::integrity::{FileHealth, check_file};

    fn files() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "config/config.toml".into(),
                b"[sources]\nfiles = true\n".to_vec(),
            ),
            ("config/backup.json".into(), br#"{"keep":7}"#.to_vec()),
            (
                "vault/People/Sam.md".into(),
                "# Sam\n\nnaïve café".as_bytes().to_vec(),
            ),
            ("vault/empty.md".into(), Vec::new()),
        ]
    }

    /// Backup, read the files back, strip, open: the same data, searches, graph and audit log.
    #[test]
    fn backup_round_trip_restores_everything() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 30);
        // Build the approximate index, so it has to survive the copy too.
        // (Hybrid search first: on a store this small its automatic mode drops that index.)
        let q = embedding(5);
        let hybrid = s.search_hybrid("ledger", Some(&q), 10).unwrap();
        let approx = s.search_vector_approx(&q, 5).unwrap();
        assert!(s.ann_index_ready().unwrap());
        let exact = s.search_vector_exact(&q, 5).unwrap();
        let text = s.search_text("lighthouse", 10).unwrap();

        let dest = dir.path().join("out").join("store.nbk");
        let stats = s.backup_into(&dest, &files()).unwrap();
        assert_eq!(
            (
                stats.items,
                stats.chunks,
                stats.entities,
                stats.audit_entries
            ),
            (30, 60, 2, 3)
        );
        assert_eq!(stats.bytes, fs::metadata(&dest).unwrap().len());
        assert!(!sidecar(&dest, "-journal").exists());
        assert!(!sidecar(&dest, "-wal").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "backup readable by others: {mode:o}");
        }
        assert_eq!(
            check_file(&dest, &key(), true).unwrap(),
            FileHealth::Healthy
        );

        let mut want = files();
        want.sort();
        assert_eq!(read_backup_files(&dest, &key()).unwrap(), want);
        strip_backup_files(&dest, &key()).unwrap();
        assert!(matches!(
            read_backup_files(&dest, &key()),
            Err(Error::Integrity(_))
        ));

        let r = Store::open(&dest, &key()).unwrap();
        assert_eq!(r.counts().unwrap(), s.counts().unwrap());
        assert!(r.ann_index_ready().unwrap());
        assert_eq!(r.search_vector_approx(&q, 5).unwrap(), approx);
        assert_eq!(r.search_vector_exact(&q, 5).unwrap(), exact);
        assert_eq!(r.search_text("lighthouse", 10).unwrap(), text);
        let report = r.check_consistency().unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!(r.search_hybrid("ledger", Some(&q), 10).unwrap(), hybrid);
        assert_eq!(r.graph().counts().unwrap(), s.graph().counts().unwrap());
        assert_eq!(r.recent_audit(10).unwrap(), s.recent_audit(10).unwrap());
        r.verify_audit_chain().unwrap();
        let report = r.check_consistency().unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!(
            r.get_cursor(&nugget_core::ids::SourceId::new("files:test"))
                .unwrap(),
            Some(serde_json::json!({"page": 3}))
        );
        // The restored store is live: writes and audit appends work.
        put_item(&r, 100);
        audit(&r, "after.restore");
        r.verify_audit_chain().unwrap();
        let report = r.check_consistency().unwrap();
        assert!(report.ok, "{:?}", report.problems);
    }

    /// A backup taken while another thread writes is a consistent snapshot.
    #[test]
    fn backup_while_another_thread_writes_is_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 50);
        let before = s.counts().unwrap().items;
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (s, stop) = (s.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 1000;
                while !stop.load(Ordering::Relaxed) || n < 1020 {
                    put_item(&s, n);
                    if n % 5 == 0 {
                        audit(&s, "concurrent");
                    }
                    n += 1;
                }
                n
            })
        };
        // Let the writer get going, then take several backups during its writes.
        while s.counts().unwrap().items < before + 5 {
            std::thread::yield_now();
        }
        let mut stats = Vec::new();
        for i in 0..3 {
            let dest = dir.path().join(format!("b{i}.nbk"));
            stats.push((dest.clone(), s.backup_into(&dest, &files()).unwrap()));
        }
        stop.store(true, Ordering::Relaxed);
        let last = writer.join().unwrap();
        let after = s.counts().unwrap().items;
        assert_eq!(after, before + (last - 1000) as u64);

        for (dest, st) in stats {
            assert!(st.items >= before + 5 && st.items <= after, "{st:?}");
            assert_eq!(
                st.chunks,
                st.items * 2,
                "snapshot split an item from its chunks"
            );
            strip_backup_files(&dest, &key()).unwrap();
            let r = Store::open(&dest, &key()).unwrap();
            let report = r.check_consistency().unwrap();
            assert!(report.ok, "{:?}", report.problems);
            assert_eq!((report.items, report.chunks), (st.items, st.chunks));
            r.verify_audit_chain().unwrap();
        }
        // The live store was never disturbed.
        assert!(s.check_consistency().unwrap().ok);
    }

    #[test]
    fn backup_is_unreadable_without_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 5);
        let dest = dir.path().join("store.nbk");
        s.backup_into(&dest, &files()).unwrap();

        let bytes = fs::read(&dest).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3\0"));
        for needle in ["quarterly ledger", "naïve café", "Samantha"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "plaintext {needle:?} in the backup"
            );
        }
        let plain = Connection::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(
            plain
                .query_row("SELECT count(*) FROM sqlite_master", [], |r| r
                    .get::<_, i64>(0))
                .is_err()
        );
        let other = StoreKey::from_bytes([8u8; 32]);
        assert!(matches!(
            read_backup_files(&dest, &other),
            Err(Error::Integrity(_))
        ));
        assert!(matches!(
            strip_backup_files(&dest, &other),
            Err(Error::Integrity(_))
        ));
        assert!(matches!(
            Store::open(&dest, &other),
            Err(Error::Integrity(_))
        ));
        assert_eq!(
            check_file(&dest, &other, false).unwrap(),
            FileHealth::WrongKey
        );
        assert_eq!(
            read_backup_files(&dest, &key()).unwrap().len(),
            files().len()
        );
    }

    #[test]
    fn backup_refuses_bad_destinations_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 2);
        let dest = dir.path().join("store.nbk");
        fs::write(&dest, b"keep me").unwrap();
        assert!(matches!(s.backup_into(&dest, &[]), Err(Error::Invalid(_))));
        assert_eq!(fs::read(&dest).unwrap(), b"keep me");

        let fresh = dir.path().join("fresh.nbk");
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "vault/../../x",
            "a//b",
            "a\\b",
            "C:/x",
            "./a",
        ] {
            let err = s
                .backup_into(&fresh, &[(bad.to_owned(), vec![1])])
                .unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{bad:?}: {err}");
            assert!(!fresh.exists());
        }
        // A failure leaves nothing behind (duplicate path).
        let dup = vec![("a".to_owned(), vec![1]), ("a".to_owned(), vec![2])];
        assert!(s.backup_into(&fresh, &dup).is_err());
        assert!(!fresh.exists());
        assert!(!sidecar(&fresh, "-journal").exists());

        let mem = Store::open_in_memory().unwrap();
        assert!(matches!(
            mem.backup_into(&dir.path().join("m.nbk"), &[]),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            read_backup_files(&dir.path().join("missing.nbk"), &key()),
            Err(Error::NotFound(_))
        ));
    }

    /// Restoring an older copy leaves the anchor ahead of the log; resetting it makes the
    /// restored log verify and anchor again.
    #[test]
    fn reset_audit_anchor_after_restoring_an_older_copy() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = MemAnchor::default();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        s.set_audit_anchor(Some(Arc::new(anchor.clone())));
        populate(&s, 3); // audit entries 1..=3
        let dest = dir.path().join("old.nbk");
        s.backup_into(&dest, &[]).unwrap();
        audit(&s, "later.1");
        audit(&s, "later.2"); // the anchor is now at entry 5
        s.verify_audit_chain().unwrap();
        drop(s);

        strip_backup_files(&dest, &key()).unwrap();
        let r = Store::open(&dest, &key()).unwrap();
        r.set_audit_anchor(Some(Arc::new(anchor.clone())));
        let err = r.verify_audit_chain().unwrap_err();
        assert!(
            matches!(&err, Error::Integrity(m) if m.contains("rolled back")),
            "{err}"
        );
        // Appending does not overwrite the conflicting anchor (it is evidence)...
        audit(&r, "restored.0");
        assert!(
            anchor
                .0
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .starts_with("v1:5:")
        );
        assert!(r.verify_audit_chain().is_err());

        // ...until the restore is acknowledged.
        r.reset_audit_anchor(false).unwrap();
        assert!(
            anchor
                .0
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .starts_with("v1:4:")
        );
        r.verify_audit_chain().unwrap();
        audit(&r, "restored.1");
        assert!(
            anchor
                .0
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .starts_with("v1:5:")
        );
        r.verify_audit_chain().unwrap();

        // A new store handle on the same anchor agrees.
        drop(r);
        let r = Store::open(&dest, &key()).unwrap();
        r.set_audit_anchor(Some(Arc::new(anchor.clone())));
        r.verify_audit_chain().unwrap();
    }

    #[test]
    fn reset_audit_anchor_refuses_a_log_that_does_not_verify() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = MemAnchor::default();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        s.set_audit_anchor(Some(Arc::new(anchor.clone())));
        populate(&s, 1);
        let before = anchor.0.lock().unwrap().clone();
        s.with_conn(|c| {
            c.execute(
                "UPDATE meta SET value = '2:' || (SELECT hash FROM audit WHERE seq = 2)
                 WHERE key = 'audit_head'",
                [],
            )
            .db("tamper")
        })
        .unwrap();
        assert!(matches!(
            s.reset_audit_anchor(false),
            Err(Error::Integrity(_))
        ));
        assert_eq!(*anchor.0.lock().unwrap(), before);

        // Without an anchor it only verifies.
        let fresh = Store::open(dir.path().join("other.db"), &key()).unwrap();
        fresh.reset_audit_anchor(false).unwrap();
    }

    /// The anchor's current value (panics when nothing is stored).
    fn anchored(anchor: &MemAnchor) -> String {
        anchor.0.lock().unwrap().clone().unwrap()
    }

    #[test]
    fn reset_audit_anchor_on_an_empty_log_needs_force_and_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = MemAnchor::default();
        // An anchor left by another (longer) log of the same key.
        let s = Store::open(dir.path().join("a.db"), &key()).unwrap();
        s.set_audit_anchor(Some(Arc::new(anchor.clone())));
        populate(&s, 1);
        drop(s);
        let before = anchored(&anchor);
        let r = Store::open(dir.path().join("b.db"), &key()).unwrap();
        r.set_audit_anchor(Some(Arc::new(anchor.clone())));
        assert!(r.verify_audit_chain().is_err());
        assert!(matches!(
            r.reset_audit_anchor(false),
            Err(Error::Integrity(_))
        ));
        assert_eq!(anchored(&anchor), before);

        r.reset_audit_anchor(true).unwrap();
        assert!(anchored(&anchor).starts_with("v1:0:"));
        r.verify_audit_chain().unwrap();
        // Durable: a new handle (a restart) verifies before anything is appended.
        drop(r);
        let r = Store::open(dir.path().join("b.db"), &key()).unwrap();
        r.set_audit_anchor(Some(Arc::new(anchor.clone())));
        r.verify_audit_chain().unwrap();
        audit(&r, "first");
        assert!(anchored(&anchor).starts_with("v1:1:"));
        r.verify_audit_chain().unwrap();
    }

    /// A log rewritten under the same key (the anchored entry is there with another hash) is
    /// not blessed without `force`.
    #[test]
    fn reset_audit_anchor_refuses_a_rewritten_log_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = MemAnchor::default();
        let s = Store::open(dir.path().join("a.db"), &key()).unwrap();
        s.set_audit_anchor(Some(Arc::new(anchor.clone())));
        for n in 0..3 {
            audit(&s, &format!("original.{n}"));
        }
        drop(s);
        let before = anchored(&anchor);
        assert!(before.starts_with("v1:3:"));
        // Another log of the same key, just as long: entry 3 differs.
        let r = Store::open(dir.path().join("b.db"), &key()).unwrap();
        for n in 0..4 {
            audit(&r, &format!("rewritten.{n}"));
        }
        r.set_audit_anchor(Some(Arc::new(anchor.clone())));
        assert!(r.verify_audit_chain().is_err());
        let err = r.reset_audit_anchor(false).unwrap_err();
        assert!(
            matches!(&err, Error::Integrity(m) if m.contains("rewritten")),
            "{err}"
        );
        assert_eq!(anchored(&anchor), before);

        r.reset_audit_anchor(true).unwrap();
        assert!(anchored(&anchor).starts_with("v1:4:"));
        r.verify_audit_chain().unwrap();

        // An anchor that is not this key's (or garbage) also needs force.
        *anchor.0.lock().unwrap() = Some("v1:2:abc:def".into());
        assert!(matches!(
            r.reset_audit_anchor(false),
            Err(Error::Integrity(_))
        ));
        r.reset_audit_anchor(true).unwrap();
        r.verify_audit_chain().unwrap();
        // A matching anchor is simply moved to the head.
        audit(&r, "more");
        r.reset_audit_anchor(false).unwrap();
        assert!(anchored(&anchor).starts_with("v1:5:"));
    }

    #[test]
    fn file_paths_are_validated() {
        for ok in [
            "config/config.toml",
            "vault/a b/ç.md",
            "x",
            "vault/console.md",
            "vault/.hidden",
        ] {
            assert!(validate_file_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/a",
            "a/",
            "a/../b",
            "..",
            ".",
            "a\0b",
            "c:x",
            "a\\b",
            "vault/CON",
            "vault/con.md",
            "Nul.txt",
            "a/com1",
            "LPT9.x.y",
            "a./b",
            "a /b",
            "trailing.",
            "trailing ",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\u{1}b",
            "a\nb",
        ] {
            assert!(validate_file_path(bad).is_err(), "{bad:?}");
        }
        let set = |names: &[&str]| validate_file_set(names.iter().copied());
        assert!(set(&["vault/a.md", "vault/b.md", "config/config.toml"]).is_ok());
        assert!(set(&["vault/A.md", "vault/a.md"]).is_err(), "case");
        assert!(
            set(&["vault/caf\u{e9}.md", "vault/cafe\u{301}.md"]).is_err(),
            "NFC"
        );
        assert!(
            set(&["vault/x", "vault/x/y.md"]).is_err(),
            "file used as a directory"
        );
        assert!(
            set(&["vault/X", "vault/x/y.md"]).is_err(),
            "file used as a directory, case"
        );
        assert!(set(&["vault/x.md", "vault/x.md"]).is_err(), "duplicate");
    }

    /// Both directions enforce the same rules: a backup carrying a bad set (written past
    /// `backup_into`, e.g. by a crafted file) is refused on read.
    #[test]
    fn unsafe_paths_in_a_backup_are_refused_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 1);
        assert!(matches!(
            s.backup_into(
                &dir.path().join("x.nbk"),
                &[("vault/A.md".into(), vec![]), ("vault/a.md".into(), vec![])]
            ),
            Err(Error::Invalid(_))
        ));
        for bad in [
            vec!["vault/AUX.md"],
            vec!["vault/A.md", "vault/a.md"],
            vec!["../x"],
        ] {
            let dest = dir.path().join(format!("b{}.nbk", bad.len()));
            let _ = fs::remove_file(&dest);
            s.backup_into(&dest, &[]).unwrap();
            let c = open_keyed(&dest, &key(), OpenFlags::empty()).unwrap();
            for name in &bad {
                c.execute(
                    &format!("INSERT INTO {FILES_TABLE} (path, content) VALUES (?1, x'00')"),
                    [name],
                )
                .unwrap();
            }
            drop(c);
            assert!(
                matches!(read_backup_files(&dest, &key()), Err(Error::Integrity(_))),
                "{bad:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn backup_does_not_follow_a_planted_symlink_or_remove_files_it_did_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        populate(&s, 2);
        let target = dir.path().join("victim");
        fs::write(&target, b"precious").unwrap();
        let dest = dir.path().join("store.nbk");
        std::os::unix::fs::symlink(&target, &dest).unwrap();
        assert!(matches!(s.backup_into(&dest, &[]), Err(Error::Invalid(_))));
        assert_eq!(fs::read(&target).unwrap(), b"precious");
        assert!(
            fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // Dangling symlink too.
        let dangling = dir.path().join("dangling.nbk");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        assert!(s.backup_into(&dangling, &[]).is_err());
        assert!(!dir.path().join("nowhere").exists());

        // A failing backup removes its own file but not a journal that was already there.
        let fresh = dir.path().join("fresh.nbk");
        let journal = sidecar(&fresh, "-journal");
        fs::write(&journal, b"someone else's").unwrap();
        let dup = vec![("a".to_owned(), vec![1]), ("A".to_owned(), vec![2])];
        assert!(s.backup_into(&fresh, &dup).is_err());
        let dup = vec![("a".to_owned(), vec![1]), ("a/b".to_owned(), vec![2])];
        assert!(s.backup_into(&fresh, &dup).is_err());
        assert!(!fresh.exists());
        assert_eq!(fs::read(&journal).unwrap(), b"someone else's");
        // The new file is owner-only from the start.
        let ok = dir.path().join("ok.nbk");
        s.backup_into(&ok, &[]).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&ok).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
