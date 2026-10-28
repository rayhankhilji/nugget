//! `bench/proactive` (M5): proactive cards over a 30-day fixture world.
//!
//! A fictional user's mail, calendar, notes and receipts (`bench/fixtures/proactive/`) are fed to
//! a [`ProactiveEngine`] one morning at a time. A simulated user accepts cards that match the gold
//! labels and dismisses the rest, with noise, and the engine learns from that feedback. The suite
//! scores card precision and recall, false positives on distractors, dedup, the learning curve,
//! dry-run coverage of actions and the approval gate, against a Sentient-style recency baseline.
//! Methodology: `bench/README.md`.
//!
//! `--backend fake` (the default) scores the in-crate [`reference::ReferenceEngine`].
//! `--backend real` scores the real `nugget-proactive` crate through [`real::RealEngine`] (an
//! encrypted temporary store per run, no network), with the reference engine reported next to
//! it as the bar to clear.

pub mod baseline;
pub mod engine;
pub mod generate;
pub mod real;
pub mod reference;
pub mod sim;
pub mod world;

use std::collections::BTreeMap;

use crate::report::{Direction, Metric};
use crate::runner::{Backend, BenchError, Benchmark, RunContext};

pub use engine::ProactiveEngine;
use world::{CardKind, DistractorCategory, Gold, Item, World};

/// Cards per morning brief.
pub const K: usize = 5;
/// Probability the simulated user's accept or dismiss is flipped.
pub const USER_NOISE: f64 = 0.1;

pub struct ProactiveBench;

impl Benchmark for ProactiveBench {
    fn name(&self) -> &'static str {
        "proactive"
    }

    fn description(&self) -> &'static str {
        "Proactive cards: precision@5, recall, distractor false positives, dedup, learning curve, dry-run coverage and approval gate (M5)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let world = world::load_world()
            .map_err(|e| BenchError::Failed(format!("fixtures/proactive/world.json: {e}")))?;
        let gold = world::load_gold()
            .map_err(|e| BenchError::Failed(format!("fixtures/proactive/gold.json: {e}")))?;
        let runs = ctx.scale().pick(3, 10, 30, 100);
        record_params(ctx, &world, &gold, runs);

        let seeds: Vec<u64> = (0..runs as u64)
            .map(|i| ctx.seed().wrapping_mul(1_000_003).wrapping_add(i))
            .collect();
        let arm = |make: &dyn Fn() -> Box<dyn ProactiveEngine>| {
            let scores: Vec<BTreeMap<String, f64>> = seeds
                .iter()
                .map(|&seed| {
                    let mut engine = make();
                    let run = sim::run(&world, &gold, engine.as_mut(), sim_config(seed));
                    sim::score(&world, &gold, &run, K)
                })
                .collect();
            sim::mean(&scores)
        };
        let user = world.user.email.clone();
        let reference = arm(&|| Box::new(reference::ReferenceEngine::new(&user, true)));
        let base = arm(&|| Box::new(baseline::RecencyJudge::new(&user)));
        ctx.param("baseline", "recency_judge");
        if ctx.backend() == Backend::Real {
            let ours = real_arm(&world, &gold, &seeds, true)?;
            let frozen = real_arm(&world, &gold, &seeds, false)?;
            ctx.param("backend", "real");
            ctx.param("engine", "nugget-proactive");
            ctx.param("store", "encrypted, temporary, one per user seed");
            ctx.param("brief_hour_utc", real::BRIEF_HOUR);
            ctx.param("judge", "none");
            ctx.param("drafter", "deterministic stub");
            ctx.param("policy", "PolicyEngine::default()");
            let mut out = metrics(&ours, &frozen, &base);
            out.extend(reference_metrics(&ours, &reference));
            return Ok(out);
        }
        let frozen = arm(&|| Box::new(reference::ReferenceEngine::new(&user, false)));
        ctx.param("engine", "reference");
        Ok(metrics(&reference, &frozen, &base))
    }
}

fn sim_config(seed: u64) -> sim::SimConfig {
    sim::SimConfig {
        k: K,
        noise: USER_NOISE,
        seed,
    }
}

/// The real crate over every user seed: mean scores. Any store or crate error fails the run.
fn real_arm(
    world: &World,
    gold: &Gold,
    seeds: &[u64],
    learning: bool,
) -> Result<BTreeMap<String, f64>, BenchError> {
    let mut scores = Vec::with_capacity(seeds.len());
    for &seed in seeds {
        let mut engine = real::RealEngine::new(&world.user.email, learning, seed)?;
        let run = sim::run(world, gold, &mut engine, sim_config(seed));
        if let Some(e) = engine.errors().first() {
            return Err(BenchError::Failed(format!(
                "nugget-proactive (seed {seed}, {} errors): {e}",
                engine.errors().len()
            )));
        }
        scores.push(sim::score(world, gold, &run, K));
    }
    Ok(sim::mean(&scores))
}

/// The real crate next to the reference engine on the same seeds (`--backend real` only).
fn reference_metrics(
    ours: &BTreeMap<String, f64>,
    reference: &BTreeMap<String, f64>,
) -> Vec<Metric> {
    use Direction::Info;
    let get = |m: &BTreeMap<String, f64>, k: &str| m.get(k).copied().unwrap_or(0.0);
    let p = format!("precision@{K}");
    let mut out: Vec<Metric> = [
        (p.as_str(), "precision@5"),
        ("recall", "recall of gold cards"),
        ("distractors.fpr", "distractor false-positive rate"),
        ("precision.days_21_30", "precision days 21-30"),
    ]
    .into_iter()
    .map(|(key, what)| {
        let name = match key {
            "distractors.fpr" => "reference.distractors.false_positive_rate".to_owned(),
            _ => format!("reference.{key}"),
        };
        Metric::new(
            name,
            get(reference, key),
            "ratio",
            Info,
            format!("In-crate reference engine (--backend fake) on the same seeds: {what}"),
        )
    })
    .collect();
    out.extend([
        Metric::new(
            format!("comparison.{p}_vs_reference"),
            get(ours, &p) - get(reference, &p),
            "ratio",
            Info,
            "precision@5 of nugget-proactive minus the reference engine's",
        ),
        Metric::new(
            "comparison.recall_vs_reference",
            get(ours, "recall") - get(reference, "recall"),
            "ratio",
            Info,
            "Recall of nugget-proactive minus the reference engine's",
        ),
    ]);
    out
}

fn record_params(ctx: &mut RunContext, world: &World, gold: &Gold, runs: usize) {
    let count = |f: fn(&Item) -> bool| world.items.iter().filter(|i| f(i)).count();
    ctx.param("backend", "reference_engine");
    ctx.param("days", world.days);
    ctx.param("k", K);
    ctx.param("user_noise", USER_NOISE);
    ctx.param("user_seeds", runs);
    ctx.param("judge_items", baseline::JUDGE_ITEMS);
    ctx.param("items.mail", count(|i| matches!(i, Item::Mail(_))));
    ctx.param("items.events", count(|i| matches!(i, Item::Event(_))));
    ctx.param(
        "items.cancellations",
        count(|i| matches!(i, Item::EventCancelled(_))),
    );
    ctx.param("items.notes", count(|i| matches!(i, Item::Note(_))));
    ctx.param("items.receipts", count(|i| matches!(i, Item::Receipt(_))));
    ctx.param("gold.cards", gold.cards.len());
    for kind in CardKind::ALL {
        ctx.param(
            &format!("gold.{}", kind.as_str()),
            gold.cards.iter().filter(|g| g.kind == kind).count(),
        );
    }
    ctx.param("distractors", gold.distractors.len());
    for c in DistractorCategory::ALL {
        ctx.param(
            &format!("distractors.{}", c.as_str()),
            gold.distractors.iter().filter(|d| d.category == c).count(),
        );
    }
}

fn metrics(
    ours: &BTreeMap<String, f64>,
    frozen: &BTreeMap<String, f64>,
    base: &BTreeMap<String, f64>,
) -> Vec<Metric> {
    use Direction::{HigherIsBetter as Hi, Info, LowerIsBetter as Lo};
    let get = |m: &BTreeMap<String, f64>, k: &str| m.get(k).copied().unwrap_or(0.0);
    let p = format!("precision@{K}");
    let mut out = vec![
        Metric::new(
            format!("cards.{p}"),
            get(ours, &p),
            "ratio",
            Hi,
            "Mean over days with cards of (cards matching a gold label that day) / (cards shown), k = 5",
        )
        .with_target(0.7),
        Metric::new(
            format!("cards.{p}.micro"),
            get(ours, &format!("{p}.micro")),
            "ratio",
            Info,
            "All shown cards that match a gold label / all shown cards",
        ),
        Metric::new(
            "cards.recall",
            get(ours, "recall"),
            "ratio",
            Hi,
            "Gold cards shown at least once inside their window",
        )
        .with_target(0.8),
    ];
    for kind in CardKind::ALL {
        let k = format!("recall.{}", kind.as_str());
        out.push(Metric::new(
            format!("cards.{k}"),
            get(ours, &k),
            "ratio",
            Info,
            "Recall for one card kind",
        ));
    }
    out.extend([
        Metric::new(
            "cards.shown_per_day",
            get(ours, "shown_per_day"),
            "cards",
            Info,
            "Mean cards per morning brief (at most 5)",
        ),
        Metric::new(
            "cards.acceptance_rate",
            get(ours, "acceptance_rate"),
            "ratio",
            Info,
            "Accepted / shown: what the product can track live (PLAN M5: precision of accepted cards)",
        ),
        Metric::new(
            "cards.accepted_precision",
            get(ours, "accepted_precision"),
            "ratio",
            Info,
            "Accepted cards that match a gold label / accepted cards (the rest are user noise)",
        ),
        Metric::new(
            "cards.duplicate_shows",
            get(ours, "duplicate_shows"),
            "cards",
            Lo,
            "Shows of a (kind, subject) already shown on an earlier or the same day, per run",
        )
        .with_target(0.0),
        Metric::new(
            "distractors.false_positive_rate",
            get(ours, "distractors.fpr"),
            "ratio",
            Lo,
            "Distractors (newsletters, notifications, bulk mail, answered threads, completed commitments, cancelled meetings, spam renewals) that produced any card",
        )
        .with_target(0.05),
        Metric::new(
            "distractors.header_flagged.false_positives",
            get(ours, "distractors.header_flagged.false_positives"),
            "items",
            Lo,
            "Newsletters and automated notifications that produced a card: these need no feedback to filter",
        )
        .with_target(0.0),
    ]);
    for c in DistractorCategory::ALL {
        let k = format!("distractors.{}.fpr", c.as_str());
        out.push(Metric::new(
            k.clone(),
            get(ours, &k),
            "ratio",
            Info,
            "False-positive rate for one distractor category",
        ));
    }
    let early = get(ours, "precision.days_01_10");
    let late = get(ours, "precision.days_21_30");
    let frozen_late = get(frozen, "precision.days_21_30");
    out.extend([
        Metric::new(
            "learning.precision.days_01_10",
            early,
            "ratio",
            Info,
            "Daily precision@5, days 1-10",
        ),
        Metric::new(
            "learning.precision.days_21_30",
            late,
            "ratio",
            Info,
            "Daily precision@5, days 21-30",
        ),
        Metric::new(
            "learning.precision_gain",
            late - early,
            "ratio",
            Hi,
            "Precision days 21-30 minus days 1-10: learning from feedback must not lose precision over the month",
        )
        .with_target(0.0),
        Metric::new(
            "learning.frozen.precision.days_21_30",
            frozen_late,
            "ratio",
            Info,
            "Ablation: the same engine with learning off, days 21-30",
        ),
        Metric::new(
            "learning.gain_vs_frozen",
            late - frozen_late,
            "ratio",
            Hi,
            "Precision days 21-30, learning on minus learning off: what feedback buys",
        )
        .with_target(0.0),
        Metric::new(
            "learning.frozen.distractors.false_positive_rate",
            get(frozen, "distractors.fpr"),
            "ratio",
            Info,
            "Ablation: distractor false-positive rate with learning off",
        ),
        Metric::new(
            "actions.count",
            get(ours, "actions"),
            "actions",
            Info,
            "Cards shown with an action, per run",
        ),
        Metric::new(
            "actions.dry_run_coverage",
            get(ours, "dry_run_coverage"),
            "ratio",
            Hi,
            "Shown actions with a dry-run diff (target and different before and after) / shown actions",
        )
        .with_target(1.0),
        Metric::new(
            "actions.diff_fidelity",
            get(ours, "diff_fidelity"),
            "ratio",
            Hi,
            "Approved executions whose applied change equals the diff the user approved",
        )
        .with_target(1.0),
        Metric::new(
            "actions.executions",
            get(ours, "executions"),
            "actions",
            Info,
            "Approved executions, per run",
        ),
        Metric::new(
            "actions.approved_failures",
            get(ours, "approved_failures"),
            "actions",
            Info,
            "Approved actions the engine failed to execute, per run",
        ),
        Metric::new(
            "actions.executed_without_approval",
            get(ours, "executed_without_approval"),
            "actions",
            Lo,
            "Executions with no approval, another card's approval, or a replayed approval: from the audit log and from probes",
        )
        .with_target(0.0),
        Metric::new(
            "actions.approval_probes",
            get(ours, "approval_probes"),
            "probes",
            Info,
            "Unapproved execution attempts the benchmark made, per run (all must be refused)",
        ),
    ]);
    let b = "baseline.recency_judge";
    for (key, unit, desc) in [
        (
            p.as_str(),
            "ratio",
            "Recency-ordered judge, at most 5 cards: precision@5",
        ),
        (
            "recall",
            "ratio",
            "Recency-ordered judge: recall of gold cards",
        ),
        (
            "distractors.fpr",
            "ratio",
            "Recency-ordered judge: distractor false-positive rate",
        ),
        (
            "duplicate_shows",
            "cards",
            "Recency-ordered judge: repeated shows, per run (no candidate store)",
        ),
        (
            "precision.days_21_30",
            "ratio",
            "Recency-ordered judge: precision days 21-30 (no learning)",
        ),
    ] {
        let name = match key {
            "distractors.fpr" => format!("{b}.distractors.false_positive_rate"),
            "duplicate_shows" => format!("{b}.duplicate_shows"),
            _ => format!("{b}.{key}"),
        };
        out.push(Metric::new(name, get(base, key), unit, Info, desc));
    }
    out.extend([
        Metric::new(
            format!("comparison.{p}_vs_baseline"),
            get(ours, &p) - get(base, &p),
            "ratio",
            Hi,
            "precision@5 minus the recency judge's: must be at least the baseline",
        )
        .with_target(0.0),
        Metric::new(
            "comparison.recall_vs_baseline",
            get(ours, "recall") - get(base, "recall"),
            "ratio",
            Info,
            "Recall minus the recency judge's",
        ),
    ]);
    out
}

#[cfg(test)]
mod tests;
