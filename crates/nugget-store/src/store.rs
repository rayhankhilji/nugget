//! `Store`: connection lifecycle, encryption, cursors and counts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use nugget_core::ids::SourceId;
use nugget_core::{Error, Result};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::egress::EgressWriter;
use crate::error::{DbExt, format_time};
use crate::key::{StoreKey, create_private_dir};
use crate::types::Counts;
use crate::{schema, vec_ext};

/// Handle to the encrypted store. Cheap to clone; all clones share its connections.
///
/// Writes (and reads inside write transactions) are serialized on one writer connection behind
/// a mutex. A file-backed store also keeps a small pool of read-only connections, so plain reads
/// run concurrently with each other and with the writer (WAL); an in-memory store reads through
/// its writer connection.
///
/// Egress events recorded through [`EgressSink`](nugget_core::egress::EgressSink) are written by
/// a dedicated thread so async callers never wait on SQLite. Dropping the last clone writes every
/// queued event before it returns.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    db: Arc<Db>,
    /// Owns the ledger writer thread; dropping it drains the queue and joins the thread.
    egress: EgressWriter,
    /// Audit chain MAC key and head anchor.
    audit: crate::audit::AuditState,
}

/// Read-only connections kept by a file-backed store: the most reads that run at once.
const READ_POOL_SIZE: usize = 4;

/// The connections, shared by [`Store`] clones and the egress writer thread.
pub(crate) struct Db {
    conn: Mutex<Connection>,
    /// `None` for in-memory stores (a second connection would open a different database).
    readers: Option<ReadPool>,
    encrypted: bool,
    /// Database path and key of a file-backed store (for online backups, which open their own
    /// connection); `None` for in-memory stores.
    file: Option<(PathBuf, StoreKey)>,
    /// Held while the approximate vector index is built or dropped (see `vector::build_ann`),
    /// so one runs at a time. Shared with the build thread, which holds `Db` only weakly.
    pub(crate) ann_build: Arc<Mutex<()>>,
}

impl Db {
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        // A panic while holding the lock cannot leave SQLite inconsistent (open transactions are
        // rolled back when dropped), so a poisoned mutex is safe to reuse.
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut conn)
    }

    /// Run a read on a pooled read-only connection (waiting for a free one), or on the writer
    /// connection for in-memory stores. Each statement sees every write committed before it
    /// started. Never call this from inside [`Db::with_conn`]: a read that must see the
    /// writer's uncommitted changes belongs on the writer connection.
    pub(crate) fn with_read_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        match &self.readers {
            Some(pool) => f(&pool.take()),
            None => self.with_conn(|c| f(c)),
        }
    }
}

/// Idle read-only connections; a reader takes one and puts it back when done.
struct ReadPool {
    idle: Mutex<Vec<Connection>>,
    returned: Condvar,
}

impl ReadPool {
    fn open(path: &Path, key: &StoreKey) -> Result<Self> {
        let idle = (0..READ_POOL_SIZE)
            .map(|_| open_reader(path, key))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
        })
    }

    fn take(&self) -> PooledConn<'_> {
        // Poisoning is harmless here: the vector is only pushed to and popped from.
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(conn) = idle.pop() {
                return PooledConn {
                    pool: self,
                    conn: Some(conn),
                };
            }
            idle = self
                .returned
                .wait(idle)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A connection borrowed from a [`ReadPool`], returned on drop (also when the read panics: a
/// read-only connection holds no state worth discarding once its statements are dropped).
struct PooledConn<'a> {
    pool: &'a ReadPool,
    conn: Option<Connection>,
}

impl std::ops::Deref for PooledConn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("connection is present until drop")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(conn);
            self.pool.returned.notify_one();
        }
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("encrypted", &self.inner.db.encrypted)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open (or create) the SQLCipher database at `path`, keyed with `key`, and migrate it.
    ///
    /// Returns `Error::Integrity` if the file cannot be decrypted with `key` (wrong key, or not a
    /// Nugget database), and `Error::Unsupported` if the linked SQLite lacks SQLCipher or the
    /// schema is newer than this build.
    pub fn open(path: impl AsRef<Path>, key: &StoreKey) -> Result<Self> {
        let path = path.as_ref();
        vec_ext::ensure_registered()?;
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let conn = open_keyed(path, key, OpenFlags::SQLITE_OPEN_CREATE)?;

        // WAL for concurrent readers; temp_store=MEMORY so SQLite never spills plaintext temp
        // files to disk.
        conn.pragma_update(None, "journal_mode", "WAL")
            .db("enable WAL")?;
        set_common_pragmas(&conn)?;
        restrict_file_permissions(path);
        Self::finish(conn, Some((path, key)))
    }

    /// An unencrypted, in-memory store (tests and tooling).
    pub fn open_in_memory() -> Result<Self> {
        vec_ext::ensure_registered()?;
        let conn = Connection::open_in_memory().db("open in-memory store")?;
        set_common_pragmas(&conn)?;
        Self::finish(conn, None)
    }

    /// Migrate, then build the shared state. `file` is the database path and key of a
    /// file-backed (encrypted) store, which gets a read pool; `None` for an in-memory one.
    fn finish(mut conn: Connection, file: Option<(&Path, &StoreKey)>) -> Result<Self> {
        schema::migrate(&mut conn)?;
        // Opened after migrating, so readers start on the current schema.
        let readers = file
            .map(|(path, key)| ReadPool::open(path, key))
            .transpose()?;
        let db = Arc::new(Db {
            conn: Mutex::new(conn),
            readers,
            encrypted: file.is_some(),
            file: file.map(|(path, key)| (path.to_path_buf(), key.clone())),
            ann_build: Arc::default(),
        });
        let egress = EgressWriter::spawn(db.clone())?;
        let audit = crate::audit::AuditState::ephemeral()?;
        let store = Self {
            inner: Arc::new(Inner { db, egress, audit }),
        };
        if let Some((_, key)) = file {
            crate::audit::attach_store_key(&store, key)?;
        }
        Ok(store)
    }

    /// Whether the database file is encrypted (false only for [`Store::open_in_memory`]).
    pub fn is_encrypted(&self) -> bool {
        self.inner.db.encrypted
    }

    /// Current schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<u32> {
        self.with_read_conn(schema::user_version)
    }

    /// Run any pending migrations; returns how many were applied (0 when current).
    pub fn migrate(&self) -> Result<u32> {
        self.with_conn(schema::migrate)
    }

    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.inner.db.with_conn(f)
    }

    /// A read on a pooled read-only connection; see [`Db::with_read_conn`].
    pub(crate) fn with_read_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.inner.db.with_read_conn(f)
    }

    /// Database path and key of a file-backed store; `None` for an in-memory one.
    pub(crate) fn file(&self) -> Option<(&Path, &StoreKey)> {
        self.inner.db.file.as_ref().map(|(p, k)| (p.as_path(), k))
    }

    /// The shared connections (for work that must outlive a borrowed `Store`, holding them
    /// weakly).
    pub(crate) fn db(&self) -> &Arc<Db> {
        &self.inner.db
    }

    pub(crate) fn egress_writer(&self) -> &EgressWriter {
        &self.inner.egress
    }

    pub(crate) fn audit_state(&self) -> &crate::audit::AuditState {
        &self.inner.audit
    }

    /// Number of items and chunks.
    pub fn counts(&self) -> Result<Counts> {
        self.with_read_conn(|c| {
            let (items, chunks): (i64, i64) = c
                .query_row(
                    "SELECT (SELECT count(*) FROM items), (SELECT count(*) FROM chunks)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .db("count rows")?;
            Ok(Counts {
                items: items as u64,
                chunks: chunks as u64,
            })
        })
    }

    /// The sync cursor stored for `source`, if any.
    pub fn get_cursor(&self, source: &SourceId) -> Result<Option<Value>> {
        self.get_cursor_json(source)?
            .map(|s| serde_json::from_str(&s).map_err(Error::from))
            .transpose()
    }

    /// The sync cursor stored for `source` as its JSON text, for callers that deserialize a
    /// large cursor straight into their own types (no [`Value`] tree in between).
    pub fn get_cursor_json(&self, source: &SourceId) -> Result<Option<String>> {
        self.with_read_conn(|c| {
            c.query_row(
                "SELECT value FROM cursors WHERE source = ?1",
                [source.as_str()],
                |r| r.get(0),
            )
            .optional()
            .db("read cursor")
        })
    }

    /// Store (replace) the sync cursor for `source`.
    pub fn set_cursor(&self, source: &SourceId, value: Value) -> Result<()> {
        self.set_cursor_from(source, &value)
    }

    /// Store (replace) the sync cursor for `source`, serialized from `value` directly (no
    /// [`Value`] tree in between). Read it back with [`Store::get_cursor`] or
    /// [`Store::get_cursor_json`].
    pub fn set_cursor_from<T: serde::Serialize + ?Sized>(
        &self,
        source: &SourceId,
        value: &T,
    ) -> Result<()> {
        let json = serde_json::to_string(value)?;
        let now = format_time(time::OffsetDateTime::now_utc())?;
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO cursors (source, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (source) DO UPDATE SET value = excluded.value,
                                                    updated_at = excluded.updated_at",
                params![source.as_str(), json, now],
            )
            .db("write cursor")?;
            Ok(())
        })
    }

    /// Forget the sync cursor of `source`, so its next pass starts from scratch (a full
    /// backfill, reconciled against stored item versions). Items are kept. Returns whether a
    /// cursor existed.
    pub fn clear_cursor(&self, source: &SourceId) -> Result<bool> {
        self.with_conn(|c| {
            let n = c
                .execute("DELETE FROM cursors WHERE source = ?1", [source.as_str()])
                .db("clear cursor")?;
            Ok(n > 0)
        })
    }

    /// Embedding dimension fixed for this database, once the first embedding was stored.
    pub fn embedding_dim(&self) -> Result<Option<usize>> {
        self.with_read_conn(crate::items::embedding_dim)
    }
}

/// Open the database at `path` (read-write, plus `extra` flags), check that SQLite has SQLCipher,
/// and key it, failing with `Error::Integrity` when `key` does not decrypt it.
pub(crate) fn open_keyed(path: &Path, key: &StoreKey, extra: OpenFlags) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | extra,
    )
    .db("open store")?;
    apply_key(&conn, key, path)?;
    Ok(conn)
}

/// Check that SQLite has SQLCipher and key `conn` (opened on `path`, named in errors), failing
/// with `Error::Integrity` when `key` does not decrypt it.
pub(crate) fn apply_key(conn: &Connection, key: &StoreKey, path: &Path) -> Result<()> {
    set_key(conn, key)?;
    // The key is only checked on first read of the header.
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::NotADatabase => {
            Err(Error::Integrity(format!(
                "cannot decrypt store at {}: wrong key or not a Nugget database",
                path.display()
            )))
        }
        Err(e) => Err(Error::Storage(format!("read store header: {e}"))),
    }
}

/// Check that SQLite has SQLCipher and set `key` on `conn`, without reading the database yet
/// (so a wrong key is only noticed by the first read).
pub(crate) fn set_key(conn: &Connection, key: &StoreKey) -> Result<()> {
    // Fail closed if this SQLite has no SQLCipher: `PRAGMA key` would silently do nothing.
    let cipher: Option<String> = conn
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .optional()
        .db("query cipher version")?;
    if cipher.as_deref().is_none_or(str::is_empty) {
        return Err(Error::Unsupported(
            "SQLite was built without SQLCipher; refusing to open an unencrypted store".into(),
        ));
    }

    // SQLCipher logs decryption failures to stderr by default; we report them as errors.
    conn.execute_batch("PRAGMA cipher_log_level = NONE;")
        .db("configure SQLCipher logging")?;

    // Raw 256-bit key (x'..' form): no KDF needed, the key is already uniformly random.
    // Built in place in a zeroized buffer of its final size, so no copy of the hex key is left
    // behind in a temporary or a reallocated buffer.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut pragma = Zeroizing::new(String::with_capacity(24 + 64));
    pragma.push_str("PRAGMA key = \"x'");
    for b in key.as_bytes() {
        pragma.push(char::from(HEX[usize::from(b >> 4)]));
        pragma.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    pragma.push_str("'\";");
    conn.execute_batch(&pragma).db("set store key")
}

/// A read-only (`query_only`) connection to the existing, migrated, WAL-mode database at `path`.
fn open_reader(path: &Path, key: &StoreKey) -> Result<Connection> {
    let conn = open_keyed(path, key, OpenFlags::empty())?;
    set_common_pragmas(&conn)?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .db("make reader connection read-only")?;
    Ok(conn)
}

pub(crate) fn set_common_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA busy_timeout = 5000;
         PRAGMA cache_size = -32768;",
    )
    .db("configure connection")
}

/// Owner-only permissions on the database and its WAL/SHM files (Unix). Best effort: the content
/// is encrypted regardless.
pub(crate) fn restrict_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut names = vec![path.as_os_str().to_owned()];
        for suffix in ["-wal", "-shm"] {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            names.push(p);
        }
        for name in names {
            let p = Path::new(&name);
            if p.exists()
                && let Err(e) = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
            {
                tracing::warn!(path = %p.display(), error = %e, "cannot restrict store file permissions");
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::{NewChunk, NewItem, content_hash};

    fn file_store(dir: &tempfile::TempDir) -> Store {
        Store::open(
            dir.path().join("nugget.db"),
            &StoreKey::from_bytes([7u8; 32]),
        )
        .unwrap()
    }

    fn cursor(c: &Connection, source: &str) -> Result<String> {
        c.query_row(
            "SELECT value FROM cursors WHERE source = ?1",
            [source],
            |r| r.get(0),
        )
        .db("read cursor")
    }

    /// Two reads are inside the database at the same time while another thread holds the
    /// writer connection, and both see the last committed write.
    #[test]
    fn reads_run_concurrently_while_the_writer_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let s = file_store(&dir);
        let src = SourceId::new("files:test");
        s.set_cursor(&src, json!({"n": 1})).unwrap();

        s.with_conn(|_writer| {
            let (entered_tx, entered) = mpsc::channel();
            let mut releases = Vec::new();
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    let (release, wait) = mpsc::channel::<()>();
                    releases.push(release);
                    let (s, entered_tx) = (s.clone(), entered_tx.clone());
                    std::thread::spawn(move || {
                        s.with_read_conn(|c| {
                            entered_tx.send(()).unwrap();
                            // Stay inside the read until released (or the test gave up).
                            let _ = wait.recv_timeout(Duration::from_secs(10));
                            cursor(c, "files:test")
                        })
                    })
                })
                .collect();
            let both_in = (0..2).all(|_| entered.recv_timeout(Duration::from_secs(10)).is_ok());
            drop(releases);
            assert!(both_in, "reads did not run concurrently beside the writer");
            for r in readers {
                assert_eq!(r.join().unwrap().unwrap(), r#"{"n":1}"#);
            }
            Ok(())
        })
        .unwrap();
    }

    /// Pooled reads see every committed write, and pooled connections cannot write.
    #[test]
    fn reads_see_committed_writes_and_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let s = file_store(&dir);
        let src = SourceId::new("files:test");
        for n in 0..(READ_POOL_SIZE * 2) {
            s.set_cursor(&src, json!({"n": n})).unwrap();
            assert_eq!(s.get_cursor(&src).unwrap(), Some(json!({"n": n})));
        }

        let item = NewItem::new("files:test", "a.txt", content_hash("body")).with_text("body");
        s.upsert_item_with_chunks(&item, &[NewChunk::embedded("body", vec![1.0, 0.0])])
            .unwrap();
        assert!(s.get_item(&item.item).unwrap().is_some());
        assert_eq!(s.counts().unwrap().chunks, 1);
        assert_eq!(s.embedding_dim().unwrap(), Some(2));
        let chunk = s.get_chunks_by_ord(&item.item, &[0]).unwrap()[0].id;
        assert_eq!(s.get_embeddings(&[chunk]).unwrap()[&chunk], vec![1.0, 0.0]);

        let write =
            s.with_read_conn(|c| c.execute("DELETE FROM cursors", []).db("write on a reader"));
        assert!(write.is_err(), "reader connection accepted a write");
        assert!(s.get_cursor(&src).unwrap().is_some());
    }
}
