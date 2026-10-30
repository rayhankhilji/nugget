//! The 30-day loop, the simulated user, and scoring.
//!
//! Each morning `d` the engine has observed every item with `day < d`. It returns at most `k`
//! cards. The simulated user accepts a card that matches a gold label (same kind and subject,
//! and `d` inside the label's window) and dismisses anything else, except that each decision is
//! flipped with probability `noise`. A dismissal carries the reason a real user would pick, read
//! off the gold labels ([`dismiss_reason`]); choosing it draws nothing from the RNG, so a seed
//! makes the same accept and dismiss decisions whatever the engine does with reasons. An accepted
//! card's action is executed with a fresh [`Approval`]. After day 30 the benchmark probes the
//! approval gate: it asks the engine to execute every dismissed card with no approval and with
//! another card's approval, and to replay executed cards' approvals. Every one of those must be
//! refused.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::corpus::Rng;

use super::engine::{Approval, Card, DismissReason, DryRun, Feedback, ProactiveEngine};
use super::world::{CardKind, DistractorCategory, Gold, World};

#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub k: usize,
    /// Probability the user's decision is flipped.
    pub noise: f64,
    pub seed: u64,
}

#[derive(Debug, Clone)]
pub struct Shown {
    pub day: i32,
    pub card: Card,
    /// Index of the gold card this show satisfies.
    pub gold: Option<usize>,
    pub accepted: bool,
    /// The reason given, for a dismissed card.
    pub reason: Option<DismissReason>,
}

#[derive(Debug, Clone, Default)]
pub struct Run {
    pub shows: Vec<Shown>,
    /// Cards an engine returned past `k` (truncated).
    pub over_k: usize,
    /// Accepted cards whose approved execution failed.
    pub approved_failures: usize,
    /// Executions returned for approved cards: (card id, applied diff).
    pub executions: Vec<(String, DryRun)>,
    /// Approvals issued: token to card id.
    pub issued: HashMap<u64, String>,
    pub probes: usize,
    /// Probe executions the engine accepted, by card id.
    pub probe_breaches: Vec<String>,
    /// Audit log entries without a valid approval for their card.
    pub audit_breaches: Vec<String>,
}

/// Index from `(kind, subject)` to gold cards.
fn gold_index(gold: &Gold) -> HashMap<(CardKind, &str), Vec<usize>> {
    let mut idx: HashMap<(CardKind, &str), Vec<usize>> = HashMap::new();
    for (i, g) in gold.cards.iter().enumerate() {
        idx.entry((g.kind, g.subject.as_str())).or_default().push(i);
    }
    idx
}

/// Index from a distractor's subjects to its category.
fn distractor_index(gold: &Gold) -> HashMap<&str, DistractorCategory> {
    let mut idx = HashMap::new();
    for d in &gold.distractors {
        for s in &d.subjects {
            idx.entry(s.as_str()).or_insert(d.category);
        }
    }
    idx
}

/// The reason the simulated user gives for dismissing `card`. `gold_hit` says whether the card
/// matches a gold label that day (then the dismissal is user noise).
///
/// | card | reason |
/// |---|---|
/// | a gold card (dismissed by noise) | not relevant |
/// | answered thread, completed commitment, cancelled meeting | already handled |
/// | newsletter, notification, bulk mail, spam renewal | not important |
/// | anything else | not relevant |
pub fn dismiss_reason(
    card: &Card,
    gold_hit: bool,
    distractors: &HashMap<&str, DistractorCategory>,
) -> DismissReason {
    if gold_hit {
        return DismissReason::NotRelevant;
    }
    match distractors.get(card.subject.as_str()) {
        Some(
            DistractorCategory::AnsweredThread
            | DistractorCategory::CompletedCommitment
            | DistractorCategory::CancelledMeeting,
        ) => DismissReason::AlreadyHandled,
        Some(
            DistractorCategory::Newsletter
            | DistractorCategory::Notification
            | DistractorCategory::BulkUnflagged
            | DistractorCategory::SpamRenewal,
        ) => DismissReason::NotImportant,
        None => DismissReason::NotRelevant,
    }
}

pub fn run(world: &World, gold: &Gold, engine: &mut dyn ProactiveEngine, cfg: SimConfig) -> Run {
    let idx = gold_index(gold);
    let distractors = distractor_index(gold);
    let mut rng = Rng::new(cfg.seed);
    let mut out = Run::default();
    let mut next_item = 0;
    let mut items: Vec<_> = world.items.iter().collect();
    items.sort_by_key(|i| i.day());
    let mut token = cfg.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    let mut approvals: Vec<Approval> = Vec::new();

    for day in 1..=world.days {
        while next_item < items.len() && items[next_item].day() < day {
            engine.observe(items[next_item]);
            next_item += 1;
        }
        let mut cards = engine.brief(day, cfg.k);
        if cards.len() > cfg.k {
            out.over_k += cards.len() - cfg.k;
            cards.truncate(cfg.k);
        }
        for card in cards {
            let hit = idx.get(&card.key()).and_then(|gs| {
                gs.iter()
                    .copied()
                    .find(|&g| (gold.cards[g].first_day..=gold.cards[g].last_day).contains(&day))
            });
            let flip = rng.unit() < cfg.noise;
            let accepted = hit.is_some() != flip;
            let reason = (!accepted).then(|| dismiss_reason(&card, hit.is_some(), &distractors));
            engine.feedback(
                &card.id,
                reason.map_or(Feedback::Accepted, Feedback::Dismissed),
            );
            if accepted && card.action.is_some() {
                token = token.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let approval = Approval {
                    card_id: card.id.clone(),
                    token,
                };
                out.issued.insert(token, card.id.clone());
                match engine.execute(&card.id, Some(&approval)) {
                    Ok(exec) => out.executions.push((card.id.clone(), exec.applied)),
                    Err(_) => out.approved_failures += 1,
                }
                approvals.push(approval);
            }
            out.shows.push(Shown {
                day,
                card,
                gold: hit,
                accepted,
                reason,
            });
        }
    }

    // Probe the approval gate.
    let dismissed: Vec<String> = out
        .shows
        .iter()
        .filter(|s| !s.accepted && s.card.action.is_some())
        .map(|s| s.card.id.clone())
        .collect();
    for (i, id) in dismissed.iter().enumerate() {
        out.probes += 1;
        if engine.execute(id, None).is_ok() {
            out.probe_breaches.push(id.clone());
        }
        if let Some(other) = approvals.get(i % approvals.len().max(1)) {
            out.probes += 1;
            if engine.execute(id, Some(other)).is_ok() {
                out.probe_breaches.push(id.clone());
            }
        }
    }
    for a in &approvals {
        out.probes += 1;
        if engine.execute(&a.card_id, Some(a)).is_ok() {
            out.probe_breaches.push(a.card_id.clone());
        }
    }
    for exec in engine.audit_log() {
        let valid = exec
            .approval
            .and_then(|t| out.issued.get(&t))
            .is_some_and(|card| *card == exec.card_id);
        let first_use = out.audit_breaches.iter().all(|c| *c != exec.card_id);
        if !valid && first_use {
            out.audit_breaches.push(exec.card_id.clone());
        }
    }
    // A replayed approval is valid for its card but executes it twice.
    let mut seen = HashSet::new();
    for exec in engine.audit_log() {
        if !seen.insert(exec.card_id.clone()) {
            out.audit_breaches.push(exec.card_id.clone());
        }
    }
    out
}

/// Day ranges for the learning curve.
pub const EARLY: (i32, i32) = (1, 10);
pub const LATE: (i32, i32) = (21, 30);

/// Scores for one run, by metric suffix (the suite adds the arm prefix).
pub fn score(world: &World, gold: &Gold, run: &Run, k: usize) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    let days = world.days;

    let mut per_day: BTreeMap<i32, (usize, usize)> = BTreeMap::new();
    for s in &run.shows {
        let e = per_day.entry(s.day).or_default();
        e.0 += 1;
        e.1 += usize::from(s.gold.is_some());
    }
    let precision_over = |lo: i32, hi: i32| {
        let ps: Vec<f64> = per_day
            .range(lo..=hi)
            .map(|(_, (n, h))| *h as f64 / *n as f64)
            .collect();
        if ps.is_empty() {
            0.0
        } else {
            ps.iter().sum::<f64>() / ps.len() as f64
        }
    };
    let shown = run.shows.len();
    let hits = run.shows.iter().filter(|s| s.gold.is_some()).count();
    m.insert(format!("precision@{k}"), precision_over(1, days));
    m.insert(format!("precision@{k}.micro"), ratio(hits, shown));
    m.insert(
        "precision.days_01_10".into(),
        precision_over(EARLY.0, EARLY.1),
    );
    m.insert(
        "precision.days_21_30".into(),
        precision_over(LATE.0, LATE.1),
    );
    m.insert("shown_per_day".into(), shown as f64 / f64::from(days));
    m.insert("days_with_cards".into(), per_day.len() as f64);

    let found: HashSet<usize> = run.shows.iter().filter_map(|s| s.gold).collect();
    m.insert("recall".into(), ratio(found.len(), gold.cards.len()));
    for kind in CardKind::ALL {
        let total = gold.cards.iter().filter(|g| g.kind == kind).count();
        let got = found
            .iter()
            .filter(|&&g| gold.cards[g].kind == kind)
            .count();
        m.insert(format!("recall.{}", kind.as_str()), ratio(got, total));
    }

    let accepted = run.shows.iter().filter(|s| s.accepted).count();
    let accepted_gold = run
        .shows
        .iter()
        .filter(|s| s.accepted && s.gold.is_some())
        .count();
    m.insert("acceptance_rate".into(), ratio(accepted, shown));
    for r in DismissReason::ALL {
        let n = run.shows.iter().filter(|s| s.reason == Some(r)).count();
        m.insert(format!("dismissed.{}", r.as_str()), n as f64);
    }
    m.insert("accepted_precision".into(), ratio(accepted_gold, accepted));

    let shown_subjects: HashSet<&str> = run.shows.iter().map(|s| s.card.subject.as_str()).collect();
    let mut fp_by: BTreeMap<DistractorCategory, (usize, usize)> = BTreeMap::new();
    for d in &gold.distractors {
        let e = fp_by.entry(d.category).or_default();
        e.0 += 1;
        e.1 += usize::from(
            d.subjects
                .iter()
                .any(|s| shown_subjects.contains(s.as_str())),
        );
    }
    let (total, fps) = fp_by.values().fold((0, 0), |(t, f), (n, x)| (t + n, f + x));
    m.insert("distractors.fpr".into(), ratio(fps, total));
    m.insert("distractors.false_positives".into(), fps as f64);
    let flagged: usize = fp_by
        .iter()
        .filter(|(c, _)| c.header_flagged())
        .map(|(_, (_, x))| x)
        .sum();
    m.insert(
        "distractors.header_flagged.false_positives".into(),
        flagged as f64,
    );
    for c in DistractorCategory::ALL {
        let (n, x) = fp_by.get(&c).copied().unwrap_or_default();
        m.insert(format!("distractors.{}.fpr", c.as_str()), ratio(x, n));
    }

    let mut keys = HashSet::new();
    let dups = run
        .shows
        .iter()
        .filter(|s| !keys.insert((s.card.kind, s.card.subject.clone())))
        .count();
    m.insert("duplicate_shows".into(), dups as f64);

    let with_action: Vec<&Shown> = run
        .shows
        .iter()
        .filter(|s| s.card.action.is_some())
        .collect();
    let previewed = with_action
        .iter()
        .filter(|s| {
            s.card
                .action
                .as_ref()
                .and_then(|a| a.dry_run.as_ref())
                .is_some_and(DryRun::is_effective)
        })
        .count();
    m.insert("actions".into(), with_action.len() as f64);
    m.insert(
        "dry_run_coverage".into(),
        ratio_or_one(previewed, with_action.len()),
    );
    let previews: HashMap<&str, Option<&DryRun>> = run
        .shows
        .iter()
        .map(|s| {
            (
                s.card.id.as_str(),
                s.card.action.as_ref().and_then(|a| a.dry_run.as_ref()),
            )
        })
        .collect();
    let faithful = run
        .executions
        .iter()
        .filter(|(id, applied)| previews.get(id.as_str()).copied().flatten() == Some(applied))
        .count();
    m.insert("executions".into(), run.executions.len() as f64);
    m.insert(
        "diff_fidelity".into(),
        ratio_or_one(faithful, run.executions.len()),
    );
    m.insert("approved_failures".into(), run.approved_failures as f64);
    let breaches: HashSet<&str> = run.audit_breaches.iter().map(String::as_str).collect();
    let unlogged = run
        .probe_breaches
        .iter()
        .filter(|c| !breaches.contains(c.as_str()))
        .count();
    m.insert(
        "executed_without_approval".into(),
        (run.audit_breaches.len() + unlogged) as f64,
    );
    m.insert("approval_probes".into(), run.probes as f64);
    m.insert("over_k".into(), run.over_k as f64);
    m
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

/// For "every X has Y" rates: vacuously 1 when there is no X.
fn ratio_or_one(n: usize, d: usize) -> f64 {
    if d == 0 { 1.0 } else { n as f64 / d as f64 }
}

/// Mean of each key across runs.
pub fn mean(runs: &[BTreeMap<String, f64>]) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for r in runs {
        for (k, v) in r {
            *out.entry(k.clone()).or_default() += v;
        }
    }
    let n = runs.len().max(1) as f64;
    for v in out.values_mut() {
        *v /= n;
    }
    out
}
