//! Read-ahead: prefill a draft's whole prompt into a local server's cache while the reply field
//! keeps focus, so the trigger only has the remainder to read.
//!
//! CONTRACT SKELETON (wave CC, CC1). The engine half fills in the behaviour; the types and the
//! `Drafter` methods below are the surface the daemon feature and the bench build on.

use nugget_core::ids::ProviderId;
use serde::{Deserialize, Serialize};

use crate::Refusal;

/// Read-ahead settings (`[draft.read_ahead]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadAheadConfig {
    /// Off: `Drafter::read_ahead` always skips with [`ReadAheadSkip::Disabled`].
    pub enabled: bool,
    /// Minimum time between two read-aheads that do work (others skip as
    /// [`ReadAheadSkip::RateLimited`]).
    pub min_interval_ms: u64,
}

impl Default for ReadAheadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_interval_ms: 5_000,
        }
    }
}

/// Why a read-ahead did nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadAheadSkip {
    /// `[draft.read_ahead] enabled = false`.
    Disabled,
    /// Drafting would be refused in this field or app.
    Refused(Refusal),
    /// The draft would go to a cloud provider: nothing is sent ahead of the trigger.
    Cloud,
    /// Too soon after the last read-ahead.
    RateLimited,
    /// The same context was read ahead already and is still cached.
    Unchanged,
    /// No warmer is configured, or no provider could draft.
    Unavailable,
    /// Cancelled (focus left the field, or a newer read-ahead replaced it).
    Cancelled,
}

impl ReadAheadSkip {
    /// Stable snake_case reason for IPC results.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Refused(_) => "refused",
            Self::Cloud => "cloud",
            Self::RateLimited => "rate_limited",
            Self::Unchanged => "unchanged",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What a completed read-ahead did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAheadOutcome {
    pub provider: ProviderId,
    /// Characters of the prefilled prompt (system and messages).
    pub prompt_chars: usize,
    /// Retrieval and voice lookup, until the prompt was built.
    pub prepare_ms: u64,
    /// The prefill request itself (the server reading the prompt).
    pub warm_ms: u64,
}
