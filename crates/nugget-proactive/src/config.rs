//! Engine configuration.

use nugget_core::ids::EntityId;
use serde::{Deserialize, Serialize};
use time::UtcOffset;

/// Mailbox domains shared by unrelated people: two addresses there are not colleagues.
pub const PUBLIC_MAIL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "pm.me",
    "fastmail.com",
    "hey.com",
    "gmx.com",
    "gmx.de",
    "zoho.com",
    "yandex.com",
    "mail.com",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProactiveConfig {
    /// The user's own addresses (any case). When empty, they are inferred from messages in the
    /// Sent folder / with Gmail's `SENT` label.
    pub user_addresses: Vec<String>,
    /// Domains whose people count as colleagues (not external). When empty, the non-public
    /// domains of `user_addresses`.
    pub internal_domains: Vec<String>,
    /// The user's person entity in the graph. When unset, looked up by `user_addresses`.
    pub user_entity: Option<EntityId>,
    /// Unanswered threads: the last inbound message is at least this many days old...
    pub unanswered_min_days: i64,
    /// ...and at most this many.
    pub unanswered_max_days: i64,
    /// Commitments due within this many days (or overdue) become cards.
    pub commitment_horizon_days: i64,
    /// Overdue commitments older than this many days are dropped as stale.
    pub commitment_overdue_max_days: i64,
    /// Meetings starting within this many hours get a prep card.
    pub meeting_horizon_hours: i64,
    /// Renewals and trial ends within this many days become cards.
    pub renewal_horizon_days: i64,
    /// Receipts and notices older than this are not read for renewals.
    pub renewal_lookback_days: i64,
    /// Promises in sent mail younger than this many days are tracked.
    pub followup_max_days: i64,
    /// Cards without a natural deadline expire this many days after creation.
    pub card_ttl_days: i64,
    /// Terminal cards (dismissed, done, expired) are kept this long as dedup tombstones.
    pub tombstone_days: i64,
    /// Inbox size: at most this many cards are offered at once.
    pub max_inbox: usize,
    /// The user's UTC offset, for "today" in the brief and for floating calendar times.
    #[serde(with = "offset_serde")]
    pub utc_offset: UtcOffset,
    /// LLM judge: how many top candidates it may enrich per refresh.
    pub judge_top_k: usize,
    /// LLM judge: whether a `Cloud` provider may be used (never for sensitive candidates).
    pub judge_allow_cloud: bool,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            user_addresses: Vec::new(),
            internal_domains: Vec::new(),
            user_entity: None,
            unanswered_min_days: 1,
            unanswered_max_days: 14,
            commitment_horizon_days: 7,
            commitment_overdue_max_days: 30,
            meeting_horizon_hours: 48,
            renewal_horizon_days: 14,
            renewal_lookback_days: 45,
            followup_max_days: 14,
            card_ttl_days: 14,
            tombstone_days: 90,
            max_inbox: 12,
            utc_offset: UtcOffset::UTC,
            judge_top_k: 8,
            judge_allow_cloud: false,
        }
    }
}

impl ProactiveConfig {
    /// Lowercased user addresses.
    pub(crate) fn user_set(&self) -> Vec<String> {
        self.user_addresses
            .iter()
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty())
            .collect()
    }

    /// Internal domains, explicit or derived from the user's own non-public addresses.
    pub(crate) fn internal_domains_for(&self, users: &[String]) -> Vec<String> {
        if !self.internal_domains.is_empty() {
            return self
                .internal_domains
                .iter()
                .map(|d| d.trim().trim_start_matches('@').to_lowercase())
                .collect();
        }
        let mut out: Vec<String> = users
            .iter()
            .filter_map(|a| a.rsplit_once('@').map(|(_, d)| d.to_owned()))
            .filter(|d| !PUBLIC_MAIL_DOMAINS.contains(&d.as_str()))
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

mod offset_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::UtcOffset;

    pub fn serialize<S: Serializer>(o: &UtcOffset, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(o.whole_seconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<UtcOffset, D::Error> {
        let secs = i32::deserialize(d)?;
        UtcOffset::from_whole_seconds(secs).map_err(serde::de::Error::custom)
    }
}
