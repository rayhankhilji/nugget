use nugget_policy::{PolicyGate, ActionContext, ActionDescriptor, KillSwitch, PolicyEngine, RiskClass};
fn main() {
    let eng = PolicyEngine::builder().kill_switch(KillSwitch::new()).build();
    let cases: &[(&str, &str, &str, Option<&str>, &str)] = &[
        // app, verb, label, preview, role
        ("settings", "click", "Automatic updates", None, "switch"),
        ("com.apple.systempreferences", "click", "Automatic updates", None, "switch"),
        ("com.apple.systempreferences", "click", "Lock screen after 5 minutes", None, "switch"),
        ("gnome-control-center", "click", "Lock screen after 5 minutes", None, "switch"),
        ("SystemSettings", "click", "Automatic updates", None, "switch"),
        ("com.apple.systempreferences", "press_key", "Firewall", Some("space"), "switch"),
        ("com.apple.systempreferences", "press_key", "Lock screen after 5 minutes", Some("space"), "switch"),
        ("settings", "click", "Firewall", None, "switch"),
        ("settings", "press_key", "Firewall", Some("space"), "switch"),
        ("mail", "click", "Löschen", None, "button"),
        ("mail", "press_key", "Löschen", Some("space"), "button"),
        ("mail", "press_key", "Löschen", Some("enter"), "button"),
        ("mail", "click", "Senden", None, "button"),
        ("mail", "press_key", "Senden", Some("space"), "button"),
        ("shop", "press_key", "Bezahlen", Some("space"), "button"),
        ("mail", "press_key", "Delete", Some("space"), "button"),
    ];
    for (app, verb, label, prev, role) in cases {
        let a = ActionDescriptor {
            target_app: app.to_string(), verb: verb.to_string(),
            target_label: Some(label.to_string()), text_preview: prev.map(String::from),
            declared_risk: RiskClass::Edit, task_id: "v".into(),
        };
        let mut ctx = ActionContext::default(); ctx.role = Some(role.to_string());
        let d = eng.evaluate_with_context(&a, &ctx);
        let c = nugget_policy::RiskClassifier::global().classify_with_context(&a, &ctx);
        println!("{app:30} {verb:10} {label:30} {prev:?} -> risk={:?} decision={:?}", c.risk, d);
    }
}
