//! The egress ledger: every byte that leaves the device, by host and feature.

use nugget_core::Result;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ItemRef;
use rusqlite::params;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};

impl Store {
    /// Append an event to the egress ledger. [`EgressSink::record`] calls this and logs failures,
    /// since the trait cannot return errors.
    pub fn record_egress(&self, event: &EgressEvent) -> Result<()> {
        let at = format_time(event.at)?;
        let items = serde_json::to_string(&event.items)?;
        self.with_conn(|c| {
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

    /// The most recent egress events, newest first.
    pub fn recent_egress(&self, limit: usize) -> Result<Vec<EgressEvent>> {
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
    fn record(&self, event: EgressEvent) {
        if let Err(e) = self.record_egress(&event) {
            tracing::error!(
                error = %e,
                host = %event.host,
                feature = %event.feature,
                bytes_sent = event.bytes_sent,
                "failed to record egress event"
            );
        }
    }
}

fn clamp_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}
