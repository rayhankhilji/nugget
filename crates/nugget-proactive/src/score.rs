//! Feedback-learned scoring: an online logistic model over [`Features`], updated from accept,
//! dismiss and snooze events, persisted with the cards and explainable per card.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::candidate::{FEATURE_NAMES, Features};
use crate::cards::Card;
use crate::config::ProactiveConfig;

/// Whether a card ranks high enough to be offered (inbox and brief). Cards below the threshold
/// stay in the card set, so feedback learned later can lift them back.
pub(crate) fn shown(card: &Card, cfg: &ProactiveConfig) -> bool {
    card.rank >= cfg.min_score
}

/// Weight-map prefix of the per-sender weights.
const SENDER_PREFIX: &str = "sender:";
/// Per-sender weights stay within this many log-odds of zero.
const SENDER_CAP: f64 = 3.0;
/// At most this many senders are remembered (the weakest are forgotten first).
const MAX_SENDERS: usize = 2000;

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
    /// Learning rate of the per-sender weights: one decision about a sender should move the next
    /// card from them noticeably, unlike the shared weights.
    pub sender_learning_rate: f64,
    /// L2 pull of non-bias weights towards their priors.
    pub l2: f64,
    pub updates: u64,
}

impl Default for ScoringModel {
    fn default() -> Self {
        Self {
            weights: Self::prior(),
            learning_rate: 0.3,
            sender_learning_rate: 2.0,
            l2: 0.01,
            updates: 0,
        }
    }
}

fn sigmoid(z: f64) -> f64 {
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

    fn sender_key(f: &Features) -> Option<String> {
        f.sender
            .as_deref()
            .map(|s| format!("{SENDER_PREFIX}{}", s.trim().to_lowercase()))
    }

    /// The learned weight of the card's sender (0 when unknown).
    fn sender_weight(&self, f: &Features) -> f64 {
        Self::sender_key(f)
            .and_then(|k| self.weights.get(&k).copied())
            .unwrap_or(0.0)
    }

    /// Probability that the user acts on a card with these features.
    pub fn score(&self, f: &Features) -> f64 {
        let w = self.weight_vec();
        let z: f64 = f.vector().iter().zip(w).map(|(x, w)| x * w).sum();
        sigmoid(z + self.sender_weight(f))
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
        let sw = self.sender_weight(f);
        if sw != 0.0 {
            out.push(Factor {
                name: "sender".to_owned(),
                value: 1.0,
                weight: sw,
                contribution: sw,
            });
        }
        out.sort_by(|a, b| b.contribution.abs().total_cmp(&a.contribution.abs()));
        out
    }

    /// One SGD step of logistic regression towards `feedback.label()`.
    pub fn update(&mut self, f: &Features, feedback: Feedback) {
        let x = f.vector();
        let p = self.score(f);
        let err = feedback.label() - p;
        let prior = Self::prior();
        let mut w = self.weight_vec();
        for i in 0..w.len() {
            let reg = if i == 0 {
                0.0
            } else {
                self.l2 * (w[i] - prior[FEATURE_NAMES[i]])
            };
            w[i] += self.learning_rate * (err * x[i] - reg);
        }
        for (n, v) in FEATURE_NAMES.iter().zip(w) {
            self.weights.insert((*n).to_owned(), v);
        }
        if let Some(k) = Self::sender_key(f) {
            let v = self.weights.get(&k).copied().unwrap_or(0.0) + self.sender_learning_rate * err;
            self.weights.insert(k, v.clamp(-SENDER_CAP, SENDER_CAP));
            self.forget_senders();
        }
        self.updates += 1;
    }
}

impl ScoringModel {
    /// Keep the per-sender weights bounded: drop the ones closest to zero.
    fn forget_senders(&mut self) {
        let n = self
            .weights
            .keys()
            .filter(|k| k.starts_with(SENDER_PREFIX))
            .count();
        if n <= MAX_SENDERS {
            return;
        }
        let mut senders: Vec<(String, f64)> = self
            .weights
            .iter()
            .filter(|(k, _)| k.starts_with(SENDER_PREFIX))
            .map(|(k, v)| (k.clone(), v.abs()))
            .collect();
        senders.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (k, _) in senders.into_iter().take(n - MAX_SENDERS) {
            self.weights.remove(&k);
        }
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
    fn one_dismissal_mutes_a_sender_not_others() {
        let mut m = ScoringModel::default();
        let from = |s: &str| Features {
            freshness: 0.8,
            direct: 1.0,
            question: 1.0,
            kind: Some(CandidateKind::UnansweredThread),
            sender: Some(s.to_owned()),
            ..Default::default()
        };
        let (cold, friend) = (from("Pitch@Vendor.test"), from("ana@acme.io"));
        let (c0, f0) = (m.score(&cold), m.score(&friend));
        m.update(&from("pitch@vendor.test"), Feedback::Dismissed);
        // Both lose the shared (kind, bias) part; the dismissed sender loses much more.
        let (dc, df) = (c0 - m.score(&cold), f0 - m.score(&friend));
        assert!(dc > df + 0.2, "{dc} vs {df}");
        assert!(m.explain(&cold).iter().any(|f| f.name == "sender"));
    }
}
