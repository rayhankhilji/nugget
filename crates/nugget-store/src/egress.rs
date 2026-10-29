//! The egress ledger: every byte that leaves the device, by host and feature.
//!
//! [`EgressSink`] is called from async code (an HTTP exchange's meter is dropped on a runtime
//! worker thread), so it never touches SQLite itself: it queues work for a dedicated writer
//! thread. [`Store::record_egress`] goes through the same queue and waits for its result, so the
//! ledger keeps the order events were recorded in. Reads ([`Store::recent_egress`]) first wait
//! for the queue to drain, and dropping the last [`Store`] clone writes everything queued.
//!
//! Two-phase exchanges: [`EgressSink::begin`] queues an `open` row and waits briefly
//! ([`BEGIN_WAIT`]) for it to be written, so a crash mid-request still leaves a row without an
//! async caller ever waiting long on a busy database. The returned id is a process-local token
//! the writer maps to the row; `update`/`finish` are queued behind it without waiting.
//!
//! Interrupted exchanges: each store handle is an *owner* with a random id, stamped on the rows
//! it writes. A file-backed handle holds an OS file lock on `<db>.egress-<owner>.lock` for its
//! lifetime (released by the OS when the process dies, however it dies). Opening a store marks
//! `interrupted` only the `open` rows whose owner is gone (its lock file missing or no longer
//! locked), so the CLI or a second process opening the database while the daemon is running
//! leaves the daemon's live exchanges alone. A handle dropped with exchanges still unfinished
//! marks its own rows `interrupted` once its queue is drained. The lock is per handle (not per
//! process), so two handles in one process see each other as live too.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use nugget_core::egress::{EgressEvent, EgressId, EgressSink};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use rusqlite::params;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};
use crate::store::Db;

/// How long [`EgressSink::begin`] waits for its row to be written before letting the caller
/// proceed (the row still lands, just after the exchange started).
const BEGIN_WAIT: Duration = Duration::from_millis(250);

enum Msg {
    /// Write one event; report the result if a reply channel is given.
    Write(EgressEvent, Option<SyncSender<Result<()>>>),
    /// Open a two-phase row for `token`; acknowledged once written (or failed).
    Begin(u64, Box<EgressEvent>, SyncSender<()>),
    /// New totals for the row of `token`; `done` also marks it finished.
    Update {
        token: u64,
        sent: u64,
        received: u64,
        done: bool,
    },
    /// Barrier: answered once every earlier message has been handled.
    Flush(SyncSender<()>),
}

/// The ledger writer thread and its queue.
pub(crate) struct EgressWriter {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    /// For writing directly if the thread is gone (it only stops on drop, or by panicking).
    db: Arc<Db>,
    /// Next two-phase token ([`EgressId`] values handed out by this store).
    next_token: AtomicU64,
    /// This handle's owner id, stamped on every row it writes.
    owner: Arc<str>,
    /// Held while this handle lives (file-backed stores only); see the module docs.
    lock: Mutex<Option<OwnerLock>>,
}

/// Prefix (after the database file name) and suffix of owner lock files.
const LOCK_INFIX: &str = ".egress-";
const LOCK_SUFFIX: &str = ".lock";

/// The OS file lock showing that an owner is alive. Unlocked and removed on drop.
struct OwnerLock {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Close (unlocking) before removing: deleting an open file is not portable. Either a
        // missing or an unlocked lock file reads as "owner gone".
        drop(self.file.take());
        if let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != ErrorKind::NotFound
        {
            tracing::debug!(path = %self.path.display(), error = %e, "cannot remove egress lock");
        }
    }
}

/// The lock file of `owner` for the database at `db`.
fn lock_path(db: &Path, owner: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(LOCK_INFIX);
    name.push(owner);
    name.push(LOCK_SUFFIX);
    PathBuf::from(name)
}

/// A fresh owner id: 128 random bits, hex.
fn new_owner() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
    Ok(hex::encode(bytes))
}

/// Owner ids are ours (hex); anything else in the column is not a usable lock file name.
fn valid_owner(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 64 && owner.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Create and lock this handle's lock file. Created under a temporary name and renamed once
/// locked, so another process sweeping lock files never sees it unlocked.
fn acquire_owner_lock(db: &Path, owner: &str) -> Result<OwnerLock> {
    let path = lock_path(db, owner);
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let io = |what: &str, e: std::io::Error| {
        Error::Storage(format!("{what} egress lock {}: {e}", path.display()))
    };
    let file = opts.open(&tmp).map_err(|e| io("create", e))?;
    let locked = match fs4::FileExt::try_lock(&file) {
        Ok(()) => std::fs::rename(&tmp, &path).map_err(|e| io("place", e)),
        Err(fs4::TryLockError::WouldBlock) => Err(Error::Storage(format!(
            "egress lock {} is held elsewhere",
            path.display()
        ))),
        Err(fs4::TryLockError::Error(e)) => Err(io("lock", e)),
    };
    if let Err(e) = locked {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(OwnerLock {
        file: Some(file),
        path,
    })
}

/// Whether the owner of `path`'s lock is still alive. A missing file or one we can lock means
/// it is gone (the file is then removed); when in doubt (an I/O error), alive, so live rows are
/// never marked interrupted.
fn owner_alive(path: &Path) -> bool {
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return false,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot check egress lock");
            return true;
        }
    };
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => {
            drop(file);
            let _ = std::fs::remove_file(path);
            false
        }
        Err(fs4::TryLockError::WouldBlock) => true,
        Err(fs4::TryLockError::Error(e)) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot check egress lock");
            true
        }
    }
}

/// The database file of `db`, or `None` for an in-memory store.
fn db_path(db: &Db) -> Result<Option<PathBuf>> {
    db.with_conn(|c| Ok(c.path().filter(|p| !p.is_empty()).map(PathBuf::from)))
}

impl EgressWriter {
    /// Takes this handle's owner lock, marks rows left `open` by owners that are gone
    /// `interrupted`, then starts the writer.
    pub(crate) fn spawn(db: Arc<Db>) -> Result<Self> {
        let owner: Arc<str> = new_owner()?.into();
        let path = db_path(&db)?;
        let lock = path
            .as_deref()
            .map(|p| acquire_owner_lock(p, &owner))
            .transpose()?;
        mark_interrupted(&db, path.as_deref(), &owner)?;
        let (tx, rx) = mpsc::channel();
        let (thread_db, thread_owner) = (db.clone(), owner.clone());
        let thread = std::thread::Builder::new()
            .name("nugget-egress".into())
            .spawn(move || run_writer(&thread_db, &thread_owner, rx))
            .map_err(|e| Error::Storage(format!("start egress ledger writer: {e}")))?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            db,
            next_token: AtomicU64::new(1),
            owner,
            lock: Mutex::new(lock),
        })
    }

    fn send(&self, msg: Msg) -> std::result::Result<(), Msg> {
        match &self.tx {
            Some(tx) => tx.send(msg).map_err(|SendError(m)| m),
            None => Err(msg),
        }
    }

    /// Queue an event; never blocks on the database.
    fn record(&self, event: EgressEvent) {
        if let Err(Msg::Write(event, _)) = self.send(Msg::Write(event, None))
            && let Err(e) = insert_event(&self.db, &self.owner, &event)
        {
            log_failure(&e, &event);
        }
    }

    /// Queue an event and wait until it is written.
    fn record_sync(&self, event: EgressEvent) -> Result<()> {
        let (reply, result) = mpsc::sync_channel(1);
        match self.send(Msg::Write(event, Some(reply))) {
            Ok(()) => result
                .recv()
                .unwrap_or_else(|_| Err(Error::Storage("egress ledger writer stopped".into()))),
            Err(Msg::Write(event, _)) => insert_event(&self.db, &self.owner, &event),
            Err(_) => unreachable!("sent a write"),
        }
    }

    /// Queue an `open` row and wait up to [`BEGIN_WAIT`] for it. `None` if the writer is gone:
    /// the caller then records the exchange one-shot when it ends.
    fn begin(&self, event: EgressEvent) -> Option<EgressId> {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let (ack, written) = mpsc::sync_channel(1);
        self.send(Msg::Begin(token, Box::new(event), ack)).ok()?;
        if written.recv_timeout(BEGIN_WAIT).is_err() {
            tracing::debug!(token, "egress row not written yet; proceeding");
        }
        Some(EgressId(token))
    }

    /// Queue new totals for an open row; never blocks on the database.
    fn update(&self, id: EgressId, sent: u64, received: u64, done: bool) {
        let msg = Msg::Update {
            token: id.0,
            sent,
            received,
            done,
        };
        if self.send(msg).is_err() {
            // Only after a writer panic: the row stays `open` and is marked `interrupted` when
            // this handle is dropped (or by the next open, if the process dies first).
            tracing::error!(token = id.0, "egress ledger writer stopped; totals lost");
        }
    }

    /// Tests: let go of the owner lock as a crashed process would (the lock file stays behind,
    /// unlocked), while this handle keeps running.
    #[cfg(test)]
    pub(crate) fn simulate_crash(&self) {
        let lock = self
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut lock) = lock {
            drop(lock.file.take());
            std::mem::forget(lock);
        }
    }

    /// Wait until every event queued before this call is written.
    pub(crate) fn flush(&self) {
        let (done, wait) = mpsc::sync_channel(1);
        if self.send(Msg::Flush(done)).is_ok() {
            let _ = wait.recv();
        }
    }
}

impl Drop for EgressWriter {
    fn drop(&mut self) {
        // Closing the queue lets the thread drain what is queued and exit; join so no event is
        // lost when the store goes away. Exchanges still unfinished then can never finish: mark
        // them `interrupted` before releasing the owner lock.
        drop(self.tx.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("egress ledger writer panicked");
        }
        match interrupt_owner(&self.db, Some(&self.owner)) {
            Ok(0) => {}
            Ok(n) => tracing::warn!(rows = n, "egress exchanges unfinished at store close"),
            Err(e) => tracing::error!(error = %e, "failed to mark unfinished egress rows"),
        }
        drop(
            self.lock
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}

/// A two-phase exchange as the writer knows it.
enum Open {
    Row(i64),
    /// The `open` insert failed; retried one-shot with the final totals.
    Unwritten(EgressEvent),
}

fn run_writer(db: &Db, owner: &str, rx: Receiver<Msg>) {
    let mut open: HashMap<u64, Open> = HashMap::new();
    for msg in rx {
        match msg {
            Msg::Write(event, reply) => {
                let result = insert_event(db, owner, &event);
                match reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        if let Err(e) = &result {
                            log_failure(e, &event);
                        }
                    }
                }
            }
            Msg::Begin(token, event, ack) => {
                let entry = match insert_row(db, owner, &event, "open") {
                    Ok(id) => Open::Row(id),
                    Err(e) => {
                        log_failure(&e, &event);
                        Open::Unwritten(*event)
                    }
                };
                open.insert(token, entry);
                let _ = ack.send(());
            }
            Msg::Update {
                token,
                sent,
                received,
                done,
            } => {
                let Some(entry) = open.get_mut(&token) else {
                    tracing::warn!(token, "egress update for an unknown exchange");
                    continue;
                };
                match entry {
                    Open::Row(id) => {
                        if let Err(e) = update_row(db, *id, sent, received, done) {
                            tracing::error!(error = %e, row = *id, "failed to update egress row");
                        }
                    }
                    Open::Unwritten(event) => {
                        event.bytes_sent = sent;
                        event.bytes_received = received;
                        if done && let Err(e) = insert_event(db, owner, event) {
                            log_failure(&e, event);
                        }
                    }
                }
                if done {
                    open.remove(&token);
                }
            }
            Msg::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

fn insert_event(db: &Db, owner: &str, event: &EgressEvent) -> Result<()> {
    insert_row(db, owner, event, "done").map(drop)
}

fn insert_row(db: &Db, owner: &str, event: &EgressEvent, state: &str) -> Result<i64> {
    let at = format_time(event.at)?;
    let items = serde_json::to_string(&event.items)?;
    db.with_conn(|c| {
        c.execute(
            "INSERT INTO egress (at, host, feature, bytes_sent, bytes_received, items, state, owner)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                at,
                event.host,
                event.feature,
                clamp_u64(event.bytes_sent),
                clamp_u64(event.bytes_received),
                items,
                state,
                owner,
            ],
        )
        .db("record egress")?;
        Ok(c.last_insert_rowid())
    })
}

/// Set an open row's totals; `done` also marks it finished (even if another process marked it
/// `interrupted` meanwhile: the exchange did complete).
fn update_row(db: &Db, id: i64, sent: u64, received: u64, done: bool) -> Result<()> {
    db.with_conn(|c| {
        c.execute(
            "UPDATE egress SET bytes_sent = ?2, bytes_received = ?3,
                               state = CASE WHEN ?4 THEN 'done' ELSE state END
             WHERE id = ?1",
            params![id, clamp_u64(sent), clamp_u64(received), done],
        )
        .db("update egress")?;
        Ok(())
    })
}

/// Mark `interrupted` the `open` rows of `owner` (`None`: rows written before owners were
/// recorded). Returns how many changed.
///
/// Both queries here repeat `state <> 'done'` so SQLite can use the partial `egress_unfinished`
/// index instead of scanning the whole ledger on every open.
fn interrupt_owner(db: &Db, owner: Option<&str>) -> Result<usize> {
    db.with_conn(|c| {
        c.execute(
            "UPDATE egress SET state = 'interrupted'
             WHERE state <> 'done' AND state = 'open' AND owner IS ?1",
            [owner],
        )
        .db("mark interrupted egress")
    })
}

/// Rows still `open` whose owner is gone can never be finished: mark them `interrupted`. An
/// owner is gone when its lock file next to the database at `path` is missing or can be locked
/// (see the module docs); in an in-memory store (`path` is `None`) every other owner is gone.
/// Lock files left by owners that crashed without open rows are removed too.
fn mark_interrupted(db: &Db, path: Option<&Path>, me: &str) -> Result<()> {
    let owners: Vec<Option<String>> = db.with_conn(|c| {
        let mut stmt = c
            .prepare("SELECT DISTINCT owner FROM egress WHERE state <> 'done' AND state = 'open'")
            .db("prepare open egress owners")?;
        let rows = stmt
            .query_map([], |r| r.get(0))
            .db("read open egress owners")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read open egress owners")
    })?;
    let mut n = 0;
    for owner in owners {
        let gone = match (owner.as_deref(), path) {
            (Some(o), _) if o == me => false,
            (Some(o), Some(p)) if valid_owner(o) => !owner_alive(&lock_path(p, o)),
            _ => true,
        };
        if gone {
            n += interrupt_owner(db, owner.as_deref())?;
        }
    }
    if n > 0 {
        tracing::warn!(
            rows = n,
            "egress exchanges were interrupted by an earlier shutdown"
        );
    }
    if let Some(p) = path {
        sweep_stale_locks(p, me);
    }
    Ok(())
}

/// Remove lock files of owners that are gone (a crash leaves its file behind). Best effort.
fn sweep_stale_locks(db: &Path, me: &str) {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name()) else {
        return;
    };
    let mut prefix = name.to_owned();
    prefix.push(LOCK_INFIX);
    let Some(prefix) = prefix.to_str().map(str::to_owned) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(owner) = file_name
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(|n| n.strip_suffix(LOCK_SUFFIX))
        else {
            continue;
        };
        if owner != me && valid_owner(owner) {
            // Removes the file when the owner is gone.
            owner_alive(&entry.path());
        }
    }
}

fn log_failure(e: &Error, event: &EgressEvent) {
    tracing::error!(
        error = %e,
        host = %event.host,
        feature = %event.feature,
        bytes_sent = event.bytes_sent,
        "failed to record egress event"
    );
}

/// Lifecycle of a ledger row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EgressState {
    /// The exchange is in progress (two-phase rows only); byte counts are running totals.
    Open,
    /// The exchange ended; byte counts are final.
    Done,
    /// The process stopped before the exchange ended; byte counts are the last totals flushed.
    Interrupted,
}

impl EgressState {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "open" => Ok(Self::Open),
            "done" => Ok(Self::Done),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(Error::Integrity(format!("unknown egress state {other:?}"))),
        }
    }
}

/// A ledger row with its id and state.
#[derive(Debug, Clone, PartialEq)]
pub struct EgressRow {
    pub id: i64,
    pub state: EgressState,
    pub event: EgressEvent,
}

impl Store {
    /// Append an event to the egress ledger and wait until it is written. Ordered with events
    /// queued through [`EgressSink::record`], which does not wait (and logs failures, since the
    /// trait cannot return errors).
    pub fn record_egress(&self, event: &EgressEvent) -> Result<()> {
        self.egress_writer().record_sync(event.clone())
    }

    /// Wait until every egress event recorded so far is written to the database.
    pub fn flush_egress(&self) {
        self.egress_writer().flush();
    }

    /// The most recent egress events, newest first. Includes every event recorded before the
    /// call (queued events are flushed first). Two-phase rows carry their latest totals.
    pub fn recent_egress(&self, limit: usize) -> Result<Vec<EgressEvent>> {
        Ok(self
            .egress_rows(false, limit)?
            .into_iter()
            .map(|r| r.event)
            .collect())
    }

    /// The most recent ledger rows with their ids and states, newest first (flushed like
    /// [`Self::recent_egress`]).
    pub fn recent_egress_rows(&self, limit: usize) -> Result<Vec<EgressRow>> {
        self.egress_rows(false, limit)
    }

    /// Rows not yet `done`, newest first: exchanges in progress and ones a crash interrupted.
    pub fn unfinished_egress(&self, limit: usize) -> Result<Vec<EgressRow>> {
        self.egress_rows(true, limit)
    }

    fn egress_rows(&self, unfinished: bool, limit: usize) -> Result<Vec<EgressRow>> {
        self.flush_egress();
        type Row = (i64, String, String, String, i64, i64, String, String);
        let sql = if unfinished {
            "SELECT id, at, host, feature, bytes_sent, bytes_received, items, state
             FROM egress WHERE state <> 'done' ORDER BY id DESC LIMIT ?1"
        } else {
            "SELECT id, at, host, feature, bytes_sent, bytes_received, items, state
             FROM egress ORDER BY id DESC LIMIT ?1"
        };
        let rows: Vec<Row> = self.with_read_conn(|c| {
            let mut stmt = c.prepare_cached(sql).db("prepare egress query")?;
            let rows = stmt
                .query_map([limit_i64(limit)], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                })
                .db("read egress")?;
            rows.collect::<rusqlite::Result<_>>().db("read egress")
        })?;
        rows.into_iter()
            .map(|(id, at, host, feature, sent, received, items, state)| {
                Ok(EgressRow {
                    id,
                    state: EgressState::parse(&state)?,
                    event: EgressEvent {
                        at: parse_time(&at)?,
                        host,
                        feature,
                        bytes_sent: sent.max(0) as u64,
                        bytes_received: received.max(0) as u64,
                        items: serde_json::from_str::<Vec<ItemRef>>(&items)?,
                    },
                })
            })
            .collect()
    }
}

impl EgressSink for Store {
    /// Queues the event for the writer thread; never blocks on SQLite.
    fn record(&self, event: EgressEvent) {
        self.egress_writer().record(event);
    }

    /// Queues an `open` row and waits at most [`BEGIN_WAIT`] for it to be written.
    fn begin(&self, event: EgressEvent) -> Option<EgressId> {
        self.egress_writer().begin(event)
    }

    /// Queued; never blocks on SQLite.
    fn update(&self, id: EgressId, bytes_sent: u64, bytes_received: u64) {
        self.egress_writer()
            .update(id, bytes_sent, bytes_received, false);
    }

    /// Queued; never blocks on SQLite.
    fn finish(&self, id: EgressId, bytes_sent: u64, bytes_received: u64) {
        self.egress_writer()
            .update(id, bytes_sent, bytes_received, true);
    }
}

fn clamp_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use time::OffsetDateTime;

    use super::*;

    fn event(i: usize) -> EgressEvent {
        EgressEvent {
            at: OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap(),
            host: format!("h{i}.example.com"),
            feature: "chat".into(),
            bytes_sent: i as u64,
            bytes_received: 0,
            items: Vec::new(),
        }
    }

    /// `EgressSink::record` runs on async worker threads: it must return while the database is
    /// busy, and the event must still land once it is free.
    #[test]
    fn sink_record_does_not_wait_for_the_database() {
        let s = Store::open_in_memory().unwrap();
        s.with_conn(|_conn| {
            let (done, returned) = mpsc::channel();
            let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
            std::thread::spawn(move || {
                sink.record(event(1));
                let _ = done.send(());
            });
            assert!(
                returned.recv_timeout(Duration::from_secs(5)).is_ok(),
                "record blocked while the connection was busy"
            );
            Ok(())
        })
        .unwrap();
        let got = s.recent_egress(10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].host, "h1.example.com");
    }

    /// Queued and synchronous records keep their order, and reads see every recorded event.
    #[test]
    fn records_keep_order_and_reads_flush_the_queue() {
        let s = Store::open_in_memory().unwrap();
        let sink: &dyn EgressSink = &s;
        for i in 0..300 {
            if i % 7 == 0 {
                s.record_egress(&event(i)).unwrap();
            } else {
                sink.record(event(i));
            }
        }
        let got = s.recent_egress(1000).unwrap();
        let sent: Vec<u64> = got.iter().map(|e| e.bytes_sent).collect();
        assert_eq!(sent, (0..300u64).rev().collect::<Vec<_>>());
    }

    fn states(s: &Store) -> Vec<(EgressState, u64, u64)> {
        s.recent_egress_rows(100)
            .unwrap()
            .into_iter()
            .rev()
            .map(|r| (r.state, r.event.bytes_sent, r.event.bytes_received))
            .collect()
    }

    #[test]
    fn begin_update_finish() {
        let s = Store::open_in_memory().unwrap();
        let sink: &dyn EgressSink = &s;
        let id = sink.begin(event(10)).unwrap();
        // `begin` waited for the row, so it is visible without a flush.
        let n: i64 = s
            .with_conn(|c| {
                c.query_row(
                    "SELECT count(*) FROM egress WHERE state = 'open'",
                    [],
                    |r| r.get(0),
                )
                .db("count")
            })
            .unwrap();
        assert_eq!(n, 1);
        sink.update(id, 10, 5);
        assert_eq!(states(&s), [(EgressState::Open, 10, 5)]);
        assert_eq!(s.unfinished_egress(10).unwrap().len(), 1);
        sink.finish(id, 12, 7);
        assert_eq!(states(&s), [(EgressState::Done, 12, 7)]);
        assert!(s.unfinished_egress(10).unwrap().is_empty());
        let got = s.recent_egress(10).unwrap();
        assert_eq!(got[0].host, "h10.example.com");
    }

    #[test]
    fn exchange_guard_writes_two_phase_rows() {
        use nugget_core::egress::EgressExchange;

        let s = Store::open_in_memory().unwrap();
        let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
        let mut x = EgressExchange::begin(sink, "api.example.com", "chat", Vec::new(), 100);
        x.add_received(40);
        x.flush();
        assert_eq!(states(&s), [(EgressState::Open, 100, 40)]);
        x.add_received(2);
        drop(x);
        assert_eq!(states(&s), [(EgressState::Done, 100, 42)]);
    }

    /// A process that stops mid-exchange leaves an `open` row; the next open marks it
    /// `interrupted` with the last totals flushed.
    #[test]
    fn unfinished_rows_are_interrupted_on_reopen() {
        use nugget_core::egress::EgressExchange;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let key = crate::StoreKey::from_bytes([3u8; 32]);
        {
            let s = Store::open(&path, &key).unwrap();
            let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
            let mut x = EgressExchange::begin(sink.clone(), "h", "download", Vec::new(), 7);
            x.add_received(1000);
            x.flush();
            x.add_received(1);
            // Simulate a crash: the exchange is never finished.
            std::mem::forget(x);
            let done = EgressExchange::begin(sink, "h", "chat", Vec::new(), 1);
            drop(done);
            assert_eq!(
                states(&s),
                [(EgressState::Open, 7, 1000), (EgressState::Done, 1, 0)]
            );
            // The forgotten exchange keeps this handle alive; make it look dead.
            s.egress_writer().simulate_crash();
        }
        let s = Store::open(&path, &key).unwrap();
        assert_eq!(
            states(&s),
            [
                (EgressState::Interrupted, 7, 1000),
                (EgressState::Done, 1, 0)
            ]
        );
        let unfinished = s.unfinished_egress(10).unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].state, EgressState::Interrupted);
        assert_eq!(unfinished[0].event.feature, "download");
    }

    /// One-shot records, two-phase rows and synchronous records share one ordered queue.
    #[test]
    fn two_phase_rows_keep_order_with_records() {
        let s = Store::open_in_memory().unwrap();
        let sink: &dyn EgressSink = &s;
        sink.record(event(1));
        let a = sink.begin(event(2)).unwrap();
        s.record_egress(&event(3)).unwrap();
        let b = sink.begin(event(4)).unwrap();
        sink.record(event(5));
        sink.update(a, 2, 1);
        sink.finish(b, 4, 9);
        sink.update(a, 2, 3);
        sink.finish(a, 2, 4);
        assert_ne!(a, b);
        assert_eq!(
            states(&s),
            [
                (EgressState::Done, 1, 0),
                (EgressState::Done, 2, 4),
                (EgressState::Done, 3, 0),
                (EgressState::Done, 4, 9),
                (EgressState::Done, 5, 0),
            ]
        );
    }

    /// `begin` waits for its row only briefly: with the database busy it returns, and the row
    /// lands once the database is free.
    #[test]
    fn begin_waits_only_briefly_for_a_busy_database() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .with_conn(|_conn| {
                let (done, returned) = mpsc::channel();
                let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
                std::thread::spawn(move || {
                    let _ = done.send(sink.begin(event(1)));
                });
                let id = returned
                    .recv_timeout(Duration::from_secs(5))
                    .expect("begin blocked while the connection was busy");
                Ok(id)
            })
            .unwrap()
            .unwrap();
        s.finish(id, 1, 2);
        assert_eq!(states(&s), [(EgressState::Done, 1, 2)]);
    }

    fn file_store(path: &Path) -> Store {
        Store::open(path, &crate::StoreKey::from_bytes([5u8; 32])).unwrap()
    }

    fn insert_open(s: &Store, owner: Option<&str>) -> i64 {
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO egress (at, host, feature, bytes_sent, bytes_received, items, state,
                                     owner)
                 VALUES ('2026-01-01T00:00:00Z', 'h', 'f', 1, 2, '[]', 'open', ?1)",
                [owner],
            )
            .db("insert")?;
            Ok(c.last_insert_rowid())
        })
        .unwrap()
    }

    fn state_of(s: &Store, id: i64) -> EgressState {
        s.recent_egress_rows(100)
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
            .state
    }

    fn lock_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter(|n| n.contains(LOCK_INFIX))
            .collect();
        names.sort();
        names
    }

    /// The W9 hazard: the CLI (or any second process) opening the store while the daemon has an
    /// exchange in flight must not mark the daemon's row interrupted. Two handles in one test
    /// process stand for the two processes: the owner lock is per handle.
    #[test]
    fn opening_the_store_leaves_live_rows_of_another_handle_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let daemon = file_store(&path);
        let sink: &dyn EgressSink = &daemon;
        let id = sink.begin(event(1)).unwrap();
        sink.update(id, 1, 10);
        daemon.flush_egress();

        let cli = file_store(&path);
        let rows = cli.unfinished_egress(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, EgressState::Open, "live row was interrupted");
        drop(cli);
        // The second handle closing does not touch the first one's rows either.
        assert_eq!(states(&daemon), [(EgressState::Open, 1, 10)]);
        sink.finish(id, 1, 11);
        assert_eq!(states(&daemon), [(EgressState::Done, 1, 11)]);
        drop(daemon);
        assert!(lock_files(dir.path()).is_empty(), "lock files left behind");
    }

    /// Rows whose owner is gone become `interrupted` on the next open: an owner whose lock file
    /// is still there but unlocked (a crash), one whose lock file is missing, and rows from
    /// before owners were recorded. A row whose owner still holds its lock stays `open`, and the
    /// crashed owner's lock file (and a stale one without rows) is cleaned up.
    #[test]
    fn rows_of_owners_that_are_gone_are_interrupted_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let s = file_store(&path);
        let crashed = insert_open(&s, Some("aa11"));
        let vanished = insert_open(&s, Some("bb22"));
        let legacy = insert_open(&s, None);
        let live = insert_open(&s, Some("cc33"));
        std::fs::write(lock_path(&path, "aa11"), b"").unwrap();
        std::fs::write(lock_path(&path, "dd44"), b"").unwrap();
        let held = File::create(lock_path(&path, "cc33")).unwrap();
        fs4::FileExt::try_lock(&held).unwrap();
        // `s` itself is a live owner of none of these rows, so reopening decides them.
        drop(s);

        let s = file_store(&path);
        assert_eq!(state_of(&s, crashed), EgressState::Interrupted);
        assert_eq!(state_of(&s, vanished), EgressState::Interrupted);
        assert_eq!(state_of(&s, legacy), EgressState::Interrupted);
        assert_eq!(state_of(&s, live), EgressState::Open);
        assert_eq!(
            lock_files(dir.path()).len(),
            2,
            "{:?}",
            lock_files(dir.path())
        );
        assert!(lock_path(&path, "cc33").exists());

        // Once the live owner lets go, the next open interrupts its row.
        drop(held);
        drop(s);
        let s = file_store(&path);
        assert_eq!(state_of(&s, live), EgressState::Interrupted);
    }

    /// Dropping a handle with an exchange unfinished marks its row `interrupted` at once; a
    /// late finish for a row marked `interrupted` still completes it.
    #[test]
    fn dropped_handles_interrupt_their_rows_and_late_finishes_still_complete() {
        use nugget_core::egress::EgressExchange;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let a = file_store(&path);
        let b = file_store(&path);
        {
            let sink: Arc<dyn EgressSink> = Arc::new(a.clone());
            let mut x = EgressExchange::begin(sink, "h", "download", Vec::new(), 3);
            x.flush();
            std::mem::forget(x);
        }
        // The forgotten exchange keeps a clone of `a` alive, so `a`'s row stays `open`; `b` is
        // closed with an unfinished exchange of its own.
        let sink: &dyn EgressSink = &b;
        sink.begin(event(2)).unwrap();
        drop(b);
        let rows = a.unfinished_egress(10).unwrap();
        let states: Vec<_> = rows
            .iter()
            .map(|r| (r.state, r.event.feature.as_str()))
            .collect();
        assert_eq!(
            states,
            [
                (EgressState::Interrupted, "chat"),
                (EgressState::Open, "download")
            ]
        );

        // A late finish (the exchange did complete) wins over `interrupted`.
        let late = a.unfinished_egress(10).unwrap()[1].id;
        a.with_conn(|c| {
            c.execute(
                "UPDATE egress SET state = 'interrupted' WHERE id = ?1",
                [late],
            )
            .db("mark")
        })
        .unwrap();
        let sink: &dyn EgressSink = &a;
        sink.finish(EgressId(1), 3, 9);
        assert_eq!(state_of(&a, late), EgressState::Done);
    }

    /// Marking rows on open uses the partial `egress_unfinished` index, not a ledger scan.
    #[test]
    fn interrupt_queries_use_the_unfinished_index() {
        let s = Store::open_in_memory().unwrap();
        for sql in [
            "SELECT DISTINCT owner FROM egress WHERE state <> 'done' AND state = 'open'",
            "UPDATE egress SET state = 'interrupted'
             WHERE state <> 'done' AND state = 'open' AND owner IS NULL",
        ] {
            let plan: Vec<String> = s
                .with_conn(|c| {
                    let mut stmt = c
                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                        .db("explain")?;
                    let rows = stmt.query_map([], |r| r.get(3)).db("explain")?;
                    rows.collect::<rusqlite::Result<_>>().db("explain")
                })
                .unwrap();
            assert!(
                plan.iter().any(|d| d.contains("egress_unfinished")),
                "{sql}: {plan:?}"
            );
        }
    }
}
