//! Feedback-learned scoring: an online logistic model over [`Features`], updated from accept,
//! dismiss and snooze events, persisted with the cards and explainable per card.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::candidate::{FEATURE_NAMES, Features};

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

    /// Probability that the user acts on a card with these features.
    pub fn score(&self, f: &Features) -> f64 {
        let w = self.weight_vec();
        let z: f64 = f.vector().iter().zip(w).map(|(x, w)| x * w).sum();
        sigmoid(z)
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
        self.updates += 1;
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
}
