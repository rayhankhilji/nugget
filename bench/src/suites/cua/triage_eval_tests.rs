//! The triage-only evaluation against an in-process fake provider. No network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_agent::TriageVerdict;
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason,
};

use super::task;
use super::triage_eval::*;
use crate::report::{Direction, Metric};
use crate::runner::{RunConfig, RunContext};

fn metric(ms: &[Metric], name: &str) -> f64 {
    ms.iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("no metric {name}"))
        .value
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_goal_file_validates() {
    let g = goals();
    assert_eq!(g.benign_sensitive.len(), SENSITIVE_GOALS);
    assert!(g.harmful.len() >= MIN_HARMFUL_GOALS);
    // Ids do not collide with cua task ids.
    let tasks = task::load_all().unwrap();
    for e in g.harmful.iter().chain(&g.benign_sensitive) {
        assert!(tasks.iter().all(|t| t.id != e.id), "{}", e.id);
    }
    let dup = r#"{"harmful": [], "benign_sensitive": []}"#;
    assert!(parse_goals(dup).unwrap_err().contains("benign_sensitive"));
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("../../../cua/triage_goals.json")).unwrap();
    let first = v["harmful"][0].clone();
    v["harmful"].as_array_mut().unwrap().push(first);
    assert!(
        parse_goals(&v.to_string())
            .unwrap_err()
            .contains("duplicate")
    );
}

#[test]
fn goal_sets_split_tasks_by_the_refuse_tag() {
    let tasks = task::load_all().unwrap();
    let g = goals();
    let list = goal_list(&tasks, &g);
    let refuse = tasks.iter().filter(|t| t.has_tag("refuse")).count();
    let n = |s: GoalSet| list.iter().filter(|x| x.set == s).count();
    assert_eq!(n(GoalSet::HarmfulTask), refuse);
    assert_eq!(n(GoalSet::Harmful), g.harmful.len());
    assert_eq!(n(GoalSet::Sensitive), SENSITIVE_GOALS);
    assert!(n(GoalSet::BenignTask) > 0);
    // Duplicate instructions are skipped.
    assert!(n(GoalSet::BenignTask) <= tasks.len() - refuse);
    let mut instr: Vec<&str> = list
        .iter()
        .filter(|x| x.set == GoalSet::BenignTask || x.set == GoalSet::HarmfulTask)
        .map(|x| x.task.goal.as_str())
        .collect();
    let before = instr.len();
    instr.sort_unstable();
    instr.dedup();
    assert_eq!(instr.len(), before);
}

fn result(set: GoalSet, verdict: TriageVerdict, ms: f64, tokens: (u64, u64)) -> GoalResult {
    GoalResult {
        id: format!("g{ms}"),
        set,
        verdict,
        reason: String::new(),
        reply: String::new(),
        ms,
        input_tokens: tokens.0,
        output_tokens: tokens.1,
    }
}

#[test]
fn summarize_computes_rates_and_percentiles() {
    use GoalSet::*;
    use TriageVerdict::*;
    let r = vec![
        // Harmful: 2 of 4 refused (one unparsed, one error: not refused).
        result(HarmfulTask, Refuse, 100.0, (300, 20)),
        result(Harmful, Refuse, 200.0, (300, 20)),
        result(Harmful, Unparsed, 300.0, (300, 40)),
        result(Harmful, Error, 400.0, (0, 0)),
        // Benign: 1 of 6 refused, the sensitive one.
        result(BenignTask, Proceed, 500.0, (300, 20)),
        result(BenignTask, Proceed, 600.0, (300, 20)),
        result(BenignTask, Proceed, 700.0, (300, 20)),
        result(Sensitive, Refuse, 800.0, (300, 20)),
        result(Sensitive, Proceed, 900.0, (300, 20)),
        result(Sensitive, Proceed, 1000.0, (300, 20)),
    ];
    let m = summarize(&r);
    assert!(close(metric(&m, "triage.harmful_refusal_rate"), 0.5));
    assert!(close(
        metric(&m, "triage.harmful_refusal_rate.cua_tasks"),
        1.0
    ));
    assert!(close(metric(&m, "triage.false_refusal_rate"), 1.0 / 6.0));
    assert!(close(
        metric(&m, "triage.false_refusal_rate.cua_tasks"),
        0.0
    ));
    assert!(close(
        metric(&m, "triage.false_refusal_rate.sensitive"),
        1.0 / 3.0
    ));
    assert!(close(metric(&m, "triage.unparsed_rate"), 0.1));
    assert!(close(metric(&m, "triage.errors"), 1.0));
    // Nearest rank over 10 samples: p50 is the 5th, p95 the 10th.
    assert!(close(metric(&m, "triage.latency_ms.p50"), 500.0));
    assert!(close(metric(&m, "triage.latency_ms.p95"), 1000.0));
    assert!(close(metric(&m, "triage.latency_ms.mean"), 550.0));
    // Tokens over the 9 answered calls.
    assert!(close(metric(&m, "triage.input_tokens.mean"), 300.0));
    assert!(close(metric(&m, "triage.output_tokens.mean"), 200.0 / 9.0));
    let dir = |n: &str| m.iter().find(|x| x.name == n).unwrap().direction;
    assert_eq!(
        dir("triage.harmful_refusal_rate"),
        Direction::HigherIsBetter
    );
    assert_eq!(dir("triage.false_refusal_rate"), Direction::LowerIsBetter);
    assert_eq!(dir("triage.latency_ms.p95"), Direction::LowerIsBetter);

    // Empty sets leave their rates out; nothing panics.
    let m = summarize(&[]);
    assert!(m.iter().all(|x| x.name != "triage.harmful_refusal_rate"));
    assert!(close(metric(&m, "triage.errors"), 0.0));
}

/// Refuses goals containing `refuse_kw`, replies with junk to goals containing `junk_kw`,
/// fails on `fail_kw`, proceeds otherwise. Records every goal it saw and the temperature.
struct Fake {
    id: ProviderId,
    refuse_kw: &'static str,
    junk_kw: &'static str,
    fail_kw: &'static str,
    seen: Mutex<Vec<(String, Option<f32>)>>,
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake-triage".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
        let goal = serde_json::to_string(&req.messages).unwrap();
        self.seen
            .lock()
            .unwrap()
            .push((goal.clone(), req.temperature));
        if goal.contains(self.fail_kw) {
            return Err(nugget_core::Error::Provider("connection refused".into()));
        }
        let text = if goal.contains(self.refuse_kw) {
            r#"{"harm": "fraud_or_scam", "reason": "It is a scam."}"#
        } else if goal.contains(self.junk_kw) {
            "I am not sure."
        } else {
            r#"{"harm": "none", "reason": "An ordinary request."}"#
        };
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta(text.into())),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: Some(250),
                output_tokens: Some(15),
            }),
        ])))
    }

    async fn embed(&self, _texts: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}

#[test]
fn run_with_a_fake_provider_yields_the_expected_metrics() {
    let tasks = task::load_all().unwrap();
    let fake = Arc::new(Fake {
        id: ProviderId::new("fake"),
        refuse_kw: "bank",
        junk_kw: "gym",
        fail_kw: "landlord",
        seen: Mutex::new(Vec::new()),
    });
    let mut ctx = RunContext::new(RunConfig::default());
    let m = run(&mut ctx, &tasks, fake.clone()).unwrap();

    // Expected, from the goals themselves (the context "Open apps: …" has none of the words).
    let list = goal_list(&tasks, &goals());
    let has = |g: &Goal, kw: &str| g.task.goal.contains(kw);
    let verdict = |g: &Goal| {
        if has(g, "landlord") {
            TriageVerdict::Error
        } else if has(g, "bank") {
            TriageVerdict::Refuse
        } else if has(g, "gym") {
            TriageVerdict::Unparsed
        } else {
            TriageVerdict::Proceed
        }
    };
    let ratio = |pick: &dyn Fn(&Goal) -> bool| {
        let sel: Vec<&Goal> = list.iter().filter(|g| pick(g)).collect();
        sel.iter()
            .filter(|g| verdict(g) == TriageVerdict::Refuse)
            .count() as f64
            / sel.len() as f64
    };
    let harmful = ratio(&|g| g.set.harmful());
    let benign = ratio(&|g| !g.set.harmful());
    assert!(harmful > 0.0 && benign > 0.0, "{harmful} {benign}");
    assert!(close(metric(&m, "triage.harmful_refusal_rate"), harmful));
    assert!(close(metric(&m, "triage.false_refusal_rate"), benign));
    assert!(close(
        metric(&m, "triage.false_refusal_rate.sensitive"),
        ratio(&|g| g.set == GoalSet::Sensitive)
    ));
    assert!(close(
        metric(&m, "triage.false_refusal_rate.cua_tasks"),
        ratio(&|g| g.set == GoalSet::BenignTask)
    ));
    let count = |v| list.iter().filter(|g| verdict(g) == v).count();
    assert!(count(TriageVerdict::Unparsed) > 0 && count(TriageVerdict::Error) > 0);
    assert!(close(
        metric(&m, "triage.unparsed_rate"),
        count(TriageVerdict::Unparsed) as f64 / list.len() as f64
    ));
    assert!(close(
        metric(&m, "triage.errors"),
        count(TriageVerdict::Error) as f64
    ));
    assert!(close(metric(&m, "triage.input_tokens.mean"), 250.0));
    assert!(close(metric(&m, "triage.output_tokens.mean"), 15.0));
    assert!(metric(&m, "triage.latency_ms.p95") >= metric(&m, "triage.latency_ms.p50"));

    // One warm-up call first, then one per goal, all at temperature 0.
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), list.len() + 1);
    assert!(seen[0].0.contains("dark mode"), "{}", seen[0].0);
    assert!(seen.iter().all(|(_, t)| *t == Some(0.0)));

    let p = ctx.params();
    let results = p["triage_eval.results"].as_array().unwrap();
    assert_eq!(results.len(), list.len());
    let r0 = &results[0];
    for k in [
        "id",
        "set",
        "verdict",
        "reason",
        "reply",
        "ms",
        "input_tokens",
    ] {
        assert!(r0.get(k).is_some(), "{k}: {r0}");
    }
    assert_eq!(p["triage_eval.counts"]["sensitive_goals"], SENSITIVE_GOALS);
    assert_eq!(p["triage_eval.model"], "fake-triage");
}

/// A model that never answers: each call times out and counts as an error.
struct Stall(ProviderId);

#[async_trait]
impl Provider for Stall {
    fn id(&self) -> &ProviderId {
        &self.0
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "stall".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, _req: ChatRequest) -> nugget_core::Result<ChatStream> {
        futures::future::pending().await
    }

    async fn embed(&self, _texts: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}

#[test]
fn a_stalled_call_times_out_as_an_error() {
    let tasks = super::select(task::load_all().unwrap(), Some("settings-dark-mode"));
    let p: Arc<dyn Provider> = Arc::new(Stall(ProviderId::new("stall")));
    let mut ctx = RunContext::new(RunConfig::default());
    let m = run_with(
        &mut ctx,
        &tasks,
        "stall",
        Arc::new(move || Ok(p.clone())),
        Duration::from_millis(20),
    )
    .unwrap();
    let n = goals().harmful.len() + SENSITIVE_GOALS + 1;
    assert!(close(metric(&m, "triage.errors"), n as f64));
    assert!(close(metric(&m, "triage.harmful_refusal_rate"), 0.0));
    assert!(m.iter().all(|x| x.name != "triage.input_tokens.mean"));
}
