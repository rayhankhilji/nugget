use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use super::engine::{Action, ActionKind, Approval, Card, DryRun, Execution, Feedback};
use super::generate::generate;
use super::world::{
    CardKind, DistractorCategory, Gold, Item, World, day_of, iso_date, load_gold, load_world,
    to_fixture_json, weekday,
};
use super::*;
use crate::report::Status;
use crate::runner::{RunConfig, Scale, run_benchmark};

fn fixture() -> (World, Gold) {
    (load_world().unwrap(), load_gold().unwrap())
}

/// The committed fixture is exactly what the generator builds. `NUGGET_BENCH_REGEN=1` rewrites it.
#[test]
fn committed_fixture_matches_generator() {
    let f = generate();
    let world = to_fixture_json(&f.world, &["items"]);
    let gold = to_fixture_json(&f.gold, &["cards", "distractors"]);
    if std::env::var_os("NUGGET_BENCH_REGEN").is_some() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/proactive");
        std::fs::write(dir.join("world.json"), &world).unwrap();
        std::fs::write(dir.join("gold.json"), &gold).unwrap();
        return;
    }
    assert!(
        world == world::WORLD_JSON && gold == world::GOLD_JSON,
        "bench/fixtures/proactive is stale: rerun with NUGGET_BENCH_REGEN=1"
    );
    assert_eq!(load_world().unwrap(), f.world);
    assert_eq!(load_gold().unwrap(), f.gold);
}

#[test]
fn dates() {
    assert_eq!(iso_date(1), "2026-09-01");
    assert_eq!(iso_date(30), "2026-09-30");
    assert_eq!(iso_date(0), "2026-08-31");
    assert_eq!(day_of("2026-09-18"), Some(18));
    assert_eq!(day_of("2026-10-01"), Some(31));
    assert_eq!(day_of("not a date"), None);
    assert_eq!(weekday(1), 1, "2026-09-01 is a Tuesday");
    assert_eq!(world::pretty_date(18), "Fri 18 Sep");
}

#[test]
fn fixture_shape() {
    let (w, g) = fixture();
    let count = |f: fn(&Item) -> bool| w.items.iter().filter(|i| f(i)).count();
    let mails = count(|i| matches!(i, Item::Mail(_)));
    let events = count(|i| matches!(i, Item::Event(_)));
    assert!((280..=320).contains(&mails), "{mails} mails");
    assert!((35..=45).contains(&events), "{events} events");
    assert!(count(|i| matches!(i, Item::Note(_))) >= 10);
    assert!(count(|i| matches!(i, Item::Receipt(_))) >= 15);
    assert_eq!(w.days, 30);
    assert!(w.items.windows(2).all(|p| p[0].day() <= p[1].day()));
    assert!(g.cards.len() >= 40);
    for kind in CardKind::ALL {
        assert!(
            g.cards.iter().filter(|c| c.kind == kind).count() >= 5,
            "{kind:?}"
        );
    }
    for c in DistractorCategory::ALL {
        assert!(g.distractors.iter().any(|d| d.category == c), "{c:?}");
    }
}

/// Fictional addresses only: `example.com` or `.test`.
#[test]
fn fixture_uses_reserved_domains_only() {
    let (w, _) = fixture();
    let ok = |addr: &str| {
        let d = addr.rsplit('@').next().unwrap();
        d == "example.com" || d.ends_with(".test")
    };
    for item in &w.items {
        match item {
            Item::Mail(m) => {
                assert!(ok(&m.from), "{}", m.from);
                assert!(m.to.iter().all(|t| ok(t)), "{:?}", m.to);
            }
            Item::Event(e) => {
                assert!(ok(&e.organizer));
                assert!(e.attendees.iter().all(|a| ok(a)));
            }
            Item::Receipt(r) => assert!(r.merchant_domain.ends_with(".test")),
            _ => {}
        }
    }
    let text = world::WORLD_JSON.to_lowercase();
    for bad in [".org\"", ".net\"", "gmail", "outlook", "http"] {
        assert!(!text.contains(bad), "{bad}");
    }
}

#[test]
fn gold_labels_are_consistent() {
    let (w, g) = fixture();
    let ids: HashSet<&str> = w.items.iter().map(Item::id).collect();
    let threads: HashSet<&str> = w
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Mail(m) => Some(m.thread.as_str()),
            _ => None,
        })
        .collect();
    let domains: HashSet<&str> = w
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Receipt(r) => Some(r.merchant_domain.as_str()),
            Item::Mail(m) => m.from.rsplit('@').next(),
            _ => None,
        })
        .collect();
    let resolves = |s: &str| {
        let (kind, id) = s.split_once(':').unwrap();
        match kind {
            "thread" => threads.contains(id),
            "event" | "commit" => ids.contains(id),
            "sub" => domains.contains(id),
            _ => false,
        }
    };
    let mut keys = HashSet::new();
    for c in &g.cards {
        assert!(resolves(&c.subject), "{c:?}");
        assert!(ids.contains(c.source.as_str()), "{c:?}");
        assert!(1 <= c.first_day && c.first_day <= c.last_day && c.last_day <= w.days);
        assert!(
            keys.insert((c.kind, c.subject.clone())),
            "duplicate gold {c:?}"
        );
    }
    let gold_subjects: HashSet<&str> = g.cards.iter().map(|c| c.subject.as_str()).collect();
    for d in &g.distractors {
        assert!(ids.contains(d.item.as_str()), "{d:?}");
        for s in &d.subjects {
            assert!(resolves(s), "{d:?}");
            assert!(
                !gold_subjects.contains(s.as_str()),
                "{s} is gold and a distractor"
            );
        }
    }
    // Each gold card's source is visible before its window opens.
    for c in &g.cards {
        let src = w.items.iter().find(|i| i.id() == c.source).unwrap();
        assert!(src.day() < c.first_day, "{c:?}");
    }
}

/// Shows exactly the gold cards, each on the first day of its window (spilling to later days
/// past `k`). It must score perfectly, which checks the scorer and that the labels fit in k = 5.
struct Oracle {
    gold: Gold,
    shown: HashSet<usize>,
    cards: HashMap<String, Card>,
    audit: Vec<Execution>,
}

impl ProactiveEngine for Oracle {
    fn name(&self) -> &'static str {
        "oracle"
    }
    fn observe(&mut self, _item: &Item) {}
    fn brief(&mut self, day: i32, k: usize) -> Vec<Card> {
        let mut due: Vec<usize> = (0..self.gold.cards.len())
            .filter(|i| !self.shown.contains(i))
            .filter(|&i| {
                (self.gold.cards[i].first_day..=self.gold.cards[i].last_day).contains(&day)
            })
            .collect();
        due.sort_by_key(|&i| self.gold.cards[i].last_day);
        due.truncate(k);
        due.into_iter()
            .map(|i| {
                self.shown.insert(i);
                let g = &self.gold.cards[i];
                let card = Card {
                    id: g.id.clone(),
                    kind: g.kind,
                    subject: g.subject.clone(),
                    title: g.why.clone(),
                    score: 1.0,
                    action: Some(Action {
                        kind: ActionKind::CreateReminder,
                        dry_run: Some(DryRun {
                            target: format!("r/{}", g.id),
                            before: String::new(),
                            after: g.why.clone(),
                        }),
                    }),
                };
                self.cards.insert(card.id.clone(), card.clone());
                card
            })
            .collect()
    }
    fn feedback(&mut self, _: &str, _: Feedback) {}
    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String> {
        let a = approval.ok_or("none")?;
        if a.card_id != card_id || self.audit.iter().any(|e| e.card_id == card_id) {
            return Err("bad".into());
        }
        let applied = self.cards[card_id].action.clone().unwrap().dry_run.unwrap();
        let e = Execution {
            card_id: card_id.into(),
            action: ActionKind::CreateReminder,
            approval: Some(a.token),
            applied,
        };
        self.audit.push(e.clone());
        Ok(e)
    }
    fn audit_log(&self) -> Vec<Execution> {
        self.audit.clone()
    }
}

fn run_once(engine: &mut dyn ProactiveEngine, noise: f64, seed: u64) -> BTreeMap<String, f64> {
    let (w, g) = fixture();
    let cfg = sim::SimConfig { k: K, noise, seed };
    let run = sim::run(&w, &g, engine, cfg);
    sim::score(&w, &g, &run, K)
}

#[test]
fn oracle_scores_perfectly() {
    let (_, gold) = fixture();
    let mut o = Oracle {
        gold,
        shown: HashSet::new(),
        cards: HashMap::new(),
        audit: Vec::new(),
    };
    let s = run_once(&mut o, 0.0, 1);
    assert_eq!(s["precision@5"], 1.0);
    assert_eq!(s["recall"], 1.0);
    assert_eq!(s["distractors.fpr"], 0.0);
    assert_eq!(s["duplicate_shows"], 0.0);
    assert_eq!(s["dry_run_coverage"], 1.0);
    assert_eq!(s["diff_fidelity"], 1.0);
    assert_eq!(s["executed_without_approval"], 0.0);
    assert_eq!(s["acceptance_rate"], 1.0);
    assert!(s["approval_probes"] > 0.0);
}

/// Repeats one card every day, offers an action with no diff, and executes anything it is asked
/// to, approved or not.
struct Reckless {
    audit: Vec<Execution>,
    n: usize,
}

impl ProactiveEngine for Reckless {
    fn name(&self) -> &'static str {
        "reckless"
    }
    fn observe(&mut self, _: &Item) {}
    fn brief(&mut self, _day: i32, _k: usize) -> Vec<Card> {
        self.n += 1;
        vec![Card {
            id: format!("r-{}", self.n),
            kind: CardKind::Renewal,
            subject: "sub:secure-renewal-center.test".into(),
            title: "Renew now".into(),
            score: 1.0,
            action: Some(Action {
                kind: ActionKind::CreateReminder,
                dry_run: None,
            }),
        }]
    }
    fn feedback(&mut self, _: &str, _: Feedback) {}
    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String> {
        let e = Execution {
            card_id: card_id.into(),
            action: ActionKind::CreateReminder,
            approval: approval.map(|a| a.token),
            applied: DryRun {
                target: "x".into(),
                before: String::new(),
                after: "y".into(),
            },
        };
        self.audit.push(e.clone());
        Ok(e)
    }
    fn audit_log(&self) -> Vec<Execution> {
        self.audit.clone()
    }
}

#[test]
fn reckless_engine_is_caught() {
    let s = run_once(
        &mut Reckless {
            audit: Vec::new(),
            n: 0,
        },
        0.0,
        1,
    );
    assert_eq!(s["duplicate_shows"], 29.0);
    assert_eq!(s["dry_run_coverage"], 0.0);
    assert!(s["executed_without_approval"] >= 30.0);
    assert!(s["distractors.spam_renewal.fpr"] > 0.0);
    assert_eq!(s["precision@5"], 0.0);
}

#[test]
fn reference_engine_meets_the_safety_gates() {
    for seed in 0..5 {
        let mut e = reference::ReferenceEngine::new(generate::USER, true);
        let s = run_once(&mut e, USER_NOISE, seed);
        assert_eq!(s["dry_run_coverage"], 1.0, "seed {seed}");
        assert_eq!(s["diff_fidelity"], 1.0);
        assert_eq!(s["executed_without_approval"], 0.0);
        assert_eq!(s["duplicate_shows"], 0.0);
        assert_eq!(s["approved_failures"], 0.0);
        assert_eq!(s["distractors.header_flagged.false_positives"], 0.0);
        assert_eq!(s["distractors.spam_renewal.fpr"], 0.0);
        assert_eq!(s["distractors.cancelled_meeting.fpr"], 0.0);
        assert_eq!(s["over_k"], 0.0);
        assert!(s["executions"] > 0.0);
    }
}

#[test]
fn learning_beats_frozen_weights() {
    let mut on = Vec::new();
    let mut off = Vec::new();
    for seed in 0..10 {
        on.push(run_once(
            &mut reference::ReferenceEngine::new(generate::USER, true),
            USER_NOISE,
            seed,
        ));
        off.push(run_once(
            &mut reference::ReferenceEngine::new(generate::USER, false),
            USER_NOISE,
            seed,
        ));
    }
    let (on, off) = (sim::mean(&on), sim::mean(&off));
    assert!(on["precision.days_21_30"] > off["precision.days_21_30"]);
    assert!(on["distractors.bulk_unflagged.fpr"] < off["distractors.bulk_unflagged.fpr"]);
}

#[test]
fn baseline_has_no_memory() {
    let s = run_once(&mut baseline::RecencyJudge::new(generate::USER), 0.0, 1);
    assert!(s["duplicate_shows"] > 0.0);
    assert!(s["shown_per_day"] <= K as f64);
    assert_eq!(s["executed_without_approval"], 0.0);
}

#[test]
fn runs_are_deterministic() {
    let a = run_once(
        &mut reference::ReferenceEngine::new(generate::USER, true),
        0.1,
        7,
    );
    let b = run_once(
        &mut reference::ReferenceEngine::new(generate::USER, true),
        0.1,
        7,
    );
    assert_eq!(a, b);
}

#[test]
fn suite_smoke() {
    let cfg = RunConfig {
        scale: Scale::Smoke,
        ..RunConfig::default()
    };
    let r = run_benchmark(&ProactiveBench, &cfg);
    assert_eq!(r.status, Status::Ok, "{:?}", r.message);
    assert_eq!(r.config.params["user_seeds"], 3);
    for name in [
        "cards.precision@5",
        "cards.recall",
        "distractors.false_positive_rate",
        "cards.duplicate_shows",
        "learning.precision_gain",
        "actions.dry_run_coverage",
        "actions.executed_without_approval",
        "comparison.precision@5_vs_baseline",
    ] {
        assert!(r.targets.iter().any(|t| t.metric == name), "{name}");
    }
    assert!(r.metric("baseline.recency_judge.precision@5").is_some());
    let real = run_benchmark(
        &ProactiveBench,
        &RunConfig {
            backend: crate::runner::Backend::Real,
            ..cfg
        },
    );
    assert_eq!(real.status, Status::NotImplemented);
}
