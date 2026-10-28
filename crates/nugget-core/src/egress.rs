use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::ItemRef;

/// One record in the egress ledger: every byte that leaves the device is accounted for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EgressEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Destination host, e.g. `api.anthropic.com`.
    pub host: String,
    /// Feature that caused the egress, e.g. `chat`, `draft`, `model_download`.
    pub feature: String,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Items whose content (or derivatives) were included in the request.
    pub items: Vec<ItemRef>,
}

/// Handle to a ledger row opened with [`EgressSink::begin`]. Only meaningful to the sink that
/// returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EgressId(pub u64);

/// Sink for egress events. Implemented by the store; called by anything that does network I/O.
///
/// Two ways to record:
/// - one-shot: [`record`](Self::record) once the exchange is over;
/// - two-phase: [`begin`](Self::begin) before the first byte is sent, so a crash mid-request
///   still leaves a row, then [`update`](Self::update) with running totals and
///   [`finish`](Self::finish) with the final ones. Prefer [`EgressExchange`], which does this
///   and falls back to one-shot for sinks without two-phase support.
pub trait EgressSink: Send + Sync {
    fn record(&self, event: EgressEvent);

    /// Open a ledger row for an exchange that is about to start. `event` carries the bytes known
    /// so far (usually the request size, received 0). Returns `None` if this sink only supports
    /// one-shot records; the caller then calls [`record`](Self::record) at the end instead.
    fn begin(&self, event: EgressEvent) -> Option<EgressId> {
        let _ = event;
        None
    }

    /// Replace the byte totals of an open row (totals, not deltas).
    fn update(&self, id: EgressId, bytes_sent: u64, bytes_received: u64) {
        let _ = (id, bytes_sent, bytes_received);
    }

    /// Final totals for an open row; marks the exchange complete.
    fn finish(&self, id: EgressId, bytes_sent: u64, bytes_received: u64) {
        self.update(id, bytes_sent, bytes_received);
    }
}

/// A sink that drops events (tests, or before the store is open).
pub struct NullEgress;

impl EgressSink for NullEgress {
    fn record(&self, _event: EgressEvent) {}
}

/// One network exchange in the ledger, recorded two-phase when the sink supports it.
///
/// Created with [`EgressExchange::begin`] before the first byte is sent. Byte counts are
/// accumulated with [`add_sent`](Self::add_sent) / [`add_received`](Self::add_received);
/// [`flush`](Self::flush) pushes running totals to the sink (call it periodically for long
/// transfers). Dropping the exchange finishes it exactly once.
pub struct EgressExchange {
    sink: Option<Arc<dyn EgressSink>>,
    id: Option<EgressId>,
    event: EgressEvent,
    /// Totals last pushed with `update`, so `flush` skips no-op writes.
    flushed: (u64, u64),
}

impl EgressExchange {
    /// Start recording an exchange to `host` for `feature`. `bytes_sent` is the request size if
    /// already known.
    pub fn begin(
        sink: Arc<dyn EgressSink>,
        host: impl Into<String>,
        feature: impl Into<String>,
        items: Vec<ItemRef>,
        bytes_sent: u64,
    ) -> Self {
        let event = EgressEvent {
            at: OffsetDateTime::now_utc(),
            host: host.into(),
            feature: feature.into(),
            bytes_sent,
            bytes_received: 0,
            items,
        };
        let id = sink.begin(event.clone());
        Self {
            sink: Some(sink),
            id,
            flushed: (event.bytes_sent, 0),
            event,
        }
    }

    /// An exchange that records nothing (on-device endpoints).
    pub fn disabled() -> Self {
        Self {
            sink: None,
            id: None,
            event: EgressEvent {
                at: OffsetDateTime::UNIX_EPOCH,
                host: String::new(),
                feature: String::new(),
                bytes_sent: 0,
                bytes_received: 0,
                items: Vec::new(),
            },
            flushed: (0, 0),
        }
    }

    pub fn bytes_sent(&self) -> u64 {
        self.event.bytes_sent
    }

    pub fn bytes_received(&self) -> u64 {
        self.event.bytes_received
    }

    pub fn add_sent(&mut self, n: u64) {
        self.event.bytes_sent = self.event.bytes_sent.saturating_add(n);
    }

    pub fn add_received(&mut self, n: u64) {
        self.event.bytes_received = self.event.bytes_received.saturating_add(n);
    }

    /// Push the running totals to the sink if they changed (two-phase sinks only).
    pub fn flush(&mut self) {
        let totals = (self.event.bytes_sent, self.event.bytes_received);
        if let (Some(sink), Some(id)) = (&self.sink, self.id)
            && totals != self.flushed
        {
            sink.update(id, totals.0, totals.1);
            self.flushed = totals;
        }
    }
}

impl Drop for EgressExchange {
    fn drop(&mut self) {
        let Some(sink) = self.sink.take() else {
            return;
        };
        match self.id {
            Some(id) => sink.finish(id, self.event.bytes_sent, self.event.bytes_received),
            None => sink.record(EgressEvent {
                at: OffsetDateTime::now_utc(),
                host: std::mem::take(&mut self.event.host),
                feature: std::mem::take(&mut self.event.feature),
                bytes_sent: self.event.bytes_sent,
                bytes_received: self.event.bytes_received,
                items: std::mem::take(&mut self.event.items),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct OneShot(Mutex<Vec<EgressEvent>>);
    impl EgressSink for OneShot {
        fn record(&self, event: EgressEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[derive(Default)]
    struct TwoPhase(Mutex<Vec<String>>);
    impl EgressSink for TwoPhase {
        fn record(&self, _event: EgressEvent) {
            self.0.lock().unwrap().push("record".into());
        }
        fn begin(&self, event: EgressEvent) -> Option<EgressId> {
            self.0
                .lock()
                .unwrap()
                .push(format!("begin {}", event.bytes_sent));
            Some(EgressId(7))
        }
        fn update(&self, id: EgressId, sent: u64, received: u64) {
            self.0
                .lock()
                .unwrap()
                .push(format!("update {} {sent} {received}", id.0));
        }
        fn finish(&self, id: EgressId, sent: u64, received: u64) {
            self.0
                .lock()
                .unwrap()
                .push(format!("finish {} {sent} {received}", id.0));
        }
    }

    #[test]
    fn one_shot_sink_gets_a_single_record_on_drop() {
        let sink = Arc::new(OneShot::default());
        let mut x = EgressExchange::begin(sink.clone(), "h", "chat", Vec::new(), 10);
        x.add_received(5);
        x.flush();
        assert!(sink.0.lock().unwrap().is_empty());
        drop(x);
        let got = sink.0.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].bytes_sent, got[0].bytes_received), (10, 5));
    }

    #[test]
    fn two_phase_sink_sees_begin_updates_and_finish() {
        let sink = Arc::new(TwoPhase::default());
        let mut x = EgressExchange::begin(sink.clone(), "h", "chat", Vec::new(), 10);
        x.flush(); // unchanged: no write
        x.add_received(5);
        x.flush();
        x.add_received(1);
        drop(x);
        assert_eq!(
            *sink.0.lock().unwrap(),
            ["begin 10", "update 7 10 5", "finish 7 10 6"]
        );
    }

    #[test]
    fn disabled_exchange_records_nothing() {
        let mut x = EgressExchange::disabled();
        x.add_sent(3);
        x.flush();
        drop(x);
    }
}
