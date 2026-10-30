//! Feedback-learned scoring: an online logistic model over [`Features`], updated from accept,
//! dismiss and snooze events, persisted with the cards and explainable per card.
//!
//! The model holds the shared weights (urgency, importance, freshness, kind, ...). What feedback
//! teaches about one sender, domain, kind or thread lives in the decaying
//! [`Learned`](crate::feedback::Learned) layer, added in log-odds by [`learned_score`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::candidate::{CandidateKind, FEATURE_NAMES, Features};
use crate::cards::Card;
use crate::config::ProactiveConfig;
use crate::feedback::{Direction, Effect, Learned};

/// Whether a card ranks high enough to be offered (inbox and brief) and is not suppressed by
/// feedback (a thread marked already handled). Cards below the threshold or suppressed stay in
/// the card set, so feedback learned (or forgotten) later can bring them back.
pub(crate) fn shown(card: &Card, cfg: &ProactiveConfig) -> bool {
    card.rank >= cfg.min_score && !hidden(&card.learned)
}

/// Any effect suppresses the card.
pub(crate) fn hidden(effects: &[Effect]) -> bool {
    effects.iter().any(|e| e.direction == Direction::Hidden)
}

/// Weight-map prefix of the per-sender weights models before the learned layer kept. They are
/// no longer read or updated: [`ScoringModel::take_legacy_senders`] moves them into
/// [`Learned`].
const SENDER_PREFIX: &str = "sender:";

/// The score of a card at `now`: the model's log-odds plus the learned layer's effects. Returns
/// `(probability, model log-odds, effects)`.
pub fn learned_score(
    model: &ScoringModel,
    learned: &Learned,
    kind: CandidateKind,
    f: &Features,
    now: OffsetDateTime,
) -> (f64, f64, Vec<Effect>) {
    let z = model.log_odds(f);
    let effects = learned.effects(kind, f, now);
    let adj: f64 = effects.iter().map(|e| e.contribution).sum();
    (sigmoid(z + adj), z, effects)
}

/// User feedback on a card, as a training label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feedback {
    Accepted,
    Dismissed,
    Snoozed,
}

impl Feedback {
    /// Target probability: accept 1, dismiss 0; a snooze is a weak "not now".
    pub fn label(self) -> f64 {
        match self {
            Self::Accepted => 1.0,
            Self::Dismissed => 0.0,
            Self::Snoozed => 0.35,
        }
    }
}

/// One feature's share of a score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Factor {
    pub name: String,
    pub value: f64,
    pub weight: f64,
    /// `value × weight` (log-odds).
    pub contribution: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoringModel {
    /// Feature name → weight (by name, so new features can be added without a migration).
    pub weights: BTreeMap<String, f64>,
    pub learning_rate: f64,
    /// L2 pull of non-bias weights towards their priors.
    pub l2: f64,
    pub updates: u64,
}

impl Default for ScoringModel {
    fn default() -> Self {
        Self {
            weights: Self::prior(),
            learning_rate: 0.3,
            l2: 0.01,
            updates: 0,
        }
    }
}

pub(crate) fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

impl ScoringModel {
    /// Hand-set starting weights: due dates matter most, then people the user deals with.
    pub fn prior() -> BTreeMap<String, f64> {
        let p: [f64; FEATURE_NAMES.len()] =
            [-1.5, 2.5, 1.5, 0.8, 0.6, 0.5, 0.0, 0.2, 0.3, 0.0, 0.2];
        FEATURE_NAMES
            .iter()
            .zip(p)
            .map(|(n, w)| ((*n).to_owned(), w))
            .collect()
    }

    fn weight_vec(&self) -> [f64; FEATURE_NAMES.len()] {
        let prior = Self::prior();
        let mut w = [0.0; FEATURE_NAMES.len()];
        for (i, n) in FEATURE_NAMES.iter().enumerate() {
            w[i] = self
                .weights
                .get(*n)
                .or_else(|| prior.get(*n))
                .copied()
                .unwrap_or(0.0);
        }
        w
    }

    /// The model's log-odds for these features (no learned layer).
    pub fn log_odds(&self, f: &Features) -> f64 {
        let w = self.weight_vec();
        f.vector().iter().zip(w).map(|(x, w)| x * w).sum()
    }

    /// Probability that the user acts on a card with these features (no learned layer; see
    /// [`learned_score`]).
    pub fn score(&self, f: &Features) -> f64 {
        sigmoid(self.log_odds(f))
    }

    /// The features' contributions, largest magnitude first (bias and zero terms left out).
    pub fn explain(&self, f: &Features) -> Vec<Factor> {
        let w = self.weight_vec();
        let mut out: Vec<Factor> = FEATURE_NAMES
            .iter()
            .zip(f.vector())
            .zip(w)
            .skip(1)
            .filter(|((_, x), _)| *x != 0.0)
            .map(|((n, x), w)| Factor {
                name: (*n).to_owned(),
                value: x,
                weight: w,
                contribution: x * w,
            })
            .filter(|f| f.contribution != 0.0)
            .collect();
        out.sort_by(|a, b| b.contribution.abs().total_cmp(&a.contribution.abs()));
        out
    }

    /// One SGD step of logistic regression towards `feedback.label()`.
    pub fn update(&mut self, f: &Features, feedback: Feedback) {
        self.update_weighted(f, feedback, 1.0);
    }

    /// An SGD step scaled by `weight` in `[0, 1]` (how much this feedback says about the card's
    /// features rather than about the one card: a dismissal as "wrong info" says little).
    pub fn update_weighted(&mut self, f: &Features, feedback: Feedback, weight: f64) {
        let x = f.vector();
        let p = self.score(f);
        let err = feedback.label() - p;
        let prior = Self::prior();
        let lr = self.learning_rate * weight.clamp(0.0, 1.0);
        let mut w = self.weight_vec();
        for i in 0..w.len() {
            let reg = if i == 0 {
                0.0
            } else {
                self.l2 * (w[i] - prior[FEATURE_NAMES[i]])
            };
            w[i] += lr * (err * x[i] - reg);
        }
        for (n, v) in FEATURE_NAMES.iter().zip(w) {
            self.weights.insert((*n).to_owned(), v);
        }
        self.updates += 1;
    }

    /// Remove the per-sender weights older models learned, returning `(address, weight)`.
    pub fn take_legacy_senders(&mut self) -> Vec<(String, f64)> {
        let keys: Vec<String> = self
            .weights
            .keys()
            .filter(|k| k.starts_with(SENDER_PREFIX))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| {
                let w = self.weights.remove(&k)?;
                Some((k[SENDER_PREFIX.len()..].to_owned(), w))
            })
            .collect()
    }

    /// Forget everything learned: weights back to the prior, update count to zero.
    pub fn reset_learned(&mut self) {
        self.weights = Self::prior();
        self.updates = 0;
    }

    /// Put one kind's weight back to its prior; returns whether it had moved.
    pub fn reset_kind(&mut self, kind: CandidateKind) -> bool {
        let name = format!("kind_{}", kind.as_str());
        let prior = Self::prior()[&name];
        let old = self.weights.insert(name, prior);
        old.is_some_and(|o| o != prior)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateKind;

    #[test]
    fn urgent_beats_far() {
        let m = ScoringModel::default();
        let near = Features {
            urgency: 1.0,
            ..Default::default()
        };
        let far = Features {
            urgency: 0.05,
            ..Default::default()
        };
        assert!(m.score(&near) > m.score(&far));
        assert_eq!(m.explain(&near)[0].name, "urgency");
    }

    #[test]
    fn dismissals_lower_a_kind() {
        let mut m = ScoringModel::default();
        let f = Features {
            urgency: 0.5,
            kind: Some(CandidateKind::Renewal),
            ..Default::default()
        };
        let before = m.score(&f);
        for _ in 0..10 {
            m.update(&f, Feedback::Dismissed);
        }
        assert!(m.score(&f) < before);
        assert!(m.weights["kind_renewal"] < 0.0);
        assert_eq!(m.updates, 10);
    }

    #[test]
    fn legacy_senders_are_taken_out_and_resets() {
        let mut m = ScoringModel::default();
        m.weights.insert("sender:pitch@vendor.test".into(), -1.2);
        let f = Features {
            urgency: 0.5,
            kind: Some(CandidateKind::Renewal),
            sender: Some("pitch@vendor.test".into()),
            ..Default::default()
        };
        // Legacy weights are no longer read.
        assert_eq!(m.score(&f), ScoringModel::default().score(&f));
        assert_eq!(
            m.take_legacy_senders(),
            vec![("pitch@vendor.test".to_owned(), -1.2)]
        );
        assert_eq!(m.weights, ScoringModel::prior());
        m.update(&f, Feedback::Dismissed);
        assert!(m.reset_kind(CandidateKind::Renewal));
        assert!(!m.reset_kind(CandidateKind::Renewal));
        m.reset_learned();
        assert_eq!(m, ScoringModel::default());
        // A weighted step moves less.
        let (mut a, mut b) = (ScoringModel::default(), ScoringModel::default());
        a.update(&f, Feedback::Dismissed);
        b.update_weighted(&f, Feedback::Dismissed, 0.25);
        assert!(a.weights["kind_renewal"] < b.weights["kind_renewal"]);
        assert!(b.weights["kind_renewal"] < 0.0);
    }
}
