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

/// Sink for egress events. Implemented by the store; called by anything that does network I/O.
pub trait EgressSink: Send + Sync {
    fn record(&self, event: EgressEvent);
}

/// A sink that drops events (tests, or before the store is open).
pub struct NullEgress;

impl EgressSink for NullEgress {
    fn record(&self, _event: EgressEvent) {}
}
