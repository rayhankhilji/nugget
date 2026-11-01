//! Regression tests for QA GG3's findings on the loop's judgement: acting on a guess (GG3-1),
//! irreversible steps nobody asked for (GG3-2), success after a declined send (GG3-4), a control
//! that changes before it is pressed (GG3-7), false "done" (GG3-8), no-progress loops (GG3-14),
//! and a triage refusal put to the user.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::*;
use nugget_agent::{
    ActionTarget, AgentConfig, GateOutcome, Outcome, TRIAGE_GO_AHEAD, Task, Tool, ToolResult,
    Verification,
};
use nugget_policy::{Decision, RiskClass};
use serde_json::json;

/// A button with a label and a risk class; counts presses. With `ok: false` in the arguments the
/// press has no visible effect (verification fails).
fn button(
    name: &'static str,
    label: &'static str,
    risk: RiskClass,
) -> (Arc<dyn Tool>, Arc<AtomicU32>) {
    let pressed = Arc::new(AtomicU32::new(0));
    let (p1, p2) = (pressed.clone(), pressed.clone());
    let tool = Fake::new(name, risk, move |a| {
        if a["ok"] != json!(false) {
            p1.fetch_add(1, Ordering::SeqCst);
        }
        ToolResult::ok("pressed")
    })
    .observe(move || json!(p2.load(Ordering::SeqCst)))
    .verify(|_, before, after, _| match before == after {
        true => Verification::Failed("no visible effect".into()),
        false => Verification::Passed("pressed".into()),
    })
    .describe(move |_| ActionTarget {
        target_app: "app".into(),
        verb: "click".into(),
        target_label: Some(label.into()),
        text_preview: None,
    });
    (tool.arc(), pressed)
}

fn presses(p: &AtomicU32) -> u32 {
    p.load(Ordering::SeqCst)
}

/// GG3-1: "Delete the annual report" with only folders on screen. The first delete is sent back
/// (find it first), the second asks the user; nothing is deleted. Once it is on screen, it goes.
#[tokio::test]
async fn a_destructive_action_on_an_object_never_seen_is_sent_back_then_asked() {
    let goal = "Delete the annual report in Documents";
    let (trash, pressed) = button("trash", "Move to Trash", RiskClass::Destroy);
    let look = screen("look", &["Folder Documents", "Folder Downloads"]);
    let turns = vec![
        one("c1", "look", json!({})),
        one("c2", "trash", json!({})),
        one("c3", "trash", json!({})),
    ];
    let (run, model) = exec(registry(&World::new()).with(look).with(trash), turns, goal).await;
    assert_eq!(presses(&pressed), 0);
    assert!(
        model
            .result_text(2)
            .contains("has not appeared on any screen")
    );
    let Outcome::NeedsInput(q) = &run.outcome else {
        panic!("{:?}", run.outcome);
    };
    assert!(
        q.contains("annual report") && q.contains("have not found it"),
        "{q}"
    );

    let (trash, pressed) = button("trash", "Move to Trash", RiskClass::Destroy);
    let look = screen("look", &["File annual-report.pdf", "File notes.txt"]);
    let turns = vec![
        one("c1", "look", json!({})),
        one("c2", "trash", json!({})),
        fin("done"),
    ];
    let (run, _) = exec(registry(&World::new()).with(look).with(trash), turns, goal).await;
    assert_eq!(presses(&pressed), 1);
    assert_eq!(run.outcome, Outcome::Succeeded);
}

/// GG3-2: after deleting a file, Empty Trash is refused without asking; asked for, it goes on.
#[tokio::test]
async fn an_irreversible_step_the_goal_did_not_ask_for_is_refused() {
    let (empty, pressed) = button("empty", "Empty Trash", RiskClass::Destroy);
    let model = Scripted::new(vec![one("c1", "empty", json!({})), fin("done")]);
    let gate = FakeGate::new();
    let run = agent(&model, registry(&World::new()).with(empty), &gate)
        .run(
            Task::new("t-empty", "Delete backup.zip from Downloads"),
            cfg(),
        )
        .await;
    assert_eq!(presses(&pressed), 0);
    let to_gate = gate
        .seen()
        .iter()
        .any(|d| d.target_label.as_deref() == Some("Empty Trash"));
    assert!(!to_gate, "never put to the gate or the user");
    assert!(
        matches!(&run.actions[0].decision, GateOutcome::Denied { reason } if reason.contains("irreversible"))
    );

    let (empty, pressed) = button("empty", "Empty Trash", RiskClass::Destroy);
    let turns = vec![one("c1", "empty", json!({})), fin("done")];
    exec(
        registry(&World::new()).with(empty),
        turns,
        "Empty the trash",
    )
    .await;
    assert_eq!(presses(&pressed), 1);
}

/// GG3-4: a declined or unanswered send followed by `finish(done)` is not success.
#[tokio::test]
async fn done_after_a_declined_or_unanswered_send_is_not_success() {
    for answer in [Some(false), None] {
        let world = World::new();
        let model = Scripted::new(vec![send("c1", "bob@x.y", "hi"), fin("done")]);
        let gate = FakeGate::new().rule("send_email", Decision::Confirm("sends".into()));
        let (a, _events) = events(agent(&model, registry(&world), &gate), answer);
        let c = AgentConfig {
            confirm_timeout: Duration::from_millis(50),
            ..cfg()
        };
        let run = a.run(Task::new("t-decl", "Email Bob hi"), c).await;
        assert_eq!(world.sent(), 0);
        let not_done = matches!(&run.outcome, Outcome::Failed(d) if d.starts_with("not done"));
        assert!(not_done, "{answer:?}: {:?}", run.outcome);
    }
}

/// GG3-4: a failed press is cleared only by a later pass on the same control, not by any pass
/// of the same tool.
#[tokio::test]
async fn a_failure_is_cleared_only_by_the_same_control() {
    let world = World::new();
    let (send, _) = button("press", "Send", RiskClass::Edit);
    let turns = vec![
        one("c1", "press", json!({"ok": false})),
        one("c2", "set_value", json!({"value": 2})),
        fin("done"),
    ];
    let (run, _) = exec(registry(&world).with(send), turns, "press it").await;
    assert!(
        matches!(&run.outcome, Outcome::Failed(d) if d.contains("verification failed")),
        "{:?}",
        run.outcome
    );
}

/// GG3-7: the control is looked at again right before acting; a changed label is not pressed.
#[tokio::test]
async fn a_control_that_changed_before_the_press_is_not_pressed() {
    let looks = Arc::new(AtomicU32::new(0));
    let pressed = Arc::new(AtomicU32::new(0));
    let (l1, l2, p) = (looks.clone(), looks.clone(), pressed.clone());
    let click = Fake::new("click", RiskClass::Edit, move |_| {
        p.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("clicked")
    })
    .observe(move || json!(l1.fetch_add(1, Ordering::SeqCst)))
    .describe(move |_| ActionTarget {
        target_app: "calendar".into(),
        verb: "click".into(),
        target_label: Some(match l2.load(Ordering::SeqCst) < 2 {
            true => "Save event".into(),
            false => "Save and send invitations".into(),
        }),
        text_preview: None,
    });
    let turns = vec![one("c1", "click", json!({})), fin("cannot")];
    let (run, _) = exec(
        registry(&World::new()).with(click.arc()),
        turns,
        "save the event",
    )
    .await;
    assert_eq!(presses(&pressed), 0);
    assert!(matches!(
        &run.actions[0].decision,
        GateOutcome::Denied { reason } if reason.contains("changed before the action ran")
    ));
}

/// GG3-8: "done" for a request to send, when nothing was sent, is not success.
#[tokio::test]
async fn done_without_the_send_the_goal_asks_for_is_not_success() {
    let turns = vec![one("c1", "set_value", json!({"value": 1})), fin("done")];
    let (run, _) = exec(registry(&World::new()), turns, "Send Bob the Q3 numbers").await;
    assert!(
        matches!(&run.outcome, Outcome::Failed(d) if d.contains("no such action")),
        "{:?}",
        run.outcome
    );
}

/// GG3-14: the same failing call three times ends the run.
#[tokio::test]
async fn the_same_failing_call_three_times_ends_the_run() {
    let turns = (0..10)
        .map(|i| one(format!("c{i}").leak(), "nope", json!({})))
        .collect();
    let (run, _) = exec(registry(&World::new()), turns, "do it").await;
    assert!(
        matches!(&run.outcome, Outcome::Failed(d) if d.starts_with("no progress")),
        "{:?}",
        run.outcome
    );
    assert_eq!(run.steps, 3);
}

/// GG3-14: a declined send is not put to the user again; the second re-proposal ends the run.
#[tokio::test]
async fn a_declined_action_is_not_asked_about_again() {
    let world = World::new();
    let turns = ["c1", "c2", "c3", "c4"]
        .map(|id| send(id, "b@x.y", "hi"))
        .into();
    let model = Scripted::new(turns);
    let gate = FakeGate::new().rule("send_email", Decision::Confirm("sends".into()));
    let (a, events) = events(agent(&model, registry(&world), &gate), Some(false));
    let run = a.run(Task::new("t-redecl", "Email b hi"), cfg()).await;
    drop(a);
    assert_eq!(asked(&events.await.unwrap(), None).len(), 1, "asked once");
    let stopped =
        matches!(&run.outcome, Outcome::Failed(d) if d.contains("stopped instead of asking again"));
    assert!(stopped, "{:?}", run.outcome);
    assert_eq!(world.sent(), 0);
}

/// A triage refusal becomes a question when `triage_override` is on; "Do it anyway" goes ahead
/// and is logged as the user's decision, anything else keeps the refusal.
#[tokio::test]
async fn a_triage_refusal_can_be_overridden_by_the_user() {
    let refusal = || text_turn(r#"{"harm": "harassment", "reason": "it threatens someone."}"#);
    let c = AgentConfig {
        triage: true,
        triage_override: true,
        ..cfg()
    };
    let world = World::new();
    let model = Scripted::new(vec![
        refusal(),
        one("c1", "set_value", json!({"value": 1})),
        fin("done"),
    ]);
    let log = Arc::new(nugget_agent::MemoryLog::new());
    let a = agent(&model, registry(&world), &FakeGate::new()).with_log(log.clone());
    let run = a
        .run(Task::new("t-ovr", "set the counter"), c.clone())
        .await;
    let Outcome::NeedsInput(q) = &run.outcome else {
        panic!("{:?}", run.outcome);
    };
    assert!(q.contains("it threatens someone."), "{q}");
    let st = run.suspended.unwrap();
    assert_eq!(st.pending.as_ref().unwrap().options[0], TRIAGE_GO_AHEAD);
    assert_eq!(world.value(), 0);
    let run = a.resume(st, "Do it anyway", c.clone()).await;
    assert_eq!((run.outcome, world.value()), (Outcome::Succeeded, 1));
    let report = nugget_agent::explain(&log.entries());
    assert!(
        report.contains("the user chose to go ahead anyway"),
        "{report}"
    );

    let world = World::new();
    let model = Scripted::new(vec![refusal()]);
    let a = agent(&model, registry(&world), &FakeGate::new());
    let run = a
        .run(Task::new("t-ovr2", "set the counter"), c.clone())
        .await;
    let run = a.resume(run.suspended.unwrap(), "Cancel", c).await;
    assert_eq!(
        run.outcome,
        Outcome::Refused("it threatens someone.".into())
    );
    assert_eq!(world.value(), 0);
}
