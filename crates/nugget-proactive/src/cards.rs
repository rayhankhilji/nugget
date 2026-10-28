//! Cards: candidates the user sees, their state machine, persistence and metrics.
//!
//! ```text
//!   new ──seen──▶ seen ──accept──▶ accepted ──done──▶ done
//!    │  ╲          │  ╲                │
//!    │   snooze    │   dismiss         └─dismiss──▶ dismissed
//!    │     ▼       │     ▼
//!    └─▶ snoozed ──(wakes)──▶ new        any open ──(deadline passed / resolved)──▶ expired
//! ```
//!
//! Terminal states (dismissed, done, expired) are kept as dedup tombstones so the same situation
//! never comes back as a new card; they are compacted after two weeks and dropped after
//! `tombstone_days`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use nugget_core::ids::SourceId;
use nugget_core::{Error, Result};
use nugget_store::Store;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::actions::Action;
use crate::candidate::{Candidate, CandidateKind, EvidenceRef, Features};
use crate::judge::Enrichment;
use crate::score::{Factor, Feedback, ScoringModel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    New,
    Seen,
    Snoozed,
    Accepted,
    Dismissed,
    Done,
    Expired,
}

impl CardState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Dismissed | Self::Done | Self::Expired)
    }

    /// Shown in the inbox.
    pub fn is_open(self) -> bool {
        matches!(self, Self::New | Self::Seen)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "reason", content = "text", rename_all = "snake_case")]
pub enum DismissReason {
    NotRelevant,
    AlreadyHandled,
    WrongInfo,
    NotImportant,
    Other(String),
}

impl DismissReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotRelevant => "not_relevant",
            Self::AlreadyHandled => "already_handled",
            Self::WrongInfo => "wrong_info",
            Self::NotImportant => "not_important",
            Self::Other(_) => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub from: CardState,
    pub to: CardState,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// Short, stable id derived from the dedup key.
    pub id: String,
    pub dedup_key: String,
    pub kind: CandidateKind,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<EvidenceRef>,
    pub people: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub due: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
    pub features: Features,
    /// Model probability.
    pub score: f64,
    /// Inbox order: the model score, nudged by the judge's confidence when present.
    pub rank: f64,
    /// Top factors of the score.
    pub factors: Vec<Factor>,
    pub actions: Vec<Action>,
    pub enrichment: Option<Enrichment>,
    pub sensitive: bool,
    pub state: CardState,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Last refresh whose generators produced this card.
    #[serde(with = "time::serde::rfc3339")]
    pub last_generated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub first_seen_at: Option<OffsetDateTime>,
    /// First accept or dismiss.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub acted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub snoozed_until: Option<OffsetDateTime>,
    pub dismiss_reason: Option<DismissReason>,
    /// Accepted at some point (precision counts it even after done/expired).
    #[serde(default)]
    pub was_accepted: bool,
    pub history: Vec<Transition>,
}

pub(crate) fn card_id(dedup_key: &str) -> String {
    crate::text::stable_hash(&["card", dedup_key])
}

impl Card {
    pub(crate) fn from_candidate(c: Candidate, now: OffsetDateTime) -> Self {
        Self {
            id: card_id(&c.dedup_key),
            dedup_key: c.dedup_key,
            kind: c.kind,
            title: c.title,
            detail: c.detail,
            evidence: c.evidence,
            people: c.people,
            due: c.due,
            expires_at: c.expires_at,
            features: c.features,
            score: 0.0,
            rank: 0.0,
            factors: Vec::new(),
            actions: c.actions,
            enrichment: None,
            sensitive: c.sensitive,
            state: CardState::New,
            created_at: now,
            updated_at: now,
            last_generated_at: now,
            first_seen_at: None,
            acted_at: None,
            snoozed_until: None,
            dismiss_reason: None,
            was_accepted: false,
            history: Vec::new(),
        }
    }

    /// Refresh content from a regenerated candidate (state and history are kept).
    pub(crate) fn refresh_from(&mut self, c: Candidate, now: OffsetDateTime) {
        self.title = c.title;
        self.detail = c.detail;
        self.evidence = c.evidence;
        self.people = c.people;
        self.due = c.due;
        self.expires_at = c.expires_at;
        self.features = c.features;
        self.actions = c.actions;
        self.sensitive = c.sensitive;
        self.last_generated_at = now;
    }

    pub(crate) fn rescore(&mut self, model: &ScoringModel) {
        self.score = model.score(&self.features);
        self.factors = model.explain(&self.features).into_iter().take(3).collect();
        self.rank = match &self.enrichment {
            Some(e) => 0.8 * self.score + 0.2 * e.confidence,
            None => self.score,
        };
    }

    fn go(&mut self, to: CardState, now: OffsetDateTime) {
        self.history.push(Transition {
            from: self.state,
            to,
            at: now,
        });
        self.state = to;
        self.updated_at = now;
    }

    fn invalid(&self, op: &str) -> Error {
        Error::Invalid(format!("cannot {op} a card in state {:?}", self.state))
    }

    pub fn mark_seen(&mut self, now: OffsetDateTime) -> Result<()> {
        match self.state {
            CardState::New => {
                self.first_seen_at.get_or_insert(now);
                self.go(CardState::Seen, now);
                Ok(())
            }
            CardState::Seen => Ok(()),
            _ => Err(self.invalid("mark seen")),
        }
    }

    pub fn snooze(&mut self, until: OffsetDateTime, now: OffsetDateTime) -> Result<()> {
        if !self.state.is_open() {
            return Err(self.invalid("snooze"));
        }
        if until <= now {
            return Err(Error::Invalid("snooze must end in the future".into()));
        }
        self.first_seen_at.get_or_insert(now);
        self.snoozed_until = Some(until);
        self.go(CardState::Snoozed, now);
        Ok(())
    }

    pub fn accept(&mut self, now: OffsetDateTime) -> Result<()> {
        match self.state {
            CardState::New | CardState::Seen | CardState::Snoozed => {
                self.first_seen_at.get_or_insert(now);
                self.acted_at.get_or_insert(now);
                self.was_accepted = true;
                self.snoozed_until = None;
                self.go(CardState::Accepted, now);
                Ok(())
            }
            CardState::Accepted => Ok(()),
            _ => Err(self.invalid("accept")),
        }
    }

    pub fn dismiss(&mut self, reason: DismissReason, now: OffsetDateTime) -> Result<()> {
        if self.state.is_terminal() {
            return Err(self.invalid("dismiss"));
        }
        self.first_seen_at.get_or_insert(now);
        self.acted_at.get_or_insert(now);
        self.dismiss_reason = Some(reason);
        self.go(CardState::Dismissed, now);
        Ok(())
    }

    pub fn complete(&mut self, now: OffsetDateTime) -> Result<()> {
        if self.state != CardState::Accepted {
            return Err(self.invalid("complete"));
        }
        self.go(CardState::Done, now);
        Ok(())
    }

    pub(crate) fn expire(&mut self, now: OffsetDateTime) {
        if !self.state.is_terminal() {
            self.go(CardState::Expired, now);
        }
    }

    /// Snoozed → new once the snooze ends.
    pub(crate) fn wake(&mut self, now: OffsetDateTime) -> bool {
        if self.state == CardState::Snoozed && self.snoozed_until.is_some_and(|u| u <= now) {
            self.snoozed_until = None;
            self.go(CardState::New, now);
            return true;
        }
        false
    }
}

/// Everything the engine persists: cards (by id) and the scoring model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProactiveState {
    pub version: u32,
    pub cards: BTreeMap<String, Card>,
    pub model: ScoringModel,
    /// Feedback events applied to the model (for audit/bench).
    #[serde(default)]
    pub feedback_log: Vec<FeedbackEvent>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_refresh: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackEvent {
    pub card_id: String,
    pub kind: CandidateKind,
    pub feedback: Feedback,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

pub const STATE_VERSION: u32 = 1;
/// Feedback log entries kept.
pub const MAX_FEEDBACK_LOG: usize = 2000;

impl ProactiveState {
    /// Compact terminal cards after 14 days (drop evidence, actions, history) and forget them
    /// after `tombstone_days`.
    pub(crate) fn prune(&mut self, now: OffsetDateTime, tombstone_days: i64) {
        self.cards.retain(|_, c| {
            !(c.state.is_terminal() && now - c.updated_at > Duration::days(tombstone_days))
        });
        for c in self.cards.values_mut() {
            if c.state.is_terminal() && now - c.updated_at > Duration::days(14) {
                c.evidence.clear();
                c.actions.clear();
                c.enrichment = None;
                c.detail.clear();
            }
        }
        if self.feedback_log.len() > MAX_FEEDBACK_LOG {
            let cut = self.feedback_log.len() - MAX_FEEDBACK_LOG;
            self.feedback_log.drain(..cut);
        }
    }

    pub fn by_dedup_key(&self, key: &str) -> Option<&Card> {
        self.cards.get(&card_id(key))
    }
}

/// Persistence for [`ProactiveState`].
pub trait CardStore: Send + Sync {
    fn load(&self) -> Result<ProactiveState>;
    fn save(&self, state: &ProactiveState) -> Result<()>;
}

/// In memory (tests, previews).
#[derive(Default)]
pub struct MemCardStore(Mutex<Option<ProactiveState>>);

impl CardStore for MemCardStore {
    fn load(&self) -> Result<ProactiveState> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| ProactiveState {
                version: STATE_VERSION,
                ..Default::default()
            }))
    }

    fn save(&self, state: &ProactiveState) -> Result<()> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state.clone());
        Ok(())
    }
}

/// In the encrypted store, as one JSON document under the cursor key [`STATE_KEY`] (SQLCipher
/// encrypts it at rest; nothing goes to plists or preference files).
#[derive(Clone)]
pub struct StoreCardStore {
    store: Store,
    key: SourceId,
}

/// Cursor key holding the proactive state.
pub const STATE_KEY: &str = "proactive:state";

impl StoreCardStore {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            key: SourceId::new(STATE_KEY),
        }
    }
}

impl CardStore for StoreCardStore {
    fn load(&self) -> Result<ProactiveState> {
        match self.store.get_cursor(&self.key)? {
            None => Ok(ProactiveState {
                version: STATE_VERSION,
                ..Default::default()
            }),
            Some(v) => {
                let s: ProactiveState = serde_json::from_value(v)?;
                if s.version > STATE_VERSION {
                    return Err(Error::Unsupported(format!(
                        "proactive state v{} is newer than this build (v{STATE_VERSION})",
                        s.version
                    )));
                }
                Ok(s)
            }
        }
    }

    fn save(&self, state: &ProactiveState) -> Result<()> {
        self.store
            .set_cursor(&self.key, serde_json::to_value(state)?)
    }
}

// ---- metrics ----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KindMetrics {
    pub shown: u64,
    pub accepted: u64,
    pub dismissed: u64,
    pub precision: Option<f64>,
}

/// Card quality, for the desktop and the bench.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub total: u64,
    pub by_state: BTreeMap<String, u64>,
    /// Cards the user saw (marked seen, or acted on).
    pub shown: u64,
    pub accepted: u64,
    pub dismissed: u64,
    pub snoozed: u64,
    /// `accepted / shown`.
    pub precision: Option<f64>,
    pub by_kind: BTreeMap<String, KindMetrics>,
    /// Seconds from first seen to accept or dismiss.
    pub median_time_to_action_secs: Option<f64>,
    pub mean_time_to_action_secs: Option<f64>,
    pub dismiss_reasons: BTreeMap<String, u64>,
    pub model_updates: u64,
}

fn state_name(s: CardState) -> &'static str {
    match s {
        CardState::New => "new",
        CardState::Seen => "seen",
        CardState::Snoozed => "snoozed",
        CardState::Accepted => "accepted",
        CardState::Dismissed => "dismissed",
        CardState::Done => "done",
        CardState::Expired => "expired",
    }
}

impl Metrics {
    pub fn compute(state: &ProactiveState) -> Self {
        let mut m = Metrics {
            model_updates: state.model.updates,
            ..Default::default()
        };
        let mut tta = Vec::new();
        for c in state.cards.values() {
            m.total += 1;
            *m.by_state.entry(state_name(c.state).into()).or_default() += 1;
            let k = m.by_kind.entry(c.kind.as_str().into()).or_default();
            let shown = c.first_seen_at.is_some();
            let dismissed = c.state == CardState::Dismissed;
            if shown {
                m.shown += 1;
                k.shown += 1;
            }
            if c.was_accepted {
                m.accepted += 1;
                k.accepted += 1;
            }
            if dismissed {
                m.dismissed += 1;
                k.dismissed += 1;
                if let Some(r) = &c.dismiss_reason {
                    *m.dismiss_reasons.entry(r.as_str().into()).or_default() += 1;
                }
            }
            if c.history.iter().any(|t| t.to == CardState::Snoozed) {
                m.snoozed += 1;
            }
            if let (Some(seen), Some(acted)) = (c.first_seen_at, c.acted_at) {
                tta.push((acted - seen).as_seconds_f64().max(0.0));
            }
        }
        let ratio = |a: u64, s: u64| (s > 0).then(|| a as f64 / s as f64);
        m.precision = ratio(m.accepted, m.shown);
        for k in m.by_kind.values_mut() {
            k.precision = ratio(k.accepted, k.shown);
        }
        if !tta.is_empty() {
            tta.sort_by(f64::total_cmp);
            let n = tta.len();
            m.median_time_to_action_secs = Some(if n % 2 == 1 {
                tta[n / 2]
            } else {
                (tta[n / 2 - 1] + tta[n / 2]) / 2.0
            });
            m.mean_time_to_action_secs = Some(tta.iter().sum::<f64>() / n as f64);
        }
        m
    }
}
