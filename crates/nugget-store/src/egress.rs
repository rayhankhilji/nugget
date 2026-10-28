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
//! the writer maps to the row; `update`/`finish` are queued behind it without waiting. Rows still
//! `open` when a store is opened were left by a crash (or a kill) and are marked `interrupted`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender, SyncSender};
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
}

impl EgressWriter {
    /// Marks rows left `open` by a previous process `interrupted`, then starts the writer.
    pub(crate) fn spawn(db: Arc<Db>) -> Result<Self> {
        mark_interrupted(&db)?;
        let (tx, rx) = mpsc::channel();
        let thread_db = db.clone();
        let thread = std::thread::Builder::new()
            .name("nugget-egress".into())
            .spawn(move || run_writer(&thread_db, rx))
            .map_err(|e| Error::Storage(format!("start egress ledger writer: {e}")))?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            db,
            next_token: AtomicU64::new(1),
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
            && let Err(e) = insert_event(&self.db, &event)
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
            Err(Msg::Write(event, _)) => insert_event(&self.db, &event),
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
            // Only after a writer panic: the row stays `open` and is marked `interrupted` on the
            // next open.
            tracing::error!(token = id.0, "egress ledger writer stopped; totals lost");
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
        // lost when the store goes away. Rows never finished stay `open` (see `mark_interrupted`).
        drop(self.tx.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("egress ledger writer panicked");
        }
    }
}

/// A two-phase exchange as the writer knows it.
enum Open {
    Row(i64),
    /// The `open` insert failed; retried one-shot with the final totals.
    Unwritten(EgressEvent),
}

fn run_writer(db: &Db, rx: Receiver<Msg>) {
    let mut open: HashMap<u64, Open> = HashMap::new();
    for msg in rx {
        match msg {
            Msg::Write(event, reply) => {
                let result = insert_event(db, &event);
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
                let entry = match insert_row(db, &event, "open") {
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
                        if done && let Err(e) = insert_event(db, event) {
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

fn insert_event(db: &Db, event: &EgressEvent) -> Result<()> {
    insert_row(db, event, "done").map(drop)
}

fn insert_row(db: &Db, event: &EgressEvent, state: &str) -> Result<i64> {
    let at = format_time(event.at)?;
    let items = serde_json::to_string(&event.items)?;
    db.with_conn(|c| {
        c.execute(
            "INSERT INTO egress (at, host, feature, bytes_sent, bytes_received, items, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                at,
                event.host,
                event.feature,
                clamp_u64(event.bytes_sent),
                clamp_u64(event.bytes_received),
                items,
                state,
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

/// Rows still `open` from an earlier process can never be finished: mark them `interrupted`.
fn mark_interrupted(db: &Db) -> Result<()> {
    let n = db.with_conn(|c| {
        c.execute(
            "UPDATE egress SET state = 'interrupted' WHERE state = 'open'",
            [],
        )
        .db("mark interrupted egress")
    })?;
    if n > 0 {
        tracing::warn!(
            rows = n,
            "egress exchanges were interrupted by an earlier shutdown"
        );
    }
    Ok(())
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
}
