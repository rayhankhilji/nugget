//! The egress ledger: every byte that leaves the device, by host and feature.
//!
//! [`EgressSink::record`] is called from async code (an HTTP exchange's meter is dropped on a
//! runtime worker thread), so it never touches SQLite itself: it queues the event for a dedicated
//! writer thread. [`Store::record_egress`] goes through the same queue and waits for its result,
//! so the ledger keeps the order events were recorded in. Reads ([`Store::recent_egress`]) first
//! wait for the queue to drain, and dropping the last [`Store`] clone writes every queued event.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SendError, Sender, SyncSender};
use std::thread::JoinHandle;

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use rusqlite::params;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};
use crate::store::Db;

enum Msg {
    /// Write one event; report the result if a reply channel is given.
    Write(EgressEvent, Option<SyncSender<Result<()>>>),
    /// Barrier: answered once every earlier message has been handled.
    Flush(SyncSender<()>),
}

/// The ledger writer thread and its queue.
pub(crate) struct EgressWriter {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    /// For writing directly if the thread is gone (it only stops on drop, or by panicking).
    db: Arc<Db>,
}

impl EgressWriter {
    pub(crate) fn spawn(db: Arc<Db>) -> Result<Self> {
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
            Err(Msg::Flush(_)) => unreachable!("sent a write"),
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
        // lost when the store goes away.
        drop(self.tx.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("egress ledger writer panicked");
        }
    }
}

fn run_writer(db: &Db, rx: Receiver<Msg>) {
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
            Msg::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

fn insert_event(db: &Db, event: &EgressEvent) -> Result<()> {
    let at = format_time(event.at)?;
    let items = serde_json::to_string(&event.items)?;
    db.with_conn(|c| {
        c.execute(
            "INSERT INTO egress (at, host, feature, bytes_sent, bytes_received, items)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                at,
                event.host,
                event.feature,
                clamp_u64(event.bytes_sent),
                clamp_u64(event.bytes_received),
                items,
            ],
        )
        .db("record egress")?;
        Ok(())
    })
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
    /// call (queued events are flushed first).
    pub fn recent_egress(&self, limit: usize) -> Result<Vec<EgressEvent>> {
        self.flush_egress();
        type Row = (String, String, String, i64, i64, String);
        let rows: Vec<Row> = self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT at, host, feature, bytes_sent, bytes_received, items
                     FROM egress ORDER BY id DESC LIMIT ?1",
                )
                .db("prepare egress query")?;
            let rows = stmt
                .query_map([limit_i64(limit)], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })
                .db("read egress")?;
            rows.collect::<rusqlite::Result<_>>().db("read egress")
        })?;
        rows.into_iter()
            .map(|(at, host, feature, sent, received, items)| {
                Ok(EgressEvent {
                    at: parse_time(&at)?,
                    host,
                    feature,
                    bytes_sent: sent.max(0) as u64,
                    bytes_received: received.max(0) as u64,
                    items: serde_json::from_str::<Vec<ItemRef>>(&items)?,
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
}
