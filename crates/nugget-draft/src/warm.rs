//! Read-ahead: prefill a draft's whole prompt into a local server's cache while the reply field
//! keeps focus, so the trigger only has the remainder to read.
//!
//! The shell calls [`Drafter::read_ahead`](crate::Drafter::read_ahead) (IPC `draft.read_ahead`)
//! with the field context it would read at the trigger, once the field has kept focus for a
//! moment, and [`Drafter::cancel_read_ahead`](crate::Drafter::cancel_read_ahead)
//! (`draft.read_ahead_cancel`) on blur. The engine prepares the draft exactly as `begin` would
//! (retrieval, voice, the same request) and sends it to a non-cloud provider as a prefill with no
//! output (llama-server `n_predict: 0`, `cache_prompt`). At the trigger, `begin` sends the same
//! request and the server reads only what changed since; the trigger's prefix-only prewarm is
//! skipped so that it does not cut the cached prompt back to the prefix.
//!
//! Privacy: never for cloud providers (nothing at all is sent, not even a connection prewarm),
//! never in secure fields or excluded apps, rate-limited ([`ReadAheadConfig::min_interval_ms`]),
//! and nothing is stored or audited: the engine keeps a fingerprint of the last prompt, the app
//! id and timestamps in memory; the prompt itself lives only in the server's in-memory cache.

use nugget_core::ids::ProviderId;
use serde::{Deserialize, Serialize};

use crate::Refusal;

/// Read-ahead settings (`[draft.read_ahead]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadAheadConfig {
    /// Off: `Drafter::read_ahead` always skips with [`ReadAheadSkip::Disabled`].
    pub enabled: bool,
    /// Minimum time between two read-aheads that send a prefill (others skip as
    /// [`ReadAheadSkip::RateLimited`]; one that finds its prompt cached does not count).
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
    /// The same prompt was the last one sent to the provider (by a read-ahead or a draft) and
    /// is still cached: nothing was sent.
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
    /// The provider prefilled: the first one the draft would try.
    pub provider: ProviderId,
    /// Characters of the prefilled prompt (system and messages).
    pub prompt_chars: usize,
    /// Retrieval and voice lookup, until the prompt was built.
    pub prepare_ms: u64,
    /// The prefill request itself (the server reading the prompt).
    pub warm_ms: u64,
}
