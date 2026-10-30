//! Learning from the user's own feedback (BB2): per sender, domain, kind and thread, with decay.
//!
//! Every accept and dismiss adds to decayed counts on up to four entries: the card's sender
//! (lowercased address), the sender's domain (not for personal mailbox domains such as
//! `gmail.com`, where unrelated people share a domain), the card kind, and the mail thread. Each
//! entry keeps how many accepts and dismissals it saw (for the explanation) and a weighted
//! `penalty` (how strongly those dismissals count, by reason). Its adjustment to a card's
//! log-odds is
//!
//! ```text
//!   weight × (accepted − penalty) / (prior + accepted + penalty)
//! ```
//!
//! with per-scope `weight` and `prior` ([`params`]): monotone in both, zero with no feedback,
//! and bounded by `±weight` however much feedback piles up. Everything decays with a half-life of
//! [`HALF_LIFE_DAYS`], so old feedback fades and [`Learned::prune`] eventually forgets it.
//!
//! The dismiss reason decides what a dismissal teaches ([`dismiss_weights`], penalty per scope):
//!
//! | reason            | sender | domain | kind | thread                       |
//! |-------------------|--------|--------|------|------------------------------|
//! | `not_important`   | 3      | 3      | 1    | 1                            |
//! | `not_relevant`    | 1      | 1      | 0.5  | 1                            |
//! | `other`           | 0.6    | 0.6    | 0.3  | 0.6                          |
//! | `wrong_info`      | 0.25   | 0      | 0.25 | 0                            |
//! | `already_handled` | 0      | 0      | 0    | hides the thread (see below) |
//!
//! "Not important" is the user saying who does not matter, so it is the strongest: one such
//! dismissal puts a sender at −2.25 log-odds. An accept counts 1 on every scope. A kind the user
//! likes does not lift a card whose sender or domain they turned down. A snooze is "not now",
//! not "never": it teaches the layer nothing (the shared scoring model still takes it as a weak
//! negative label).
//!
//! **Already handled** suppresses the thread instead of penalising anyone: conversation cards
//! from that thread (unanswered messages, questions awaiting an answer) whose latest activity
//! ([`Features::activity_at`]) is not newer than what the dismissed card was about are hidden
//! ([`Direction::Hidden`]) for [`SUPPRESS_DAYS`]. A genuinely new message in the thread surfaces
//! again, and the user's own promises in the thread are never hidden: a handled conversation does
//! not discharge a commitment.
//!
//! Deterministic and explainable: no model calls, nothing leaves the device, and every effect on
//! a card carries the counts behind it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::candidate::{CandidateKind, Features};
use crate::cards::DismissReason;
use crate::config::PUBLIC_MAIL_DOMAINS;
use crate::score::Feedback;

/// Feedback counts halve every this many days.
pub const HALF_LIFE_DAYS: f64 = 60.0;
/// A thread marked already handled stays suppressed this long.
pub const SUPPRESS_DAYS: i64 = 30;
/// At most this many entries are remembered (the weakest are forgotten first).
pub const MAX_ENTRIES: usize = 5000;
/// Entries whose decayed counts sum to less than this are forgotten.
const PRUNE_MASS: f64 = 0.05;
/// Effects smaller than this (log-odds) are not reported on cards.
const MIN_EFFECT: f64 = 0.01;
/// The summed adjustment on one card stays within this many log-odds of zero.
pub const TOTAL_CAP: f64 = 4.0;

/// What a learned entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Sender,
    Domain,
    Kind,
    Thread,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Self::Sender, Self::Domain, Self::Kind, Self::Thread];

    /// The wire name (`sender`, `domain`, `kind`, `thread`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Domain => "domain",
            Self::Kind => "kind",
            Self::Thread => "thread",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Which way an effect moved a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Ranked lower.
    Less,
    /// Ranked higher.
    More,
    /// Suppressed (a thread marked already handled).
    Hidden,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Less => "less",
            Self::More => "more",
            Self::Hidden => "hidden",
        }
    }
}

/// How one learned entry changed one card: the card's explanation ("shown less often: you
/// dismissed 3 similar cards from this sender").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub scope: Scope,
    pub key: String,
    pub direction: Direction,
    /// Decayed counts behind it, rounded.
    pub accepted: u32,
    pub dismissed: u32,
    /// Most common dismiss reason (`DismissReason::as_str`), when dismissals dominate.
    #[serde(default)]
    pub reason: Option<String>,
    /// Change in log-odds.
    pub contribution: f64,
}

/// A learned entry, for listing and reset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnedEntry {
    pub scope: Scope,
    pub key: String,
    /// Human label when the key is opaque (a thread's card title).
    #[serde(default)]
    pub label: Option<String>,
    /// Counts decayed to the time of the listing.
    pub accepted: f64,
    pub dismissed: f64,
    /// Current adjustment in log-odds.
    pub adjustment: f64,
    pub suppresses: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub last_at: OffsetDateTime,
}

/// What to forget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetScope {
    /// Everything: learned entries and the scoring model's learned weights.
    All,
    /// One entry (`key` as listed: lowercased address, domain, kind wire name, thread key).
    One(Scope, String),
}

/// One remembered sender, domain, kind or thread. Counts are decayed to `at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub scope: Scope,
    pub key: String,
    /// Human label for opaque keys (a thread's latest card title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Accepts and dismissals that counted on this entry (for the explanation).
    #[serde(default)]
    pub accepted: f64,
    #[serde(default)]
    pub dismissed: f64,
    /// The dismissals weighted by reason ([`dismiss_weights`]): what the adjustment uses.
    #[serde(default)]
    pub penalty: f64,
    /// Dismissals per reason (unweighted, decayed), for the explanation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasons: BTreeMap<String, f64>,
    /// The time the counts are decayed to: the last feedback.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// A thread dismissed as already handled at this time.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub handled_at: Option<OffsetDateTime>,
    /// The latest thread activity the user called handled: cards from the thread whose activity
    /// is not newer stay hidden.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub handled_through: Option<OffsetDateTime>,
}

/// Decay factor over `from → to` (1 when `to` is not later).
fn decay(from: OffsetDateTime, to: OffsetDateTime) -> f64 {
    let days = (to - from).as_seconds_f64() / 86_400.0;
    if days <= 0.0 {
        1.0
    } else {
        0.5f64.powf(days / HALF_LIFE_DAYS)
    }
}

/// Per-scope `(weight, prior)` of the adjustment `weight × (a − p) / (prior + a + p)` (`a`
/// accepts, `p` penalty): a sender moves fastest (one "not relevant" dismissal: −1.5 log-odds,
/// one "not important": −2.25), a domain is weaker, a kind (shared by many cards, and also
/// learned by the shared model) weakest.
pub fn params(scope: Scope) -> (f64, f64) {
    match scope {
        Scope::Sender => (3.0, 1.0),
        Scope::Domain => (1.5, 2.0),
        Scope::Kind => (1.0, 4.0),
        Scope::Thread => (2.0, 1.0),
    }
}

/// The penalty one dismissal with `reason` adds on each scope (`[sender, domain, kind, thread]`);
/// 0 means the scope does not learn from it.
pub fn dismiss_weights(reason: Option<&DismissReason>) -> [f64; 4] {
    match reason {
        Some(DismissReason::NotImportant) => [3.0, 3.0, 1.0, 1.0],
        None | Some(DismissReason::NotRelevant) => [1.0, 1.0, 0.5, 1.0],
        Some(DismissReason::Other(_)) => [0.6, 0.6, 0.3, 0.6],
        Some(DismissReason::WrongInfo) => [0.25, 0.0, 0.25, 0.0],
        Some(DismissReason::AlreadyHandled) => [0.0; 4],
    }
}

fn adjustment(scope: Scope, accepted: f64, penalty: f64) -> f64 {
    let (w, prior) = params(scope);
    w * (accepted - penalty) / (prior + accepted + penalty)
}

impl Entry {
    fn new(scope: Scope, key: String, now: OffsetDateTime) -> Self {
        Self {
            scope,
            key,
            label: None,
            accepted: 0.0,
            dismissed: 0.0,
            penalty: 0.0,
            reasons: BTreeMap::new(),
            at: now,
            handled_at: None,
            handled_through: None,
        }
    }

    /// Counts `(accepted, dismissed)` decayed to `now`.
    fn counts(&self, now: OffsetDateTime) -> (f64, f64) {
        let k = decay(self.at, now);
        (self.accepted * k, self.dismissed * k)
    }

    /// `(accepted, penalty)` decayed to `now`.
    fn strength(&self, now: OffsetDateTime) -> (f64, f64) {
        let k = decay(self.at, now);
        (self.accepted * k, self.penalty * k)
    }

    fn adjustment(&self, now: OffsetDateTime) -> f64 {
        let (a, p) = self.strength(now);
        adjustment(self.scope, a, p)
    }

    /// The thread suppression is in force at `now`.
    fn suppressing(&self, now: OffsetDateTime) -> bool {
        self.handled_at
            .is_some_and(|h| now - h < Duration::days(SUPPRESS_DAYS))
    }

    /// Bring the counts to `now` (or scale a late-arriving event when `now` is earlier).
    fn advance(&mut self, now: OffsetDateTime) -> f64 {
        if now > self.at {
            let k = decay(self.at, now);
            self.accepted *= k;
            self.dismissed *= k;
            self.penalty *= k;
            for v in self.reasons.values_mut() {
                *v *= k;
            }
            self.at = now;
            1.0
        } else {
            decay(now, self.at)
        }
    }

    /// Decayed mass left at `now` (what pruning looks at).
    fn mass(&self, now: OffsetDateTime) -> f64 {
        let (a, d) = self.counts(now);
        a + d
    }

    fn top_reason(&self) -> Option<String> {
        self.reasons
            .iter()
            .filter(|(_, v)| **v > 0.0)
            .max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(k, _)| k.clone())
    }
}

/// Everything learned from feedback, persisted in [`ProactiveState`](crate::ProactiveState)
/// (the encrypted store). Deterministic: no model calls, nothing leaves the device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Learned {
    /// Entries keyed by `"<scope>:<key>"`.
    pub entries: BTreeMap<String, Entry>,
}

fn entry_id(scope: Scope, key: &str) -> String {
    format!("{}:{key}", scope.as_str())
}

/// Normalise a key as entered for `scope` (addresses and domains are case-insensitive).
fn normalise(scope: Scope, key: &str) -> String {
    match scope {
        Scope::Sender | Scope::Domain => key.trim().to_lowercase(),
        Scope::Kind | Scope::Thread => key.trim().to_owned(),
    }
}

/// Mailbox providers whose every country domain is personal mail (`yahoo.co.uk`, `hotmail.fr`,
/// `web.de`): matched on the first label of the base domain.
const MAILBOX_BRANDS: &[&str] = &[
    "gmail",
    "googlemail",
    "yahoo",
    "ymail",
    "rocketmail",
    "hotmail",
    "outlook",
    "live",
    "msn",
    "aol",
    "icloud",
    "gmx",
    "web",
    "yandex",
    "mail",
    "proton",
    "protonmail",
    "tutanota",
    "tuta",
    "zoho",
    "fastmail",
    "orange",
    "laposte",
    "libero",
    "t-online",
    "qq",
    "163",
    "126",
    "naver",
    "seznam",
    "rediffmail",
];

/// Base domains under which anyone can get their own subdomain and send from it (a Microsoft
/// 365 tenant, a hosted site): what one sender there taught says nothing about another.
const HOSTED_SUFFIXES: &[&str] = &[
    "onmicrosoft.com",
    "github.io",
    "herokuapp.com",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "web.app",
    "firebaseapp.com",
    "appspot.com",
    "blogspot.com",
    "me.uk",
];

/// Whether unrelated people can get addresses at this base domain.
fn shared_domain(d: &str) -> bool {
    PUBLIC_MAIL_DOMAINS.contains(&d)
        || HOSTED_SUFFIXES.contains(&d)
        || d.split('.')
            .next()
            .is_some_and(|l| MAILBOX_BRANDS.contains(&l))
}

/// The domain of a sender address, unless unrelated people can get addresses there (a personal
/// mailbox provider in any country, a hosted tenant suffix): anyone could otherwise sign up
/// and inherit the lift the user's accepts gave that domain.
pub fn sender_domain(sender: &str) -> Option<String> {
    let (_, d) = sender.trim().rsplit_once('@')?;
    let d = crate::generators::base_domain(d);
    (!d.is_empty() && !shared_domain(&d)).then_some(d)
}

/// A card about the conversation itself (an unanswered message, or the user's question still
/// waiting for an answer: a follow-up with a counterpart sender), which a thread marked already
/// handled hides. The user's own promises (follow-ups without a sender) and commitments are never
/// hidden: a handled conversation does not discharge a commitment.
fn conversational(kind: CandidateKind, f: &Features) -> bool {
    match kind {
        CandidateKind::UnansweredThread => true,
        CandidateKind::FollowUp => f.sender.is_some(),
        _ => false,
    }
}

/// The kind with this wire name.
pub fn parse_kind(s: &str) -> Option<CandidateKind> {
    CandidateKind::ALL.into_iter().find(|k| k.as_str() == s)
}

/// The entries a card with these features touches: `(scope, key)` for sender, domain, kind,
/// thread (missing ones left out).
fn keys(kind: CandidateKind, f: &Features) -> Vec<(Scope, String)> {
    let mut out = Vec::with_capacity(4);
    if let Some(s) = f.sender.as_deref().map(|s| normalise(Scope::Sender, s))
        && !s.is_empty()
    {
        if let Some(d) = sender_domain(&s) {
            out.push((Scope::Domain, d));
        }
        out.insert(0, (Scope::Sender, s));
    }
    out.push((Scope::Kind, kind.as_str().to_owned()));
    if let Some(t) = f.thread.as_deref().filter(|t| !t.is_empty()) {
        out.push((Scope::Thread, t.to_owned()));
    }
    out
}

fn scope_index(scope: Scope) -> usize {
    match scope {
        Scope::Sender => 0,
        Scope::Domain => 1,
        Scope::Kind => 2,
        Scope::Thread => 3,
    }
}

impl Learned {
    /// Record one piece of feedback on a card with these features.
    pub fn record(
        &mut self,
        kind: CandidateKind,
        features: &Features,
        feedback: Feedback,
        reason: Option<&DismissReason>,
        label: &str,
        now: OffsetDateTime,
    ) {
        if feedback == Feedback::Snoozed {
            return;
        }
        let weights = dismiss_weights(reason);
        let handled = matches!(reason, Some(DismissReason::AlreadyHandled));
        for (scope, key) in keys(kind, features) {
            let (a, d) = match feedback {
                Feedback::Accepted => (1.0, 0.0),
                _ => (0.0, weights[scope_index(scope)]),
            };
            let suppress = feedback == Feedback::Dismissed && handled && scope == Scope::Thread;
            if a == 0.0 && d == 0.0 && !suppress {
                continue;
            }
            let e = self
                .entries
                .entry(entry_id(scope, &key))
                .or_insert_with(|| Entry::new(scope, key, now));
            let k = e.advance(now);
            e.accepted += a * k;
            if d > 0.0 {
                e.dismissed += k;
                e.penalty += d * k;
            }
            if feedback == Feedback::Dismissed && d > 0.0 {
                let r = reason.map_or("not_relevant", DismissReason::as_str);
                *e.reasons.entry(r.to_owned()).or_default() += k;
            }
            if suppress {
                e.handled_at = Some(e.handled_at.map_or(now, |h| h.max(now)));
                // What the dismissed card was about; without it, everything until now.
                let through = features.activity_at.map_or(now, |t| t.min(now));
                e.handled_through = Some(e.handled_through.map_or(through, |h| h.max(through)));
            } else if feedback == Feedback::Accepted {
                // Accepting a card from the thread means it is live again.
                e.handled_at = None;
                e.handled_through = None;
            }
            if scope == Scope::Thread && !label.is_empty() {
                e.label = Some(crate::text::truncate(label, 120));
            }
        }
        self.bound(now);
    }

    /// The effects on a card with these features at `now`, strongest first. Their
    /// `contribution`s sum to the log-odds adjustment; any `Hidden` effect suppresses the card.
    pub fn effects(
        &self,
        kind: CandidateKind,
        features: &Features,
        now: OffsetDateTime,
    ) -> Vec<Effect> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (scope, key) in keys(kind, features) {
            let Some(e) = self.entries.get(&entry_id(scope, &key)) else {
                continue;
            };
            let (a, d) = e.counts(now);
            let (_, pen) = e.strength(now);
            let reason = (pen > a).then(|| e.top_reason()).flatten();
            let hidden = e.suppressing(now)
                && conversational(kind, features)
                && e.handled_through
                    .or(e.handled_at)
                    .is_some_and(|h| features.activity_at.is_none_or(|t| t <= h));
            if hidden {
                out.push(Effect {
                    scope,
                    key: key.clone(),
                    direction: Direction::Hidden,
                    accepted: a.round() as u32,
                    // The already-handled dismissal itself counts.
                    dismissed: (d.round() as u32).max(1),
                    reason: Some(DismissReason::AlreadyHandled.as_str().to_owned()),
                    contribution: 0.0,
                });
            }
            let adj = adjustment(scope, a, pen);
            if adj.abs() >= MIN_EFFECT {
                out.push(Effect {
                    scope,
                    key,
                    direction: if adj > 0.0 {
                        Direction::More
                    } else {
                        Direction::Less
                    },
                    accepted: a.round() as u32,
                    dismissed: d.round() as u32,
                    reason,
                    contribution: adj,
                });
            }
        }
        // A sender or domain the user turned down is not lifted back by a kind they like.
        if out
            .iter()
            .any(|e| matches!(e.scope, Scope::Sender | Scope::Domain) && e.contribution < 0.0)
        {
            out.retain(|e| !(e.scope == Scope::Kind && e.contribution > 0.0));
        }
        // Several scopes agreeing should not bury (or lift) a card without limit.
        let total: f64 = out.iter().map(|e| e.contribution).sum();
        if total.abs() > TOTAL_CAP {
            let k = TOTAL_CAP / total.abs();
            for e in &mut out {
                e.contribution *= k;
            }
        }
        out.sort_by(|x, y| {
            (y.direction == Direction::Hidden)
                .cmp(&(x.direction == Direction::Hidden))
                .then_with(|| y.contribution.abs().total_cmp(&x.contribution.abs()))
                .then_with(|| x.scope.cmp(&y.scope))
        });
        out
    }

    /// Every entry still in effect at `now`, strongest adjustment first.
    pub fn list(&self, now: OffsetDateTime) -> Vec<LearnedEntry> {
        let mut out: Vec<LearnedEntry> = self
            .entries
            .values()
            .filter_map(|e| {
                let (a, d) = e.counts(now);
                let adjustment = e.adjustment(now);
                let suppresses = e.suppressing(now);
                (adjustment.abs() >= MIN_EFFECT || suppresses).then(|| LearnedEntry {
                    scope: e.scope,
                    key: e.key.clone(),
                    label: e.label.clone(),
                    accepted: a,
                    dismissed: d,
                    adjustment,
                    suppresses,
                    last_at: e.handled_at.map_or(e.at, |h| h.max(e.at)),
                })
            })
            .collect();
        out.sort_by(|x, y| {
            y.suppresses
                .cmp(&x.suppresses)
                .then_with(|| y.adjustment.abs().total_cmp(&x.adjustment.abs()))
                .then_with(|| (x.scope, &x.key).cmp(&(y.scope, &y.key)))
        });
        out
    }

    /// Forget entries; returns how many were removed.
    pub fn reset(&mut self, scope: &ResetScope) -> usize {
        match scope {
            ResetScope::All => {
                let n = self.entries.len();
                self.entries.clear();
                n
            }
            ResetScope::One(s, key) => usize::from(
                self.entries
                    .remove(&entry_id(*s, &normalise(*s, key)))
                    .is_some(),
            ),
        }
    }

    /// Drop entries that decayed to nothing.
    pub fn prune(&mut self, now: OffsetDateTime) {
        self.entries
            .retain(|_, e| e.mass(now) >= PRUNE_MASS || e.suppressing(now));
        self.bound(now);
    }

    /// Seed a sender entry from a legacy per-sender model weight (log-odds `w`), so feedback
    /// learned before this layer existed keeps its effect (and now decays from `now`).
    pub fn import_sender_weight(&mut self, sender: &str, w: f64, now: OffsetDateTime) {
        let key = normalise(Scope::Sender, sender);
        if key.is_empty() || !w.is_finite() || w.abs() < MIN_EFFECT {
            return;
        }
        let (weight, prior) = params(Scope::Sender);
        // Solve weight × n / (prior + n) = |w| for the count n.
        let target = w.abs().min(0.95 * weight);
        let n = prior * target / (weight - target);
        let e = self
            .entries
            .entry(entry_id(Scope::Sender, &key))
            .or_insert_with(|| Entry::new(Scope::Sender, key, now));
        let k = e.advance(now);
        if w > 0.0 {
            e.accepted += n * k;
        } else {
            e.dismissed += n * k;
            e.penalty += n * k;
        }
    }

    /// Keep at most [`MAX_ENTRIES`]: forget the weakest (then oldest) beyond that.
    fn bound(&mut self, now: OffsetDateTime) {
        let n = self.entries.len();
        if n <= MAX_ENTRIES {
            return;
        }
        let mut v: Vec<(String, bool, f64, OffsetDateTime)> = self
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), e.suppressing(now), e.mass(now), e.at))
            .collect();
        v.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| a.2.total_cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (id, ..) in v.into_iter().take(n - MAX_ENTRIES) {
            self.entries.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-10-01 09:00 UTC)
    }

    fn mail(sender: &str, thread: &str, activity: OffsetDateTime) -> Features {
        Features {
            urgency: 0.3,
            kind: Some(CandidateKind::UnansweredThread),
            sender: Some(sender.to_owned()),
            thread: Some(thread.to_owned()),
            activity_at: Some(activity),
            ..Default::default()
        }
    }

    const K: CandidateKind = CandidateKind::UnansweredThread;

    fn adj(l: &Learned, f: &Features, now: OffsetDateTime) -> f64 {
        l.effects(K, f, now).iter().map(|e| e.contribution).sum()
    }

    fn effect(l: &Learned, f: &Features, scope: Scope, now: OffsetDateTime) -> Option<Effect> {
        l.effects(K, f, now).into_iter().find(|e| e.scope == scope)
    }

    #[test]
    fn decay_halves_after_a_half_life() {
        let mut l = Learned::default();
        let f = mail("a@acme.io", "t1", t0());
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "x",
            t0(),
        );
        let e0 = &l.list(t0())[0];
        let later = t0() + Duration::days(HALF_LIFE_DAYS as i64);
        let e1 = l
            .list(later)
            .into_iter()
            .find(|e| e.scope == e0.scope && e.key == e0.key)
            .unwrap();
        assert!((e1.dismissed - e0.dismissed / 2.0).abs() < 1e-9);
        assert!(e1.adjustment.abs() < e0.adjustment.abs());
        // Recording later decays the old counts first.
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "x",
            later,
        );
        let s = &l.entries["sender:a@acme.io"];
        assert!((s.dismissed - 1.5).abs() < 1e-9);
        assert_eq!(s.at, later);
    }

    #[test]
    fn not_important_lowers_sender_domain_and_kind() {
        let mut l = Learned::default();
        let now = t0();
        let f = mail("Pitch@Vendor.test", "t1", now);
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "x",
            now,
        );
        // Another card from the same sender, other thread.
        let g = mail("pitch@vendor.test", "t2", now);
        let s = effect(&l, &g, Scope::Sender, now).unwrap();
        assert_eq!(s.direction, Direction::Less);
        assert_eq!((s.accepted, s.dismissed), (0, 1));
        assert_eq!(s.reason.as_deref(), Some("not_important"));
        assert!((s.contribution + 2.25).abs() < 1e-9);
        let d = effect(&l, &g, Scope::Domain, now).unwrap();
        assert!(d.contribution < 0.0 && d.contribution > s.contribution);
        let k = effect(&l, &g, Scope::Kind, now).unwrap();
        assert!(k.contribution < 0.0 && k.contribution > d.contribution);
        // Someone else at another domain only loses the kind part.
        let other = mail("ana@acme.io", "t3", now);
        let e = l.effects(K, &other, now);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].scope, Scope::Kind);
        // Effects sum to the adjustment and are strongest first.
        let all = l.effects(K, &g, now);
        assert_eq!(all[0].scope, Scope::Sender);
        assert!((adj(&l, &g, now) - all.iter().map(|e| e.contribution).sum::<f64>()).abs() < 1e-12);
    }

    #[test]
    fn not_relevant_hits_the_kind_less() {
        let now = t0();
        let f = mail("x@corp.test", "t1", now);
        let mut a = Learned::default();
        a.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "",
            now,
        );
        let mut b = Learned::default();
        b.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotRelevant),
            "",
            now,
        );
        let kind = |l: &Learned| effect(l, &f, Scope::Kind, now).unwrap().contribution;
        assert!(kind(&b) > kind(&a));
        let sender = |l: &Learned| effect(l, &f, Scope::Sender, now).unwrap().contribution;
        assert!(sender(&a) < sender(&b) && sender(&b) < 0.0);
        // wrong_info only weakly.
        let mut c = Learned::default();
        c.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::WrongInfo),
            "",
            now,
        );
        assert!(sender(&c) > sender(&b) && sender(&c) < 0.0);
        assert!(effect(&c, &f, Scope::Domain, now).is_none());
    }

    #[test]
    fn already_handled_hides_the_thread_not_a_newer_message() {
        let mut l = Learned::default();
        let now = t0();
        let f = mail("ana@acme.io", "t1", now - Duration::days(2));
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::AlreadyHandled),
            "Reply to Ana: Pricing",
            now,
        );
        // Another card on the same thread about nothing newer: hidden.
        let same = mail("ana@acme.io", "t1", now - Duration::days(3));
        let e = l.effects(K, &same, now + Duration::hours(1));
        assert_eq!(e[0].direction, Direction::Hidden);
        assert_eq!(e[0].scope, Scope::Thread);
        assert_eq!(e[0].dismissed, 1);
        assert_eq!(e[0].reason.as_deref(), Some("already_handled"));
        // The sender, domain and kind are not penalised.
        assert!(e.iter().all(|e| e.contribution == 0.0));
        let other_thread = mail("ana@acme.io", "t2", now);
        assert!(l.effects(K, &other_thread, now).is_empty());
        // A new message in the thread surfaces.
        let newer = mail("ana@acme.io", "t1", now + Duration::days(1));
        assert!(l.effects(K, &newer, now + Duration::days(1)).is_empty());
        // Listed with its label; the suppression expires.
        let listed = l.list(now);
        assert!(listed[0].suppresses);
        assert_eq!(listed[0].label.as_deref(), Some("Reply to Ana: Pricing"));
        let later = now + Duration::days(SUPPRESS_DAYS + 1);
        assert!(l.effects(K, &same, later).is_empty());
        l.prune(later);
        assert!(l.entries.is_empty());
    }

    #[test]
    fn already_handled_never_hides_the_users_own_promise() {
        let mut l = Learned::default();
        let now = t0();
        let q = mail("ana@acme.io", "t1", now - Duration::days(1));
        l.record(
            K,
            &q,
            Feedback::Dismissed,
            Some(&DismissReason::AlreadyHandled),
            "",
            now,
        );
        // A promise the user made earlier in the thread (no counterpart sender) still shows.
        let promise = Features {
            kind: Some(CandidateKind::FollowUp),
            thread: Some("t1".into()),
            activity_at: Some(now - Duration::days(2)),
            ..Default::default()
        };
        assert!(l.effects(CandidateKind::FollowUp, &promise, now).is_empty());
        // The user's own question waiting for an answer in it is part of the conversation.
        let awaiting = Features {
            sender: Some("ana@acme.io".into()),
            ..promise
        };
        let e = l.effects(CandidateKind::FollowUp, &awaiting, now);
        assert_eq!(e[0].direction, Direction::Hidden);
        // Only conversation cards: a commitment is never hidden.
        assert!(
            l.effects(CandidateKind::Commitment, &awaiting, now)
                .is_empty()
        );
    }

    #[test]
    fn a_liked_kind_does_not_lift_a_turned_down_sender() {
        let mut l = Learned::default();
        let now = t0();
        for i in 0..20 {
            let f = mail(&format!("p{i}@corp{i}.test"), &format!("t{i}"), now);
            l.record(K, &f, Feedback::Accepted, None, "", now);
        }
        let bulk = mail("news@bulk.test", "tb", now);
        l.record(
            K,
            &bulk,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "",
            now,
        );
        let next = mail("news@bulk.test", "tc", now);
        let e = l.effects(K, &next, now);
        assert!(e.iter().all(|e| e.contribution <= 0.0), "{e:?}");
        // Someone new still gets the kind's lift.
        let new = mail("new@other.test", "td", now);
        assert!(effect(&l, &new, Scope::Kind, now).unwrap().contribution > 0.0);
    }

    #[test]
    fn accepts_raise() {
        let mut l = Learned::default();
        let now = t0();
        let f = mail("boss@corp.test", "t1", now);
        l.record(K, &f, Feedback::Accepted, None, "", now);
        let e = l.effects(K, &mail("boss@corp.test", "t9", now), now);
        assert!(e.iter().all(|e| e.direction == Direction::More));
        assert_eq!(e[0].scope, Scope::Sender);
        assert_eq!((e[0].accepted, e[0].dismissed), (1, 0));
        assert!(e[0].reason.is_none());
        // More accepts raise further, bounded by the scope weight.
        for _ in 0..50 {
            l.record(K, &f, Feedback::Accepted, None, "", now);
        }
        let s = effect(&l, &f, Scope::Sender, now).unwrap().contribution;
        assert!(s > 1.5 && s < params(Scope::Sender).0);
        // Snoozes teach nothing.
        let before = l.clone();
        l.record(K, &f, Feedback::Snoozed, None, "", now);
        assert_eq!(l, before);
    }

    #[test]
    fn monotone_and_capped() {
        let mut prev = 0.0;
        for d in 1..100 {
            let a = adjustment(Scope::Sender, 0.0, f64::from(d));
            assert!(a < prev && a > -params(Scope::Sender).0);
            prev = a;
        }
        assert!(adjustment(Scope::Kind, 2.0, 1.0) < adjustment(Scope::Kind, 3.0, 1.0));
        // Every scope against one card: the total is capped.
        let mut l = Learned::default();
        let f = mail("x@corp.test", "t1", t0());
        for _ in 0..200 {
            l.record(
                K,
                &f,
                Feedback::Dismissed,
                Some(&DismissReason::NotImportant),
                "",
                t0(),
            );
        }
        assert!((adj(&l, &f, t0()) + TOTAL_CAP).abs() < 1e-9);
    }

    #[test]
    fn personal_domains_are_skipped() {
        assert_eq!(sender_domain("a@gmail.com"), None);
        assert_eq!(sender_domain("a@ICLOUD.com"), None);
        assert_eq!(sender_domain("a@mail.acme.io").as_deref(), Some("acme.io"));
        let mut l = Learned::default();
        let f = mail("friend@gmail.com", "t1", t0());
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "",
            t0(),
        );
        assert!(!l.entries.keys().any(|k| k.starts_with("domain:")));
        assert!(l.entries.contains_key("sender:friend@gmail.com"));
        assert!(
            effect(
                &l,
                &mail("other@gmail.com", "t2", t0()),
                Scope::Sender,
                t0()
            )
            .is_none()
        );
    }

    /// PL-1: an address anyone can sign up for does not inherit what the user taught its
    /// domain.
    #[test]
    fn free_signup_domains_teach_nothing_about_the_domain() {
        for (friend, stranger) in [
            ("friend@yahoo.co.uk", "phish@yahoo.co.uk"),
            ("friend@hotmail.fr", "phish@hotmail.fr"),
            ("friend@web.de", "phish@web.de"),
            ("alice@corp.onmicrosoft.com", "phish@evil.onmicrosoft.com"),
            ("ops@team.github.io", "phish@evil.github.io"),
        ] {
            let mut l = Learned::default();
            for i in 0..10 {
                l.record(
                    K,
                    &mail(friend, &format!("t{i}"), t0()),
                    Feedback::Accepted,
                    None,
                    "",
                    t0(),
                );
            }
            let e = effect(&l, &mail(stranger, "tx", t0()), Scope::Domain, t0());
            assert!(e.is_none(), "{stranger} lifted by {friend}: {e:?}");
            assert!(
                !l.entries.keys().any(|k| k.starts_with("domain:")),
                "{friend}"
            );
        }
        // A company domain still learns.
        assert_eq!(sender_domain("a@mail.acme.io").as_deref(), Some("acme.io"));
        assert_eq!(sender_domain("a@acme.co.uk").as_deref(), Some("acme.co.uk"));
    }

    #[test]
    fn reset_all_one_sender_one_kind() {
        let mut l = Learned::default();
        let now = t0();
        let f = mail("a@acme.io", "t1", now);
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::NotImportant),
            "",
            now,
        );
        let n = l.entries.len();
        assert_eq!(n, 4);
        assert_eq!(
            l.reset(&ResetScope::One(Scope::Sender, "A@Acme.io ".into())),
            1
        );
        assert!(effect(&l, &f, Scope::Sender, now).is_none());
        assert!(effect(&l, &f, Scope::Domain, now).is_some());
        assert_eq!(
            l.reset(&ResetScope::One(Scope::Kind, "unanswered_thread".into())),
            1
        );
        assert!(effect(&l, &f, Scope::Kind, now).is_none());
        assert_eq!(l.reset(&ResetScope::One(Scope::Kind, "nope".into())), 0);
        assert_eq!(l.reset(&ResetScope::All), 2);
        assert!(l.entries.is_empty() && l.effects(K, &f, now).is_empty());
    }

    #[test]
    fn pruning_and_bound() {
        let mut l = Learned::default();
        let now = t0();
        for i in 0..(MAX_ENTRIES + 10) {
            let f = Features {
                sender: Some(format!("s{i}@gmail.com")),
                ..Default::default()
            };
            // The kind entry is shared; every sender adds one.
            l.record(K, &f, Feedback::Dismissed, None, "", now);
        }
        assert_eq!(l.entries.len(), MAX_ENTRIES);
        // Shared, heavier entries survive the bound.
        assert!(l.entries.contains_key("kind:unanswered_thread"));
        // After many half-lives everything has decayed away.
        l.prune(now + Duration::days(2000));
        assert!(l.entries.is_empty());
    }

    #[test]
    fn legacy_sender_weights_import() {
        let mut l = Learned::default();
        l.import_sender_weight("Pitch@Vendor.test", -1.2, t0());
        let f = mail("pitch@vendor.test", "t", t0());
        let s = effect(&l, &f, Scope::Sender, t0()).unwrap();
        assert!((s.contribution + 1.2).abs() < 1e-9);
        l.import_sender_weight("friend@acme.io", 9.0, t0());
        let g = mail("friend@acme.io", "t", t0());
        let s = effect(&l, &g, Scope::Sender, t0()).unwrap();
        assert!(s.contribution > 2.5 && s.contribution < params(Scope::Sender).0);
    }

    #[test]
    fn serde_round_trip_and_old_state() {
        let mut l = Learned::default();
        let f = mail("a@acme.io", "t1", t0());
        l.record(
            K,
            &f,
            Feedback::Dismissed,
            Some(&DismissReason::AlreadyHandled),
            "T",
            t0(),
        );
        l.record(K, &f, Feedback::Accepted, None, "T", t0());
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(serde_json::from_value::<Learned>(v).unwrap(), l);
        let old: Learned = serde_json::from_str("{}").unwrap();
        assert!(old.entries.is_empty());
    }
}
