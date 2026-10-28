//! Candidates: what the deterministic generators found, before scoring and storage.

use nugget_core::ids::ItemRef;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::actions::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// Inbound mail from a person with no reply from the user.
    UnansweredThread,
    /// A commitment in the knowledge graph (mine or owed to me), due soon or overdue.
    Commitment,
    /// A meeting with external attendees in the next day or two.
    MeetingPrep,
    /// A subscription renewal or trial ending.
    Renewal,
    /// Something the user promised in sent mail and has not followed through on.
    FollowUp,
}

impl CandidateKind {
    pub const ALL: [CandidateKind; 5] = [
        Self::UnansweredThread,
        Self::Commitment,
        Self::MeetingPrep,
        Self::Renewal,
        Self::FollowUp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnansweredThread => "unanswered_thread",
            Self::Commitment => "commitment",
            Self::MeetingPrep => "meeting_prep",
            Self::Renewal => "renewal",
            Self::FollowUp => "follow_up",
        }
    }
}

/// A source item backing a candidate, with a short quote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub item: ItemRef,
    pub quote: Option<String>,
    pub url: Option<String>,
}

/// Model inputs, each in `[0, 1]` (urgency may reach 1.2 when overdue).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Features {
    /// How soon it is due: `1 / (1 + days left)`, above 1 when overdue.
    pub urgency: f64,
    /// Relationship strength with the people involved (graph claims and mentions, mail history).
    pub importance: f64,
    /// How recent the trigger is: `exp(-age / 7 days)`.
    pub freshness: f64,
    /// The user was addressed directly (in `To`, or the owner of the commitment).
    pub direct: f64,
    /// The trigger asks a question or makes a request.
    pub question: f64,
    pub kind: Option<CandidateKind>,
}

/// Feature names in model order.
pub const FEATURE_NAMES: [&str; 11] = [
    "bias",
    "urgency",
    "importance",
    "freshness",
    "direct",
    "question",
    "kind_unanswered_thread",
    "kind_commitment",
    "kind_meeting_prep",
    "kind_renewal",
    "kind_follow_up",
];

impl Features {
    pub fn vector(&self) -> [f64; FEATURE_NAMES.len()] {
        let k = |c: CandidateKind| f64::from(u8::from(self.kind == Some(c)));
        [
            1.0,
            self.urgency,
            self.importance,
            self.freshness,
            self.direct,
            self.question,
            k(CandidateKind::UnansweredThread),
            k(CandidateKind::Commitment),
            k(CandidateKind::MeetingPrep),
            k(CandidateKind::Renewal),
            k(CandidateKind::FollowUp),
        ]
    }
}

/// Urgency from the time left until `due`: 1 at due, 1.2 when overdue, ~0 far away.
pub fn urgency(due: Option<OffsetDateTime>, now: OffsetDateTime) -> f64 {
    match due {
        None => 0.0,
        Some(d) => {
            let days = (d - now).as_seconds_f64() / 86_400.0;
            if days < 0.0 { 1.2 } else { 1.0 / (1.0 + days) }
        }
    }
}

/// Freshness from the age of the trigger.
pub fn freshness(at: OffsetDateTime, now: OffsetDateTime) -> f64 {
    let days = ((now - at).as_seconds_f64() / 86_400.0).max(0.0);
    (-days / 7.0).exp()
}

/// One proactive suggestion, with its evidence and the actions it offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Stable across runs: the same situation always yields the same key.
    pub dedup_key: String,
    pub kind: CandidateKind,
    pub title: String,
    /// One or two sentences of detail.
    pub detail: String,
    pub evidence: Vec<EvidenceRef>,
    /// The people involved (addresses or names).
    pub people: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub due: Option<OffsetDateTime>,
    /// When the card stops being useful.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
    pub features: Features,
    /// Offered actions, best first.
    pub actions: Vec<Action>,
    /// Any evidence item is sensitive (gates cloud models).
    pub sensitive: bool,
}
