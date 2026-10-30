mod common;

use std::sync::{Arc, Mutex};

use common::{FakeBridge, FakeGate, ScriptedConfirmer, form, id};
use nugget_browser::bridge::BridgeError;
use nugget_browser::protocol::{ErrorCode, SnapshotOptions};
use nugget_browser::tool::{ToolContext, browser_tools};
use nugget_browser::{BrowserAction, BrowserController, BrowserError, DeclineAll};
use nugget_policy::RiskClass;
use serde_json::json;

fn setup(
    gate: FakeGate,
    confirm: Option<bool>,
) -> (Arc<FakeGate>, Arc<FakeBridge>, Arc<BrowserController>) {
    let gate = Arc::new(gate);
    let bridge = Arc::new(FakeBridge::new(form()));
    let confirmer: Arc<dyn nugget_browser::Confirmer> = match confirm {
        Some(v) => Arc::new(ScriptedConfirmer(v, Mutex::new(Vec::new()))),
        None => Arc::new(DeclineAll),
    };
    let c = Arc::new(BrowserController::new(
        bridge.clone(),
        gate.clone(),
        confirmer,
    ));
    (gate, bridge, c)
}

#[tokio::test]
async fn gate_is_asked_before_every_snapshot_and_action() {
    let (gate, bridge, c) = setup(FakeGate::new(), None);
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    let email = id(&s, "Email address");
    let out = c
        .act(
            "t",
            BrowserAction::Type {
                element: email,
                text: "grace@example.com".into(),
                clear: None,
            },
        )
        .await
        .unwrap();
    assert!(out.verification.is_confirmed());
    assert_eq!(out.descriptor.target_app, "shop.example");
    c.act(
        "t",
        BrowserAction::Scroll {
            element: None,
            dx: None,
            dy: Some(100.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(gate.verbs(), ["snapshot", "set_value", "scroll"]);
    // Every request that reached the browser was preceded by a gate decision for it.
    let kinds = bridge.kinds();
    assert_eq!(kinds, ["snapshot", "overlay_on", "act", "act"]);
    assert_eq!(
        kinds.iter().filter(|k| **k == "act").count(),
        gate.verbs().iter().filter(|v| *v != "snapshot").count()
    );
    c.finish().await;
    assert_eq!(bridge.kinds().last(), Some(&"overlay_off"));
}

#[tokio::test]
async fn denied_host_never_reaches_the_browser() {
    let (gate, bridge, c) = setup(FakeGate::new(), None);
    c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    let err = c
        .act(
            "t",
            BrowserAction::Navigate {
                url: "https://bank.example/".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Denied(_)), "{err:?}");
    assert!(!bridge.kinds().contains(&"act"));
    assert_eq!(
        gate.seen.lock().unwrap().last().unwrap().target_app,
        "bank.example"
    );
}

#[tokio::test]
async fn snapshots_of_denied_sites_are_withheld() {
    let (_, bridge, c) = setup(FakeGate::new(), None);
    let mut s = form();
    s.url = "https://bank.example/accounts".into();
    *bridge.snapshot.lock().unwrap() = s;
    assert!(matches!(
        c.snapshot("t", SnapshotOptions::default()).await,
        Err(BrowserError::Denied(_))
    ));
    assert!(c.latest_snapshot().is_none());
}

#[tokio::test]
async fn confirmation_is_required_and_fails_closed() {
    let mut gate = FakeGate::new();
    gate.confirm_verbs = vec!["click".into()];
    let (_, bridge, c) = setup(gate, None);
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    let err = c
        .act(
            "t",
            BrowserAction::Click {
                element: id(&s, "Create account"),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Declined(_)));
    assert!(!bridge.kinds().contains(&"act"));

    let mut gate = FakeGate::new();
    gate.confirm_verbs = vec!["click".into()];
    let (_, bridge, c) = setup(gate, Some(true));
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    c.act(
        "t",
        BrowserAction::Click {
            element: id(&s, "Create account"),
        },
    )
    .await
    .unwrap();
    assert!(bridge.kinds().contains(&"act"));
}

#[tokio::test]
async fn kill_switch_stops_everything_and_tells_the_page() {
    let (gate, bridge, c) = setup(FakeGate::new(), None);
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    gate.halted.store(true, std::sync::atomic::Ordering::SeqCst);
    let err = c
        .act(
            "t",
            BrowserAction::Click {
                element: id(&s, "Log in"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err, BrowserError::Halted);
    assert!(matches!(
        c.snapshot("t", SnapshotOptions::default()).await,
        Err(BrowserError::Halted)
    ));
    assert_eq!(bridge.kinds(), ["snapshot", "halt", "halt"]);
}

#[tokio::test]
async fn invalid_actions_are_refused_before_the_gate() {
    let (gate, bridge, c) = setup(FakeGate::new(), None);
    // No snapshot yet.
    assert!(matches!(
        c.act(
            "t",
            BrowserAction::Click {
                element: "n1".into()
            }
        )
        .await,
        Err(BrowserError::Invalid(_))
    ));
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    let err = c
        .act(
            "t",
            BrowserAction::Type {
                element: id(&s, "Password"),
                text: "x".into(),
                clear: None,
            },
        )
        .await;
    assert!(matches!(err, Err(BrowserError::Invalid(_))));
    assert_eq!(gate.verbs(), ["snapshot"]);
    assert_eq!(bridge.kinds(), ["snapshot"]);
}

#[tokio::test]
async fn extension_errors_map_to_controller_errors() {
    let (_, bridge, c) = setup(FakeGate::new(), None);
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    *bridge.act_reply.lock().unwrap() = Some(Err(BridgeError::Extension {
        code: ErrorCode::StaleSnapshot,
        message: "m".into(),
    }));
    let err = c
        .act(
            "t",
            BrowserAction::Click {
                element: id(&s, "Log in"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err, BrowserError::StaleSnapshot);
    // The stale snapshot is dropped: the agent must look again.
    assert!(c.latest_snapshot().is_none());
}

#[tokio::test]
async fn tools_drive_the_controller() {
    let (gate, _, c) = setup(FakeGate::new(), None);
    let tools = browser_tools(c);
    let names: Vec<_> = tools.iter().map(|t| t.spec().name).collect();
    assert_eq!(
        names,
        [
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_select",
            "browser_scroll",
            "browser_navigate",
            "browser_submit",
            "browser_wait_for"
        ]
    );
    let ctx = ToolContext {
        task_id: "task-9".into(),
    };
    let snap = tools[0].call(&ctx, json!({})).await.unwrap();
    let email = snap["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Email address")
        .unwrap()["id"]
        .clone();
    let out = tools[2]
        .call(&ctx, json!({"element": email, "text": "grace@example.com"}))
        .await
        .unwrap();
    assert_eq!(out["verification"]["status"], "confirmed");
    let err = tools[5]
        .call(&ctx, json!({"url": "https://bank.example/"}))
        .await
        .unwrap_err();
    assert!(err.is_mutation_refused());
    assert!(tools[1].call(&ctx, json!({})).await.is_err());
    assert!(
        gate.seen
            .lock()
            .unwrap()
            .iter()
            .all(|d| d.task_id == "task-9")
    );
}

/// Records the context of every check; refuses any check made without one.
#[derive(Default)]
struct ContextGate {
    evaluated: Mutex<Vec<(String, nugget_policy::ActionContext)>>,
    approved: Mutex<Vec<(String, nugget_policy::ActionContext)>>,
}

impl nugget_policy::PolicyGate for ContextGate {
    fn evaluate(&self, _: &nugget_policy::ActionDescriptor) -> nugget_policy::Decision {
        nugget_policy::Decision::Deny("checked without context".into())
    }

    fn halted(&self) -> bool {
        false
    }

    fn evaluate_with_context(
        &self,
        a: &nugget_policy::ActionDescriptor,
        context: &nugget_policy::ActionContext,
    ) -> nugget_policy::Decision {
        self.evaluated
            .lock()
            .unwrap()
            .push((a.verb.clone(), context.clone()));
        if a.verb == "click" {
            nugget_policy::Decision::Confirm("click needs approval".into())
        } else {
            nugget_policy::Decision::Allow
        }
    }

    fn record_approved(&self, _: &nugget_policy::ActionDescriptor, _: RiskClass) -> bool {
        false // only the context-aware recording may approve
    }

    fn record_approved_with_context(
        &self,
        a: &nugget_policy::ActionDescriptor,
        _: RiskClass,
        context: &nugget_policy::ActionContext,
    ) -> bool {
        self.approved
            .lock()
            .unwrap()
            .push((a.verb.clone(), context.clone()));
        true
    }
}

fn context_rig() -> (Arc<ContextGate>, Arc<BrowserController>) {
    let gate = Arc::new(ContextGate::default());
    let c = Arc::new(BrowserController::new(
        Arc::new(FakeBridge::new(form())),
        gate.clone(),
        Arc::new(ScriptedConfirmer(true, Mutex::new(Vec::new()))),
    ));
    (gate, c)
}

#[tokio::test]
async fn controller_rechecks_with_the_page_context() {
    let (gate, c) = context_rig();
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    c.act(
        "t",
        BrowserAction::Type {
            element: id(&s, "Email address"),
            text: "grace@example.com".into(),
            clear: None,
        },
    )
    .await
    .unwrap();
    // The fake bridge's act reply replaces the snapshot; take the form again.
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    c.act(
        "t",
        BrowserAction::Click {
            element: id(&s, "Create account"),
        },
    )
    .await
    .unwrap();
    c.act(
        "t",
        BrowserAction::Navigate {
            url: "/checkout".into(),
        },
    )
    .await
    .unwrap();

    let evaluated = gate.evaluated.lock().unwrap().clone();
    let verbs: Vec<_> = evaluated.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(
        verbs,
        ["snapshot", "set_value", "snapshot", "click", "navigate"]
    );

    // A read is judged on the site alone.
    assert_eq!(evaluated[0].1, nugget_policy::ActionContext::default());

    let typed = &evaluated[1].1;
    assert_eq!(typed.url.as_deref(), Some("https://shop.example/page"));
    assert!(!typed.secure_field);
    assert_eq!(typed.role.as_deref(), Some("textbox"));
    // The form's password field is named in the classifier's vocabulary.
    assert!(typed.form_fields.iter().any(|f| f == "password"));
    assert!(typed.form_fields.iter().any(|f| f == "Email address"));

    let click = &evaluated[3].1;
    assert_eq!(click.role.as_deref(), Some("button"));
    assert_eq!(click.submits, Some(true));
    assert!(!click.secure_field);
    // The approval is counted in the context it was judged in.
    assert_eq!(
        *gate.approved.lock().unwrap(),
        [("click".to_owned(), click.clone())]
    );

    // A navigation is judged on its destination, not the page being left.
    let nav = &evaluated[4].1;
    assert_eq!(nav.url.as_deref(), Some("https://shop.example/checkout"));
    assert_eq!(nav.window_title, None);
}

#[tokio::test]
async fn controller_context_matches_what_the_agent_tools_give_the_loop() {
    let (gate, c) = context_rig();
    let tools = nugget_browser::tool::agent_tools(c.clone());
    let s = c.snapshot("t", SnapshotOptions::default()).await.unwrap();
    let tool = tools.iter().find(|t| t.name() == "browser_click").unwrap();
    let args = json!({"element": id(&s, "Create account")});
    let loop_context = tool.context(&args);
    c.act(
        "t",
        BrowserAction::Click {
            element: id(&s, "Create account"),
        },
    )
    .await
    .unwrap();
    let evaluated = gate.evaluated.lock().unwrap();
    assert_eq!(
        evaluated.last().unwrap(),
        &("click".to_owned(), loop_context)
    );
}
