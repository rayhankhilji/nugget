//! `Store`: connection lifecycle, encryption, cursors and counts.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

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

/// Handle to the encrypted store. Cheap to clone; all clones share one connection.
///
/// Calls are serialized on an internal mutex. M0 traffic (one daemon, light writes) does not
/// need more; a reader pool can be added behind the same API later.
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
}

/// The connection, shared by [`Store`] clones and the egress writer thread.
pub(crate) struct Db {
    conn: Mutex<Connection>,
    encrypted: bool,
}

impl Db {
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        // A panic while holding the lock cannot leave SQLite inconsistent (open transactions are
        // rolled back when dropped), so a poisoned mutex is safe to reuse.
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut conn)
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
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .db("open store")?;

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
        let pragma = Zeroizing::new(format!(
            "PRAGMA key = \"x'{}'\";",
            hex::encode(key.as_bytes())
        ));
        conn.execute_batch(&pragma).db("set store key")?;

        // The key is only checked on first read of the header.
        match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::NotADatabase => {
                return Err(Error::Integrity(format!(
                    "cannot decrypt store at {}: wrong key or not a Nugget database",
                    path.display()
                )));
            }
            Err(e) => return Err(Error::Storage(format!("read store header: {e}"))),
        }

        // WAL for concurrent readers; temp_store=MEMORY so SQLite never spills plaintext temp
        // files to disk.
        conn.pragma_update(None, "journal_mode", "WAL")
            .db("enable WAL")?;
        set_common_pragmas(&conn)?;
        restrict_file_permissions(path);
        Self::finish(conn, true)
    }

    /// An unencrypted, in-memory store (tests and tooling).
    pub fn open_in_memory() -> Result<Self> {
        vec_ext::ensure_registered()?;
        let conn = Connection::open_in_memory().db("open in-memory store")?;
        set_common_pragmas(&conn)?;
        Self::finish(conn, false)
    }

    fn finish(mut conn: Connection, encrypted: bool) -> Result<Self> {
        schema::migrate(&mut conn)?;
        let db = Arc::new(Db {
            conn: Mutex::new(conn),
            encrypted,
        });
        let egress = EgressWriter::spawn(db.clone())?;
        Ok(Self {
            inner: Arc::new(Inner { db, egress }),
        })
    }

    /// Whether the database file is encrypted (false only for [`Store::open_in_memory`]).
    pub fn is_encrypted(&self) -> bool {
        self.inner.db.encrypted
    }

    /// Current schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<u32> {
        self.with_conn(|c| schema::user_version(c))
    }

    /// Run any pending migrations; returns how many were applied (0 when current).
    pub fn migrate(&self) -> Result<u32> {
        self.with_conn(schema::migrate)
    }

    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.inner.db.with_conn(f)
    }

    pub(crate) fn egress_writer(&self) -> &EgressWriter {
        &self.inner.egress
    }

    /// Number of items and chunks.
    pub fn counts(&self) -> Result<Counts> {
        self.with_conn(|c| {
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
        let raw: Option<String> = self.with_conn(|c| {
            c.query_row(
                "SELECT value FROM cursors WHERE source = ?1",
                [source.as_str()],
                |r| r.get(0),
            )
            .optional()
            .db("read cursor")
        })?;
        raw.map(|s| serde_json::from_str(&s).map_err(Error::from))
            .transpose()
    }

    /// Store (replace) the sync cursor for `source`.
    pub fn set_cursor(&self, source: &SourceId, value: Value) -> Result<()> {
        let json = serde_json::to_string(&value)?;
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

    /// Embedding dimension fixed for this database, once the first embedding was stored.
    pub fn embedding_dim(&self) -> Result<Option<usize>> {
        self.with_conn(|c| crate::items::embedding_dim(c))
    }
}

fn set_common_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA busy_timeout = 5000;",
    )
    .db("configure connection")
}

/// Owner-only permissions on the database and its WAL/SHM files (Unix). Best effort: the content
/// is encrypted regardless.
fn restrict_file_permissions(path: &Path) {
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
