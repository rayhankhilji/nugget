use nugget_policy::*;

fn act(app: &str, verb: &str, label: Option<&str>, preview: Option<&str>, risk: RiskClass) -> ActionDescriptor {
    ActionDescriptor {
        target_app: app.into(),
        verb: verb.into(),
        target_label: label.map(Into::into),
        text_preview: preview.map(Into::into),
        declared_risk: risk,
        task_id: "t1".into(),
    }
}

fn engine() -> PolicyEngine {
    PolicyEngine::builder().kill_switch(KillSwitch::new()).build()
}

#[test]
fn pa_task_approval_leaks_to_other_site_via_label_url() {
    let e = engine();
    e.approve_for_task("t1", TaskApproval { risk: RiskClass::Send, target: Some("mail.google.com".into()) }).unwrap();
    // Sanity: on evil.com a Send click needs confirmation.
    let plain = act("evil.example", "click", Some("button \"Post comment\""), None, RiskClass::Send);
    assert!(matches!(e.evaluate(&plain), Decision::Confirm(_)));
    // The page names its button with the approved host.
    let spoof = act("evil.example", "click", Some("button \"Post https://mail.google.com/x\""), None, RiskClass::Send);
    assert!(matches!(e.evaluate(&spoof), Decision::Confirm(_)), "approval for mail.google.com reused on evil.example: {:?}", e.evaluate(&spoof));
}

#[test]
fn pa_user_allow_rule_leaks_via_preview_url() {
    let e = engine();
    e.upsert_rule(Rule::new("u:work", Effect::Allow).targets(["*.corp.example"]).risks([RiskClass::Send])).unwrap();
    let a = act("com.tinyspeck.slackmacgap", "send_message", Some("Message #general"), Some("see https://wiki.corp.example/page"), RiskClass::Send);
    assert!(matches!(e.evaluate(&a), Decision::Confirm(_)), "{:?}", e.evaluate(&a));
}

#[test]
fn pa_terminal_mid_newline_runs_command_at_edit() {
    let c = RiskClassifier::new();
    let a = act("com.apple.Terminal", "type", Some("shell"), Some("echo ok\nopen -a Calculator "), RiskClass::Edit);
    assert!(c.classify(&a).risk >= RiskClass::Security, "{:?}", c.classify(&a));
}

#[test]
fn pa_chat_mid_newline_sends_at_edit() {
    let c = RiskClassifier::new();
    let a = act("com.tinyspeck.slackmacgap", "type", Some("Message #general"), Some("secret stuff\n "), RiskClass::Edit);
    assert!(c.classify(&a).risk >= RiskClass::Send, "{:?}", c.classify(&a));
}

#[test]
fn pa_terminal_ctrl_j_runs_command_at_edit() {
    let c = RiskClassifier::new();
    for k in ["ctrl+j", "ctrl+m", "ctrl+o"] {
        let a = act("com.apple.Terminal", "key", Some(k), None, RiskClass::Edit);
        assert!(c.classify(&a).risk >= RiskClass::Security, "{k}: {:?}", c.classify(&a));
    }
}
