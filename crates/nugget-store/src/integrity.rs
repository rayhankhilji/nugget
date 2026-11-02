//! Integrity checks: [`check_file`] looks at a database file without ever writing to it (before
//! opening a store after a crash, or to verify a backup), and [`Store::check_consistency`] checks
//! an open store's derived structures against their sources (crash harness, restore verification).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nugget_core::{Error, Result};
use rusqlite::{Connection, ErrorCode, OpenFlags};

use crate::Store;
use crate::error::DbExt;
use crate::key::{StoreKey, create_private_dir};
use crate::store::{open_keyed, set_common_pragmas, set_key};

/// SQLCipher's page size (the store never changes it): every valid file is a multiple of it.
const PAGE_SIZE: u64 = 4096;
/// First bytes of an unencrypted SQLite database.
const PLAINTEXT_HEADER: &[u8; 16] = b"SQLite format 3\0";
/// Most `integrity_check` lines kept in a report.
const MAX_REPORTED_ROWS: usize = 20;

/// What [`check_file`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileHealth {
    /// No file at the path.
    Missing,
    /// The key decrypts the file and SQLite's check passes.
    Healthy,
    /// The key does not decrypt the file's first page: another key, or (indistinguishably
    /// without the right key) a damaged first page.
    WrongKey,
    /// The file decrypts but is damaged, truncated, empty or not encrypted; the reason says which.
    Corrupt(String),
}

/// Check the database at `path` with `key` **without writing to it**: `PRAGMA quick_check`, or the
/// slower `PRAGMA integrity_check` when `full`.
pub fn check_file(path: &Path, key: &StoreKey, full: bool) -> Result<FileHealth> {
    let pragma = if full {
        "PRAGMA integrity_check"
    } else {
        "PRAGMA quick_check"
    };
    check_with(path, key, Some(pragma))
}

/// [`check_file`] with SQLite's page check `pragma`, or only the cheap checks when `None`.
fn check_with(path: &Path, key: &StoreKey, pragma: Option<&str>) -> Result<FileHealth> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileHealth::Missing),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() {
        return Ok(FileHealth::Corrupt("not a regular file".into()));
    }
    if meta.len() == 0 {
        return Ok(FileHealth::Corrupt("the file is empty".into()));
    }
    if starts_with_plaintext_header(path)? {
        return Ok(FileHealth::Corrupt(
            "the file is an unencrypted SQLite database, not an encrypted store".into(),
        ));
    }
    if meta.len() % PAGE_SIZE != 0 {
        return Ok(FileHealth::Corrupt(format!(
            "the file is truncated ({} bytes is not a whole number of pages)",
            meta.len()
        )));
    }
    if let Some(problem) = wal_problem(&sidecar(path, "-wal"))? {
        return Ok(FileHealth::Corrupt(problem));
    }
    let db = ReadOnlyDb::open(path, key)?;
    match db.first_read() {
        Ok(()) => {}
        Err(e) if is_not_a_database(&e) => return Ok(FileHealth::WrongKey),
        Err(e) if is_transient(&e) => return Err(Error::Storage(format!("check store: {e}"))),
        Err(e) => return Ok(FileHealth::Corrupt(e.to_string())),
    }
    let Some(pragma) = pragma else {
        return Ok(FileHealth::Healthy);
    };
    match check_rows(&db.conn, pragma) {
        Ok(rows) if rows == ["ok"] => {}
        Ok(rows) => return Ok(FileHealth::Corrupt(rows.join("; "))),
        Err(e) if is_transient(&e) => return Err(Error::Storage(format!("check store: {e}"))),
        Err(e) => return Ok(FileHealth::Corrupt(e.to_string())),
    }
    Ok(FileHealth::Healthy)
}

/// Beside the database: when SQLite's full check last passed (Unix seconds, written by
/// [`check_file_in_background`]).
const CHECKED_SUFFIX: &str = ".checked";
/// Beside the database: a background check found damage, so the next open checks in full.
const CHECK_FAILED_SUFFIX: &str = ".check-failed";
/// How often [`check_file_in_background`] re-runs SQLite's full check.
pub const BACKGROUND_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What [`check_file_on_open`] decided about SQLite's page-by-page check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCheck {
    /// It ran before opening (the result is in the [`FileHealth`]).
    Ran,
    /// Skipped at open and due in the background ([`check_file_in_background`]): the last one
    /// is older than [`BACKGROUND_CHECK_INTERVAL`], or there was none.
    Due,
    /// Skipped: it passed within [`BACKGROUND_CHECK_INTERVAL`].
    Recent,
}

/// [`check_file`] for opening the store at startup, which must not grow with the database.
pub fn check_file_on_open(path: &Path, key: &StoreKey) -> Result<(FileHealth, OpenCheck)> {
    // Evidence of an exit without `Store::close_cleanly`: a write-ahead log with content (SQLite
    // syncs the log itself, so unlike the lock file this survives a power cut; a clean close
    // truncates it), or a lock file left behind.
    let suspect = non_empty(&sidecar(path, "-wal"))
        || crate::egress::unclean_close_left(path)
        || sidecar(path, CHECK_FAILED_SUFFIX).exists();
    if suspect {
        let health = check_file(path, key, false)?;
        if health == FileHealth::Healthy {
            record_check(path, true);
        }
        return Ok((health, OpenCheck::Ran));
    }
    let health = check_with(path, key, None)?;
    let recent = last_check(path).is_some_and(|t| {
        SystemTime::now()
            .duration_since(t)
            .is_ok_and(|age| age < BACKGROUND_CHECK_INTERVAL)
    });
    Ok((
        health,
        if recent {
            OpenCheck::Recent
        } else {
            OpenCheck::Due
        },
    ))
}

/// SQLite's `quick_check` of the database at `path` while a store has it open (on its own read-only
/// connection with a small page cache, so it neither blocks writers nor grows the process),
/// stopping early with `Ok(None)` once `cancel` is set.
pub fn check_file_in_background(
    path: &Path,
    key: &StoreKey,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<FileHealth>> {
    let conn = crate::store::open_keyed(path, key, OpenFlags::empty())?;
    conn.execute_batch(
        "PRAGMA query_only = ON; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -2048;
         PRAGMA busy_timeout = 5000;",
    )
    .db("configure check connection")?;
    let stop = cancel.clone();
    conn.progress_handler(10_000, Some(move || stop.load(Ordering::Relaxed)))
        .db("set check progress handler")?;
    let rows = check_rows(&conn, "PRAGMA quick_check");
    drop(conn);
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let health = match rows {
        Ok(rows) if rows == ["ok"] => FileHealth::Healthy,
        Ok(rows) => FileHealth::Corrupt(rows.join("; ")),
        Err(e) if is_transient(&e) => return Err(Error::Storage(format!("check store: {e}"))),
        Err(e) => FileHealth::Corrupt(e.to_string()),
    };
    record_check(path, health == FileHealth::Healthy);
    Ok(Some(health))
}

/// Write `bytes` to `path`, readable by its owner only (as the database itself).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(path)?, bytes)
}

/// When SQLite's full check last passed, if recorded.
fn last_check(path: &Path) -> Option<SystemTime> {
    let text = fs::read_to_string(sidecar(path, CHECKED_SUFFIX)).ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Remember a full check's outcome beside the database.
fn record_check(path: &Path, healthy: bool) {
    let checked = sidecar(path, CHECKED_SUFFIX);
    let failed = sidecar(path, CHECK_FAILED_SUFFIX);
    let result = if healthy {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        write_private(&checked, now.to_string().as_bytes()).and_then(|()| {
            match fs::remove_file(&failed) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        })
    } else {
        let _ = fs::remove_file(&checked);
        write_private(&failed, b"damage found by the background check\n")
    };
    if let Err(e) = result {
        tracing::warn!(db = %path.display(), error = %e, "cannot record the store check");
    }
}

fn starts_with_plaintext_header(path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut f = fs::File::open(path)?;
    let mut read = 0;
    while read < head.len() {
        match f.read(&mut head[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read == head.len() && &head == PLAINTEXT_HEADER)
}

fn is_not_a_database(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::NotADatabase)
}

const WAL_HEADER: usize = 32;
const WAL_FRAME_HEADER: usize = 24;
const WAL_MAGIC_LE: u32 = 0x377f_0682;
const WAL_MAGIC_BE: u32 = 0x377f_0683;
const WAL_VERSION: u32 = 3_007_000;

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// SQLite's WAL checksum over `data` (a multiple of 8 bytes), continuing from `s`.
fn wal_checksum(big_endian: bool, data: &[u8], s: (u32, u32)) -> (u32, u32) {
    let (mut s0, mut s1) = s;
    for w in data.as_chunks::<8>().0 {
        let (x0, x1) = if big_endian {
            (be32(&w[..4]), be32(&w[4..]))
        } else {
            (
                u32::from_le_bytes([w[0], w[1], w[2], w[3]]),
                u32::from_le_bytes([w[4], w[5], w[6], w[7]]),
            )
        };
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
    }
    (s0, s1)
}

/// Read up to `buf.len()` bytes; returns how many were read (fewer only at end of file).
fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Scan the write-ahead log at `wal` (read-only, streamed one frame at a time) for damage that
/// SQLite would silently treat as the end of the log, losing committed transactions.
pub(crate) fn wal_problem(wal: &Path) -> Result<Option<String>> {
    scan_wal(wal).map(|(problem, _)| problem)
}

/// [`wal_problem`], plus what follows the last commit when the log is sound.
fn scan_wal(wal: &Path) -> Result<(Option<String>, WalTail)> {
    use std::io::BufReader;
    let len = match fs::metadata(wal) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, WalTail::None)),
        Err(e) => return Err(e.into()),
    };
    if len == 0 {
        return Ok((None, WalTail::None));
    }
    let mut r = BufReader::new(fs::File::open(wal)?);
    let invalid = |valid_end: u64, why: &str| {
        let problem = format!(
            "write-ahead log has {} bytes of invalid frames after the last valid commit ({why})",
            len - valid_end
        );
        (Some(problem), WalTail::None)
    };
    let mut hdr = [0u8; WAL_HEADER];
    if read_full(&mut r, &mut hdr)? < WAL_HEADER {
        tracing::info!(path = %wal.display(), "write-ahead log ends inside its header (crash tail)");
        return Ok((None, WalTail::Torn(len)));
    }
    let magic = be32(&hdr[0..]);
    let page_size = be32(&hdr[8..]) as usize;
    let big_endian = magic == WAL_MAGIC_BE;
    let header_ok = (magic == WAL_MAGIC_LE || magic == WAL_MAGIC_BE)
        && be32(&hdr[4..]) == WAL_VERSION
        && (512..=65536).contains(&page_size)
        && page_size.is_power_of_two()
        && wal_checksum(big_endian, &hdr[..24], (0, 0)) == (be32(&hdr[24..]), be32(&hdr[28..]));
    if !header_ok {
        if len > WAL_HEADER as u64 {
            return Ok(invalid(0, "damaged header with frames after it"));
        }
        tracing::info!(path = %wal.display(), "write-ahead log header is torn (crash tail)");
        return Ok((None, WalTail::Torn(len)));
    }
    let salts = &hdr[16..24];
    let mut cks = (be32(&hdr[24..]), be32(&hdr[28..]));
    // The checksum stored in the previous frame (valid or not): the seed to validate a frame on
    // its own after the chain broke.
    let mut prev_stored = cks;
    let mut offset = WAL_HEADER as u64;
    let mut last_commit_end = offset;
    // End of the last frame of the unbroken chain, and whether the chain ended at a whole frame
    // from an earlier generation of the log (other salts) with no frame of the current one after.
    let mut chain_end = offset;
    let mut stale_break = false;
    let mut frame = vec![0u8; WAL_FRAME_HEADER + page_size];
    let mut broken = false;
    loop {
        let n = read_full(&mut r, &mut frame)?;
        if n < frame.len() {
            // End of file, possibly inside a partial frame: ignored by SQLite, and harmless
            // unless an earlier check found hidden commits.
            break;
        }
        let fh = &frame[..WAL_FRAME_HEADER];
        let current = &fh[8..16] == salts;
        let commit = be32(&fh[4..]) != 0;
        let stored = (be32(&fh[16..]), be32(&fh[20..]));
        let checksum_from = |seed| {
            let c = wal_checksum(big_endian, &fh[..8], seed);
            wal_checksum(big_endian, &frame[WAL_FRAME_HEADER..], c)
        };
        if !broken {
            if current && checksum_from(cks) == stored {
                cks = stored;
                chain_end = offset + frame.len() as u64;
                if commit {
                    last_commit_end = chain_end;
                }
            } else {
                broken = true;
                stale_break = !current;
            }
        } else if current && commit && checksum_from(prev_stored) == stored {
            return Ok(invalid(
                last_commit_end,
                "a damaged frame hides a later commit",
            ));
        } else if current {
            stale_break = false;
        }
        prev_stored = stored;
        offset += frame.len() as u64;
    }
    let tail = classify_tail(len, offset, chain_end, last_commit_end, stale_break);
    match tail {
        WalTail::None => {}
        WalTail::Stale(bytes) => tracing::debug!(
            path = %wal.display(),
            bytes,
            "write-ahead log ends with frames from before it was last reused (normal)"
        ),
        WalTail::Torn(bytes) => tracing::info!(
            path = %wal.display(),
            bytes,
            "write-ahead log has an uncommitted or torn tail (a crash); SQLite rolls it back"
        ),
    }
    Ok((None, tail))
}

/// What follows the last commit of a sound write-ahead log.
#[derive(Debug, PartialEq, Eq)]
enum WalTail {
    /// Nothing: the log ends at a commit.
    None,
    /// Only whole frames from an earlier generation of the log (other salts).
    Stale(u64),
    /// Frames of an uncommitted transaction, or a torn or partial frame: what a crash (or a
    /// writer caught mid-transaction) leaves.
    Torn(u64),
}

/// Classify the bytes after `last_commit_end` of a `len`-byte log whose whole frames end at
/// `whole_end`, whose chain of valid frames ends at `chain_end`, and whose first invalid frame
/// (if any) had other salts, as did every frame after it (`stale_break`).
fn classify_tail(
    len: u64,
    whole_end: u64,
    chain_end: u64,
    last_commit_end: u64,
    stale_break: bool,
) -> WalTail {
    if len <= last_commit_end {
        WalTail::None
    } else if chain_end == last_commit_end && stale_break && whole_end == len {
        WalTail::Stale(len - last_commit_end)
    } else {
        WalTail::Torn(len - last_commit_end)
    }
}

/// Errors that say nothing about the file's content.
fn is_transient(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// The rows of an `integrity_check`-style pragma (at most [`MAX_REPORTED_ROWS`]).
fn check_rows(conn: &Connection, pragma: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(pragma)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows.take(MAX_REPORTED_ROWS) {
        out.push(row?);
    }
    Ok(out)
}

/// `path` with `suffix` appended to its file name (`-wal`, `-shm`, `-journal`).
pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn non_empty(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

/// A read-only, keyed connection to a database file that never writes to it or beside it (see
/// [`check_file`] for how), plus the temporary copy it may be reading.
pub(crate) struct ReadOnlyDb {
    pub(crate) conn: Connection,
    _copy: Option<TempCopy>,
}

impl ReadOnlyDb {
    /// [`ReadOnlyDb::open`], then read the schema: `Error::Integrity` when `key` does not
    /// decrypt the file or it is damaged.
    pub(crate) fn open_verified(path: &Path, key: &StoreKey) -> Result<Self> {
        let db = Self::open(path, key)?;
        match db.first_read() {
            Ok(()) => Ok(db),
            Err(e) if is_not_a_database(&e) => Err(Error::Integrity(format!(
                "cannot decrypt {}: wrong key or not a Heed database",
                path.display()
            ))),
            Err(e) if is_transient(&e) => {
                Err(Error::Storage(format!("read {}: {e}", path.display())))
            }
            Err(e) => Err(Error::Integrity(format!(
                "{} is damaged: {e}",
                path.display()
            ))),
        }
    }

    /// The first read, which is where SQLCipher checks the key.
    fn first_read(&self) -> rusqlite::Result<()> {
        self.conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|_| ())
    }

    /// Open without reading yet (see [`ReadOnlyDb::first_read`]).
    pub(crate) fn open(path: &Path, key: &StoreKey) -> Result<Self> {
        crate::vec_ext::ensure_registered()?;
        let wal = sidecar(path, "-wal");
        let shm = sidecar(path, "-shm");
        let journal = sidecar(path, "-journal");
        let needs_recovery = non_empty(&journal) || (non_empty(&wal) && !shm.exists());
        if needs_recovery {
            let copy = TempCopy::of(path, &[&wal, &journal])?;
            // Read-write on the private copy, so SQLite can roll back or replay what it needs.
            let conn = Connection::open_with_flags(
                &copy.db,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .db("open store copy")?;
            set_key(&conn, key)?;
            conn.execute_batch("PRAGMA query_only = ON; PRAGMA temp_store = MEMORY;")
                .db("configure store copy")?;
            return Ok(Self {
                conn,
                _copy: Some(copy),
            });
        }
        let params = if wal.exists() && shm.exists() {
            "mode=ro&readonly_shm=1"
        } else {
            "mode=ro&immutable=1"
        };
        let uri = format!("{}?{params}", file_uri(path)?);
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .db("open store read-only")?;
        set_key(&conn, key)?;
        conn.execute_batch(
            "PRAGMA query_only = ON; PRAGMA temp_store = MEMORY; PRAGMA busy_timeout = 5000;",
        )
        .db("configure read-only connection")?;
        Ok(Self { conn, _copy: None })
    }
}

/// A `file:` URI for `path` (made absolute; see [`uri_for`]).
fn file_uri(path: &Path) -> Result<String> {
    let abs = std::path::absolute(path)?;
    let s = abs.to_str().ok_or_else(|| {
        Error::Invalid(format!(
            "store path {} is not valid UTF-8",
            abs.to_string_lossy()
        ))
    })?;
    Ok(uri_for(s, cfg!(windows)))
}

/// A `file:` URI for the absolute path `s`, percent-encoded.
fn uri_for(s: &str, windows: bool) -> String {
    let mut p = s.to_owned();
    if windows {
        p = p.replace('\\', "/");
        if let Some(rest) = p.strip_prefix("//?/UNC/") {
            p = format!("//{rest}");
        } else if let Some(rest) = p.strip_prefix("//?/") {
            p = rest.to_owned();
        }
    }
    let mut out = String::from(if p.starts_with("//") {
        "file://"
    } else if p.starts_with('/') {
        "file:"
    } else {
        "file:///"
    });
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A hidden, owner-only directory beside a database holding a copy of it and its side files
/// (same file system, so no other disk fills up); removed on drop. The copy stays encrypted.
struct TempCopy {
    dir: PathBuf,
    db: PathBuf,
}

impl TempCopy {
    fn of(path: &Path, sidecars: &[&Path]) -> Result<Self> {
        let mut rand = [0u8; 8];
        getrandom::fill(&mut rand)
            .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
        let name = path
            .file_name()
            .map_or_else(|| "store".into(), |n| n.to_string_lossy().into_owned());
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let dir = parent.join(format!(".{name}.check-{}", hex::encode(rand)));
        if dir.exists() {
            return Err(Error::Storage(format!(
                "temporary directory {} already exists",
                dir.display()
            )));
        }
        create_private_dir(&dir)?;
        let copy = Self {
            db: dir.join("store.db"),
            dir,
        };
        fs::copy(path, &copy.db)?;
        for side in sidecars {
            if non_empty(side) {
                let name = side.to_string_lossy();
                let suffix = &name[path.to_string_lossy().len()..];
                fs::copy(side, sidecar(&copy.db, suffix))?;
            }
        }
        Ok(copy)
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.dir) {
            tracing::warn!(path = %self.dir.display(), error = %e, "cannot remove temporary store copy");
        }
    }
}

/// What [`Store::check_consistency`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistencyReport {
    /// No problems.
    pub ok: bool,
    /// One line per problem found.
    pub problems: Vec<String>,
    /// Items in the store.
    pub items: u64,
    /// Chunks in the store.
    pub chunks: u64,
}

/// `(description, query counting offending rows)`, run in one read transaction.
const ROW_CHECKS: &[(&str, &str)] = &[
    (
        "chunks missing from the full-text index (chunks_fts)",
        "SELECT count(*) FROM chunks c
         WHERE NOT EXISTS (SELECT 1 FROM chunks_fts_docsize d WHERE d.id = c.id)",
    ),
    (
        "full-text index rows (chunks_fts) without a chunk",
        "SELECT count(*) FROM chunks_fts_docsize d
         WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.id = d.id)",
    ),
    (
        "items missing from the title index (items_fts)",
        "SELECT count(*) FROM items i
         WHERE NOT EXISTS (SELECT 1 FROM items_fts_docsize d WHERE d.id = i.id)",
    ),
    (
        "title index rows (items_fts) without an item",
        "SELECT count(*) FROM items_fts_docsize d
         WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.id = d.id)",
    ),
    (
        "entity names missing from the name index (entity_names_fts)",
        "SELECT count(*) FROM entity_aliases a
         WHERE NOT EXISTS (SELECT 1 FROM entity_names_fts_docsize d WHERE d.id = a.id)",
    ),
    (
        "name index rows (entity_names_fts) without an entity name",
        "SELECT count(*) FROM entity_names_fts_docsize d
         WHERE NOT EXISTS (SELECT 1 FROM entity_aliases a WHERE a.id = d.id)",
    ),
    (
        "duplicate items for one (source, external id)",
        "SELECT count(*) FROM (SELECT 1 FROM items NOT INDEXED GROUP BY source, key
                               HAVING count(*) > 1)",
    ),
    (
        "chunks of missing items",
        "SELECT count(*) FROM chunks c WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.id = c.item_id)",
    ),
    (
        "item text layouts of missing items",
        "SELECT count(*) FROM item_text t
         WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.id = t.item_id)",
    ),
    (
        "mentions of missing entities",
        "SELECT count(*) FROM mentions m
         WHERE NOT EXISTS (SELECT 1 FROM entities e WHERE e.id = m.entity_id)",
    ),
    (
        "claim evidence of missing claims",
        "SELECT count(*) FROM claim_evidence ev
         WHERE NOT EXISTS (SELECT 1 FROM claims c WHERE c.id = ev.claim_id)",
    ),
    (
        "items without a live entry in the change feed (item_changes)",
        "SELECT count(*) FROM items i
         WHERE NOT EXISTS (SELECT 1 FROM item_changes ch
                           WHERE ch.source = i.source AND ch.key = i.key AND ch.deleted = 0)",
    ),
    (
        "live change feed entries (item_changes) without an item",
        "SELECT count(*) FROM item_changes ch
         WHERE ch.deleted = 0
           AND NOT EXISTS (SELECT 1 FROM items i WHERE i.source = ch.source AND i.key = ch.key)",
    ),
    (
        "change feed (item_changes) head below its number of entries",
        "SELECT coalesce(max(seq), 0) < count(*) FROM item_changes",
    ),
    (
        "per-source counts (source_stats) that differ from the items and chunks",
        "WITH actual AS (
             SELECT i.source, count(*) AS items,
                    coalesce(sum((SELECT count(*) FROM chunks c WHERE c.item_id = i.id)), 0)
                        AS chunks
             FROM items i GROUP BY i.source
         )
         SELECT (SELECT count(*) FROM actual a LEFT JOIN source_stats s ON s.source = a.source
                 WHERE s.items IS NOT a.items OR s.chunks IS NOT a.chunks)
              + (SELECT count(*) FROM source_stats s
                 WHERE (s.items <> 0 OR s.chunks <> 0)
                   AND NOT EXISTS (SELECT 1 FROM actual a WHERE a.source = s.source))",
    ),
    (
        "per-entity counts (entity_stats) that differ from the claims and mentions",
        "WITH actual AS (
             SELECT e.id,
                    (SELECT count(*) FROM claims c
                     WHERE c.subject = e.id AND c.status IN ('active', 'user_asserted'))
                  + (SELECT count(*) FROM claims c
                     WHERE c.object_entity = e.id AND c.subject <> e.id
                       AND c.status IN ('active', 'user_asserted')) AS claims,
                    (SELECT count(*) FROM mentions m WHERE m.entity_id = e.id) AS mentions
             FROM entities e
         )
         SELECT (SELECT count(*) FROM actual a LEFT JOIN entity_stats s ON s.entity_id = a.id
                 WHERE s.claims IS NOT a.claims OR s.mentions IS NOT a.mentions)
              + (SELECT count(*) FROM entity_stats s
                 WHERE (s.claims <> 0 OR s.mentions <> 0)
                   AND NOT EXISTS (SELECT 1 FROM entities e WHERE e.id = s.entity_id))",
    ),
];

/// Checks of the vector tables, run once the embedding dimension is fixed (`chunk_vec` exists).
const VECTOR_CHECKS: &[(&str, &str)] = &[(
    "vectors (chunk_vec) without a chunk",
    "SELECT count(*) FROM chunk_vec_rowids v
     WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.id = v.rowid)",
)];

/// Checks of the approximate vector index, run while it exists (complete or being built).
const ANN_CHECKS: &[(&str, &str)] = &[
    (
        "approximate index vectors (ann_vectors) without a vector in chunk_vec",
        "SELECT count(*) FROM ann_vectors a
         WHERE NOT EXISTS (SELECT 1 FROM chunk_vec_rowids v WHERE v.rowid = a.id)",
    ),
    (
        "approximate index bits (ann_bits) without a vector in chunk_vec",
        "SELECT count(*) FROM ann_bits_rowids b
         WHERE NOT EXISTS (SELECT 1 FROM chunk_vec_rowids v WHERE v.rowid = b.rowid)",
    ),
];

/// Checks of a complete approximate index: it holds exactly the vectors of `chunk_vec`.
const ANN_READY_CHECKS: &[(&str, &str)] = &[
    (
        "vectors missing from the complete approximate index (ann_vectors)",
        "SELECT count(*) FROM chunk_vec_rowids v
         WHERE NOT EXISTS (SELECT 1 FROM ann_vectors a WHERE a.id = v.rowid)",
    ),
    (
        "vectors missing from the complete approximate index (ann_bits)",
        "SELECT count(*) FROM chunk_vec_rowids v
         WHERE NOT EXISTS (SELECT 1 FROM ann_bits_rowids b WHERE b.rowid = v.rowid)",
    ),
];

fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

fn run_checks(c: &Connection, checks: &[(&str, &str)], problems: &mut Vec<String>) {
    for (what, sql) in checks {
        match c.query_row(sql, [], |r| r.get::<_, i64>(0)) {
            Ok(0) => {}
            Ok(n) => problems.push(format!("{what}: {n}")),
            Err(e) => problems.push(format!("cannot check {what}: {e}")),
        }
    }
}

impl Store {
    /// Check the whole store and report every problem found (an `Err` only when the checks cannot
    /// run at all).
    pub fn check_consistency(&self) -> Result<ConsistencyReport> {
        let mut problems = Vec::new();
        if let Err(e) = self.verify_audit_chain() {
            problems.push(e.to_string());
        }
        // A connection of its own: pooled readers keep FTS5 index structures cached across
        // transactions, and `integrity_check` (which includes FTS5's own check) was seen to read
        // them stale there and report corruption that a fresh connection, the writer and every
        // search did not see. It also leaves the pool to searches.
        let (items, chunks) = match self.file() {
            Some((path, key)) => {
                let c = open_keyed(path, key, OpenFlags::empty())?;
                set_common_pragmas(&c)?;
                c.execute_batch("PRAGMA query_only = ON;")
                    .db("make check connection read-only")?;
                check_snapshot(&c, &mut problems)?
            }
            None => self.with_conn(|c| check_snapshot(c, &mut problems))?,
        };
        Ok(ConsistencyReport {
            ok: problems.is_empty(),
            problems,
            items: items as u64,
            chunks: chunks as u64,
        })
    }
}

/// The checks of [`Store::check_consistency`] that read the database, in one read transaction
/// on `c`. Returns the item and chunk counts of that snapshot.
fn check_snapshot(c: &Connection, problems: &mut Vec<String>) -> Result<(i64, i64)> {
    let tx = c.unchecked_transaction().db("begin consistency check")?;
    match check_rows(&tx, "PRAGMA integrity_check") {
        Ok(rows) if rows == ["ok"] => {}
        Ok(rows) => problems.extend(rows.into_iter().map(|r| format!("integrity: {r}"))),
        Err(e) => problems.push(format!("integrity check failed: {e}")),
    }
    match foreign_key_problems(&tx) {
        Ok(p) => problems.extend(p),
        Err(e) => problems.push(format!("foreign key check failed: {e}")),
    }
    run_checks(&tx, ROW_CHECKS, problems);
    match crate::items::embedding_dim(&tx) {
        Ok(None) => {}
        Ok(Some(_)) => match table_exists(&tx, "chunk_vec_rowids") {
            Ok(true) => run_checks(&tx, VECTOR_CHECKS, problems),
            Ok(false) => problems
                .push("the embedding dimension is set but the vector table is missing".into()),
            Err(e) => problems.push(format!("cannot look up the vector table: {e}")),
        },
        Err(e) => problems.push(format!("cannot read the embedding dimension: {e}")),
    }
    match table_exists(&tx, "ann_vectors") {
        Ok(false) => {}
        Ok(true) => {
            run_checks(&tx, ANN_CHECKS, problems);
            match crate::vector::ann_ready(&tx) {
                Ok(true) => run_checks(&tx, ANN_READY_CHECKS, problems),
                Ok(false) => {}
                Err(e) => problems.push(format!("cannot read the approximate index state: {e}")),
            }
        }
        Err(e) => problems.push(format!("cannot look up the approximate index: {e}")),
    }
    let counts = tx
        .query_row(
            "SELECT (SELECT count(*) FROM items), (SELECT count(*) FROM chunks)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .db("count rows")?;
    tx.commit().db("end consistency check")?;
    Ok(counts)
}

/// One line per `PRAGMA foreign_key_check` violation (at most [`MAX_REPORTED_ROWS`]).
fn foreign_key_problems(c: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = c.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt.query_map([], |r| {
        Ok(format!(
            "foreign key: {} row {} references a missing {} row",
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?
                .map_or_else(|| "?".to_owned(), |id| id.to_string()),
            r.get::<_, String>(2)?
        ))
    })?;
    rows.take(MAX_REPORTED_ROWS).collect()
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::backup::test_support::*;

    /// Hash of each of the file and its side files (`None` when absent).
    fn snapshot(path: &Path) -> Vec<Option<String>> {
        ["", "-wal", "-shm", "-journal"]
            .iter()
            .map(|s| {
                fs::read(sidecar(path, s))
                    .ok()
                    .map(|b| hex::encode(Sha256::digest(b)))
            })
            .collect()
    }

    fn check_both(path: &Path, key: &StoreKey) -> FileHealth {
        let quick = check_file(path, key, false).unwrap();
        assert_eq!(check_file(path, key, true).unwrap(), quick);
        quick
    }

    fn closed_store(dir: &Path) -> PathBuf {
        let path = dir.join("nugget.db");
        let s = Store::open(&path, &key()).unwrap();
        populate(&s, 20);
        drop(s);
        assert!(!sidecar(&path, "-wal").exists());
        path
    }

    #[test]
    fn healthy_closed_store_and_backup_are_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = closed_store(dir.path());
        let before = snapshot(&path);
        assert_eq!(check_both(&path, &key()), FileHealth::Healthy);
        assert_eq!(
            snapshot(&path),
            before,
            "check_file changed or created files"
        );
        assert_eq!(before[1..], [None, None, None]);

        let s = Store::open(&path, &key()).unwrap();
        let backup = dir.path().join("b.nbk");
        s.backup_into(&backup, &[]).unwrap();
        let before = snapshot(&backup);
        assert_eq!(check_both(&backup, &key()), FileHealth::Healthy);
        assert_eq!(snapshot(&backup), before);
    }

    /// A crashed copy (database, WAL and `-shm` of a store that is still open) of a store with
    /// many small commits, in its own directory.
    fn crashed_copy(dir: &Path, items: usize) -> PathBuf {
        let path = dir.join("nugget.db");
        let s = Store::open(&path, &key()).unwrap();
        populate(&s, items);
        let crash = dir.join("crash");
        fs::create_dir(&crash).unwrap();
        let copy = crash.join("nugget.db");
        for suffix in ["", "-wal", "-shm"] {
            fs::copy(sidecar(&path, suffix), sidecar(&copy, suffix)).unwrap();
        }
        copy
    }

    /// `(offset, length, is commit)` of each whole frame of a WAL.
    fn frames(wal: &[u8]) -> Vec<(usize, usize, bool)> {
        let page = be32(&wal[8..]) as usize;
        let len = WAL_FRAME_HEADER + page;
        (WAL_HEADER..)
            .step_by(len)
            .take_while(|o| o + len <= wal.len())
            .map(|o| (o, len, be32(&wal[o + 4..]) != 0))
            .collect()
    }

    fn health_with_wal(copy: &Path, wal: &[u8]) -> FileHealth {
        fs::write(sidecar(copy, "-wal"), wal).unwrap();
        let before = snapshot(copy);
        let h = check_file(copy, &key(), false).unwrap();
        assert_eq!(snapshot(copy), before, "check_file wrote");
        h
    }

    /// Damage SQLite would silently treat as the end of the log is reported; the tails a crash
    /// legitimately leaves (uncommitted frames, frames from before a restart) are not.
    #[test]
    fn wal_damage_is_told_apart_from_crash_tails() {
        let dir = tempfile::tempdir().unwrap();
        let copy = crashed_copy(dir.path(), 12);
        let wal = fs::read(sidecar(&copy, "-wal")).unwrap();
        let fr = frames(&wal);
        assert!(fr.iter().filter(|f| f.2).count() > 10, "too few commits");
        assert_eq!(health_with_wal(&copy, &wal), FileHealth::Healthy);

        let corrupt = |h: FileHealth| matches!(&h, FileHealth::Corrupt(m) if m.contains("write-ahead log has"));

        let mut bad = wal.clone();
        bad[fr[1].0 + WAL_FRAME_HEADER + 100] ^= 0x01;
        let h = health_with_wal(&copy, &bad);
        assert!(corrupt(h.clone()), "{h:?}");
        fs::remove_file(sidecar(&copy, "-shm")).unwrap();
        let h = health_with_wal(&copy, &bad);
        assert!(corrupt(h.clone()), "{h:?}");

        let mut bad = wal.clone();
        bad[fr[2].0 + 8] ^= 0xff;
        assert!(corrupt(health_with_wal(&copy, &bad)));

        let mut bad = wal.clone();
        bad[12] ^= 0x01;
        assert!(corrupt(health_with_wal(&copy, &bad)));
        assert_eq!(health_with_wal(&copy, &wal[..20]), FileHealth::Healthy);
        let mut torn = wal[..WAL_HEADER].to_vec();
        torn[30] ^= 0x01;
        assert_eq!(health_with_wal(&copy, &torn), FileHealth::Healthy);

        let tail = (1..fr.len())
            .find(|&i| fr[i - 1].2 && !fr[i].2)
            .expect("a transaction of several frames");
        let uncommitted = &wal[..fr[tail].0 + fr[tail].1];
        assert_eq!(health_with_wal(&copy, uncommitted), FileHealth::Healthy);
        let mut torn = uncommitted.to_vec();
        let at = torn.len() - 10;
        torn[at] ^= 0x01;
        assert_eq!(health_with_wal(&copy, &torn), FileHealth::Healthy);
        let mut stale = wal.clone();
        let mut old = wal[fr[0].0..fr[0].0 + fr[0].1].to_vec();
        old[8] ^= 0xff;
        old[12] ^= 0xff;
        stale.extend_from_slice(&old);
        stale.extend_from_slice(&old);
        assert_eq!(health_with_wal(&copy, &stale), FileHealth::Healthy);
        assert_eq!(health_with_wal(&copy, b""), FileHealth::Healthy);
    }

    /// After a checkpoint SQLite reuses the log from its start without truncating it, so frames
    /// of the previous generation (other salts) follow the newest commit. That is no crash tail.
    #[test]
    fn stale_frames_after_a_reused_log_are_not_a_crash_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let s = Store::open(&path, &key()).unwrap();
        populate(&s, 30);
        s.with_conn(|c| {
            c.query_row("PRAGMA wal_checkpoint(RESTART)", [], |_| Ok(()))
                .db("restart")
        })
        .unwrap();
        put_item(&s, 1000);
        let wal = sidecar(&path, "-wal");
        let (problem, tail) = scan_wal(&wal).unwrap();
        assert_eq!(problem, None);
        assert!(matches!(tail, WalTail::Stale(n) if n > 0), "{tail:?}");

        let bytes = fs::read(&wal).unwrap();
        let cut = dir.path().join("cut-wal");
        fs::write(&cut, &bytes[..bytes.len() - 100]).unwrap();
        let (problem, tail) = scan_wal(&cut).unwrap();
        assert_eq!(problem, None);
        assert!(matches!(tail, WalTail::Torn(_)), "{tail:?}");
        drop(s);
    }

    /// Open a fresh copy of `copy`'s database with `wal` as its log (a store that crashed with
    /// that log) and return its item count; the opened store is consistent.
    fn items_after_crash(copy: &Path, wal: &[u8], name: &str) -> u64 {
        let dir = copy.parent().unwrap().join(name);
        fs::create_dir(&dir).unwrap();
        let db = dir.join("nugget.db");
        fs::copy(copy, &db).unwrap();
        fs::write(sidecar(&db, "-wal"), wal).unwrap();
        let s = Store::open(&db, &key()).unwrap();
        let report = s.check_consistency().unwrap();
        assert!(report.ok, "{name}: {:?}", report.problems);
        report.items
    }

    /// End of the last commit frame that lies wholly before `cut`.
    fn commit_end_before(fr: &[(usize, usize, bool)], cut: usize) -> usize {
        fr.iter()
            .filter(|f| f.2 && f.0 + f.1 <= cut)
            .map(|f| f.0 + f.1)
            .max()
            .unwrap_or(WAL_HEADER)
    }

    /// A kill or power loss while a frame is written leaves a torn or partial frame at the
    /// tail: healthy (no recovery mode after an ordinary crash), and opening the store keeps
    /// everything committed before it.
    #[test]
    fn torn_wal_tails_from_a_crash_are_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let copy = crashed_copy(dir.path(), 12);
        fs::remove_file(sidecar(&copy, "-shm")).unwrap();
        let wal = fs::read(sidecar(&copy, "-wal")).unwrap();
        let fr = frames(&wal);
        let full = items_after_crash(&copy, &wal, "full");
        assert_eq!(full, 12);

        for (n, at) in [fr.len() / 3, fr.len() / 2, fr.len() - 1]
            .into_iter()
            .enumerate()
        {
            let cut = fr[at].0 + 100;
            assert_eq!(
                health_with_wal(&copy, &wal[..cut]),
                FileHealth::Healthy,
                "cut {cut}"
            );
            let expected =
                items_after_crash(&copy, &wal[..commit_end_before(&fr, cut)], &format!("b{n}"));
            assert_eq!(
                items_after_crash(&copy, &wal[..cut], &format!("t{n}")),
                expected
            );
            if at == fr.len() - 1 {
                assert!(expected > 0, "nothing committed before the last frame");
            }
        }

        let last = *fr.iter().rev().find(|f| f.2).unwrap();
        let mut torn = wal[..last.0 + last.1].to_vec();
        torn[last.0 + WAL_FRAME_HEADER + 7] ^= 0x01;
        assert_eq!(health_with_wal(&copy, &torn), FileHealth::Healthy);
        let expected = items_after_crash(&copy, &wal[..commit_end_before(&fr, last.0)], "lb");
        assert_eq!(items_after_crash(&copy, &torn, "lt"), expected);

        let mid = *fr[..fr.len() / 2].iter().rev().find(|f| f.2).unwrap();
        let mut bad = wal.clone();
        bad[mid.0 + WAL_FRAME_HEADER + 7] ^= 0x01;
        assert!(matches!(
            health_with_wal(&copy, &bad),
            FileHealth::Corrupt(_)
        ));
    }

    /// The copy a WAL without `-shm` needs is made beside the file, hidden, and removed.
    #[test]
    fn recovery_copy_is_made_beside_the_file_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let copy = crashed_copy(dir.path(), 3);
        fs::remove_file(sidecar(&copy, "-shm")).unwrap();
        let listing = || {
            let mut v: Vec<String> = fs::read_dir(copy.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let before = listing();
        let db = ReadOnlyDb::open(&copy, &key()).unwrap();
        let during = listing();
        let hidden: Vec<_> = during
            .iter()
            .filter(|n| n.starts_with(".nugget.db.check-"))
            .collect();
        assert_eq!(hidden.len(), 1, "{during:?}");
        db.first_read().unwrap();
        drop(db);
        assert_eq!(listing(), before);
    }

    /// A store that crashed with pending WAL content: checked including that content, with
    /// nothing written; also when the `-shm` is gone.
    #[test]
    fn crashed_store_with_wal_is_checked_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let copy = crashed_copy(dir.path(), 10);
        assert!(fs::metadata(sidecar(&copy, "-wal")).unwrap().len() > 0);

        let before = snapshot(&copy);
        assert_eq!(check_both(&copy, &key()), FileHealth::Healthy);
        assert_eq!(snapshot(&copy), before);
        assert_eq!(
            check_file(&copy, &StoreKey::from_bytes([1; 32]), false).unwrap(),
            FileHealth::WrongKey
        );
        assert_eq!(snapshot(&copy), before);

        fs::remove_file(sidecar(&copy, "-shm")).unwrap();
        let before = snapshot(&copy);
        assert_eq!(check_both(&copy, &key()), FileHealth::Healthy);
        assert_eq!(snapshot(&copy), before);
        let r = Store::open(&copy, &key()).unwrap();
        assert_eq!(r.counts().unwrap().items, 10);
    }

    /// A store in use is checked beside its writer.
    #[test]
    fn live_store_is_checked_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let s = Store::open(&path, &key()).unwrap();
        populate(&s, 10);
        let before = snapshot(&path)[..2].to_vec();
        assert_eq!(check_both(&path, &key()), FileHealth::Healthy);
        assert_eq!(snapshot(&path)[..2], before[..]);
        put_item(&s, 99);
        assert!(s.check_consistency().unwrap().ok);
    }

    #[test]
    fn open_check_runs_in_full_only_when_damage_is_likely() {
        let dir = tempfile::tempdir().unwrap();
        let path = closed_store(dir.path());
        assert_eq!(
            check_file_on_open(&path, &key()).unwrap(),
            (FileHealth::Healthy, OpenCheck::Due)
        );
        assert_eq!(
            check_file_on_open(&path, &StoreKey::from_bytes([8; 32]))
                .unwrap()
                .0,
            FileHealth::WrongKey
        );
        let short = dir.path().join("short.db");
        fs::write(&short, &fs::read(&path).unwrap()[..5000]).unwrap();
        assert!(matches!(
            check_file_on_open(&short, &key()).unwrap().0,
            FileHealth::Corrupt(m) if m.contains("truncated")
        ));

        let live = Store::open(&path, &key()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        assert_eq!(
            check_file_in_background(&path, &key(), &cancel).unwrap(),
            Some(FileHealth::Healthy)
        );
        drop(live);
        assert_eq!(
            check_file_on_open(&path, &key()).unwrap(),
            (FileHealth::Healthy, OpenCheck::Recent)
        );
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            check_file_in_background(&path, &key(), &cancel).unwrap(),
            None
        );

        let stale = sidecar(&path, ".egress-00ff00ff.lock");
        fs::write(&stale, b"").unwrap();
        assert_eq!(
            check_file_on_open(&path, &key()).unwrap(),
            (FileHealth::Healthy, OpenCheck::Ran)
        );
        fs::remove_file(&stale).unwrap();

        let held = Store::open(&path, &key()).unwrap();
        assert!(!crate::egress::unclean_close_left(&path));
        populate(&held, 5);
        assert!(non_empty(&sidecar(&path, "-wal")));
        assert_eq!(check_file_on_open(&path, &key()).unwrap().1, OpenCheck::Ran);
        drop(held);
    }

    #[test]
    fn a_clean_close_without_drop_is_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let s = Store::open(&path, &key()).unwrap();
        populate(&s, 20);
        s.close_cleanly();
        assert!(!crate::egress::unclean_close_left(&path));
        assert_eq!(
            fs::metadata(sidecar(&path, "-wal")).map_or(0, |m| m.len()),
            0
        );
        std::mem::forget(s); // as a daemon exiting without dropping it
        assert_eq!(
            check_file_on_open(&path, &key()).unwrap(),
            (FileHealth::Healthy, OpenCheck::Due)
        );
    }

    #[test]
    fn background_damage_makes_the_next_open_check_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = closed_store(dir.path());
        let bad = dir.path().join("bad.db");
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        for b in &mut bytes[mid..mid + 64] {
            *b ^= 0x5a;
        }
        fs::write(&bad, &bytes).unwrap();
        assert_eq!(
            check_file_on_open(&bad, &key()).unwrap(),
            (FileHealth::Healthy, OpenCheck::Due)
        );
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            check_file_in_background(&bad, &key(), &cancel).unwrap(),
            Some(FileHealth::Corrupt(_))
        ));
        assert!(sidecar(&bad, CHECK_FAILED_SUFFIX).exists());
        let (health, check) = check_file_on_open(&bad, &key()).unwrap();
        assert_eq!(check, OpenCheck::Ran);
        assert!(matches!(health, FileHealth::Corrupt(_)), "{health:?}");
    }

    #[test]
    fn missing_wrong_key_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = closed_store(dir.path());
        assert_eq!(
            check_file(&dir.path().join("nope.db"), &key(), false).unwrap(),
            FileHealth::Missing
        );
        assert!(!dir.path().join("nope.db").exists());
        let before = snapshot(&path);
        assert_eq!(
            check_both(&path, &StoreKey::from_bytes([8; 32])),
            FileHealth::WrongKey
        );
        assert_eq!(snapshot(&path), before);

        let bad = dir.path().join("bad.db");
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        for b in &mut bytes[mid..mid + 64] {
            *b ^= 0x5a;
        }
        fs::write(&bad, &bytes).unwrap();
        let before = snapshot(&bad);
        for full in [false, true] {
            let health = check_file(&bad, &key(), full).unwrap();
            assert!(
                matches!(health, FileHealth::Corrupt(_)),
                "full={full}: {health:?}"
            );
        }
        assert_eq!(snapshot(&bad), before);

        let truncated = dir.path().join("short.db");
        fs::write(&truncated, &fs::read(&path).unwrap()[..5000]).unwrap();
        assert!(matches!(
            check_file(&truncated, &key(), false).unwrap(),
            FileHealth::Corrupt(m) if m.contains("truncated")
        ));

        let empty = dir.path().join("empty.db");
        fs::write(&empty, b"").unwrap();
        assert!(matches!(
            check_file(&empty, &key(), false).unwrap(),
            FileHealth::Corrupt(_)
        ));

        let plain = dir.path().join("plain.db");
        Connection::open(&plain)
            .unwrap()
            .execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
            .unwrap();
        assert!(matches!(
            check_file(&plain, &key(), false).unwrap(),
            FileHealth::Corrupt(m) if m.contains("unencrypted")
        ));

        assert!(matches!(
            check_file(dir.path(), &key(), false).unwrap(),
            FileHealth::Corrupt(_)
        ));
    }

    #[test]
    fn file_uris_are_absolute_and_escaped() {
        assert_eq!(
            uri_for("/tmp/a b/c%d?e#f.db", false),
            "file:/tmp/a%20b/c%25d%3Fe%23f.db"
        );
        let windows = [
            (r"C:\Users\a b\x.db", "file:///C:/Users/a%20b/x.db"),
            (r"\\server\share\d\x.db", "file:////server/share/d/x.db"),
            (r"\\?\C:\data\x.db", "file:///C:/data/x.db"),
            (r"\\?\UNC\server\share\x.db", "file:////server/share/x.db"),
        ];
        for (path, uri) in windows {
            assert_eq!(uri_for(path, true), uri, "{path}");
        }
        assert!(file_uri(Path::new("rel.db")).unwrap().starts_with("file:/"));
    }

    #[test]
    fn consistency_is_ok_on_a_populated_store() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
        let empty = s.check_consistency().unwrap();
        assert!(empty.ok, "{:?}", empty.problems);
        populate(&s, 25);
        s.search_vector_approx(&embedding(1), 3).unwrap();
        assert!(s.ann_index_ready().unwrap());
        s.delete_item(&crate::items::item_ref("files:test".into(), "3.txt".into()))
            .unwrap();
        let report = s.check_consistency().unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!((report.items, report.chunks), (24, 48));

        let mem = Store::open_in_memory().unwrap();
        populate(&mem, 3);
        assert!(mem.check_consistency().unwrap().ok);
    }

    /// Each kind of deliberate damage is reported.
    #[test]
    fn consistency_flags_broken_rows() {
        let damage: &[(&str, &str, &str)] = &[
            (
                "chunks_fts",
                "INSERT INTO chunks_fts (chunks_fts, rowid, text)
                 SELECT 'delete', id, text FROM chunks ORDER BY id LIMIT 1",
                "chunks missing from the full-text index",
            ),
            (
                "items_fts",
                "INSERT INTO items_fts (rowid, title) VALUES (424242, 'ghost')",
                "title index rows (items_fts) without an item",
            ),
            (
                "entity_names_fts",
                "INSERT INTO entity_names_fts (entity_names_fts, rowid, alias_lower)
                 SELECT 'delete', id, alias_lower FROM entity_aliases ORDER BY id LIMIT 1",
                "entity names missing from the name index",
            ),
            (
                "vector",
                "INSERT INTO chunk_vec (rowid, embedding)
                 SELECT 999999, embedding FROM chunk_vec LIMIT 1",
                "vectors (chunk_vec) without a chunk",
            ),
            (
                "ann",
                "DELETE FROM ann_vectors WHERE id = (SELECT min(id) FROM ann_vectors)",
                "vectors missing from the complete approximate index (ann_vectors)",
            ),
            (
                "item_changes",
                "DELETE FROM item_changes WHERE key = '2.txt'",
                "items without a live entry in the change feed",
            ),
            (
                "source_stats",
                "UPDATE source_stats SET items = items + 1",
                "per-source counts",
            ),
            (
                "entity_stats",
                "INSERT INTO entities (id, kind, name, name_lower, created_at, updated_at)
                     VALUES ('ent_counted', 'person', 'C', 'c', 't', 't');
                 UPDATE entity_stats SET mentions = mentions + 1 WHERE entity_id = 'ent_counted'",
                "per-entity counts",
            ),
            (
                "dangling chunk",
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO chunks (item_id, ord, text) VALUES (777777, 0, 'orphan');
                 PRAGMA foreign_keys = ON;",
                "chunks of missing items",
            ),
            (
                "dangling mention",
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO mentions (entity_id, source, key, surface, confidence, created_at)
                 VALUES ('ent_missing', 'files:test', '0.txt', 'x', 1.0, '2026-01-01T00:00:00Z');
                 PRAGMA foreign_keys = ON;",
                "mentions of missing entities",
            ),
            (
                "audit",
                "UPDATE meta SET value = 'x' WHERE key = 'audit_seal'",
                "audit log",
            ),
        ];
        for (name, sql, expect) in damage {
            let dir = tempfile::tempdir().unwrap();
            let s = Store::open(dir.path().join("nugget.db"), &key()).unwrap();
            populate(&s, 6);
            s.search_vector_approx(&embedding(1), 3).unwrap();
            s.with_conn(|c| c.execute_batch(sql).db("damage")).unwrap();
            let report = s.check_consistency().unwrap();
            assert!(!report.ok, "{name}: not flagged");
            assert!(
                report.problems.iter().any(|p| p.contains(expect)),
                "{name}: {:?}",
                report.problems
            );
        }
    }
}
