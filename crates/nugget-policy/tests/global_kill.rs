//! The process-wide kill switch, in its own test binary so no other test shares the global.

use nugget_policy::{ActionDescriptor, Decision, KillSwitch, PolicyEngine, PolicyGate, RiskClass};

#[test]
fn global_switch_reaches_every_engine() {
    let a = PolicyEngine::default();
    let b = PolicyEngine::default();
    let action = ActionDescriptor {
        target_app: "com.apple.notes".into(),
        verb: "scroll".into(),
        target_label: None,
        text_preview: None,
        declared_risk: RiskClass::Observe,
        task_id: "t".into(),
    };
    assert_eq!(a.evaluate(&action), Decision::Allow);

    // What the hotkey crate's kill trigger calls.
    let hook = KillSwitch::global().trigger_hook();
    hook();
    assert!(PolicyGate::halted(&a));
    assert!(PolicyGate::halted(&b));
    assert!(matches!(b.evaluate(&action), Decision::Deny(_)));

    KillSwitch::global().resume();
    assert_eq!(a.evaluate(&action), Decision::Allow);
}
