//! End to end through the real agent loop and the real policy engine: an Edit runs without
//! asking, a click on "Send" needs the user's confirmation, and done means verified.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use nugget_agent::{
    Agent, AgentConfig, AgentEvent, GateOutcome, Model, Outcome, Task, ToolRegistry,
};
use nugget_computer::ComputerConfig;
use nugget_policy::{PolicyConfig, PolicyEngine, RiskClass};
use serde_json::json;
use tokio::sync::mpsc;

fn script() -> Arc<Scripted> {
    Scripted::new(vec![
        turn(vec![("c1", "observe", json!({}))]),
        turn(vec![(
            "c2",
            "type_text",
            json!({"element": "eto", "text": "bob@example.org"}),
        )]),
        turn(vec![("c3", "click", json!({"element": "esend"}))]),
        turn(vec![(
            "c4",
            "finish",
            json!({"summary": "sent", "outcome": "success"}),
        )]),
    ])
}

async fn run(approve: bool) -> (nugget_agent::AgentRun, Vec<AgentEvent>, Arc<FakeComputer>) {
    let cu = FakeComputer::new(vec![mail()]);
    cu.on("esend", |w| w.close("w1"));
    let mut registry = ToolRegistry::new();
    for t in nugget_computer::tools_with(cu.clone(), ComputerConfig::default()) {
        registry.register(t);
    }
    let gate = Arc::new(PolicyEngine::new(PolicyConfig::default()));
    let agent = Agent::new(Model::Provider(script()), registry, gate);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let agent = agent.with_events(Arc::new(tx));
    let confirmations = agent.confirmations();
    let events = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::ConfirmationNeeded { request, .. } = &ev {
                assert!(confirmations.answer(&request.request_id, approve));
            }
            seen.push(ev);
        }
        seen
    });
    let cfg = AgentConfig {
        confirm_timeout: Duration::from_secs(5),
        ..AgentConfig::default()
    };
    let r = agent.run(Task::new("t1", "email bob"), cfg).await;
    drop(agent);
    (r, events.await.unwrap(), cu)
}

fn confirmations(events: &[AgentEvent]) -> Vec<nugget_agent::ConfirmationRequest> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ConfirmationNeeded { request, .. } => Some(request.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn risky_click_needs_confirmation_and_edit_does_not() {
    let (r, events, cu) = run(true).await;
    let asked = confirmations(&events);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].action.verb, "click");
    assert_eq!(asked[0].action.target_label.as_deref(), Some("Send"));
    assert_eq!(asked[0].action.target_app, "com.apple.mail");

    let by_tool = |t: &str| r.actions.iter().find(|a| a.tool == t).unwrap();
    assert_eq!(by_tool("observe").risk, RiskClass::Observe);
    assert_eq!(by_tool("type_text").risk, RiskClass::Edit);
    assert_eq!(by_tool("type_text").decision, GateOutcome::Allowed);
    assert_eq!(by_tool("click").risk, RiskClass::Send);
    assert!(matches!(
        by_tool("click").decision,
        GateOutcome::Approved { .. }
    ));

    // Both mutations verified, so the claimed success stands.
    assert_eq!(r.verification.passed, 2, "{:?}", r.actions);
    assert_eq!(r.outcome, Outcome::Succeeded);
    assert!(cu.acts().contains(&"press:esend".to_string()));
}

#[tokio::test]
async fn declined_click_is_not_performed() {
    let (r, events, cu) = run(false).await;
    assert_eq!(confirmations(&events).len(), 1);
    assert!(!cu.acts().contains(&"press:esend".to_string()));
    assert!(cu.with(|w| w.windows.len()) == 1);
    let click = r.actions.iter().find(|a| a.tool == "click").unwrap();
    assert!(!click.executed);
    assert!(matches!(click.decision, GateOutcome::Rejected { .. }));
    // Only the typing ran (and verified); the click left nothing to verify.
    assert_eq!(r.verification.passed, 1);
}
