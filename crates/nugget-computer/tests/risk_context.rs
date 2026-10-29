//! The policy context a computer-use call hands the gate: role, containers, form fields,
//! whether the target submits, and its dialog's text.

mod common;

use common::*;
use nugget_a11y::role::Role;
use serde_json::json;

fn shop() -> nugget_a11y::computer::UiTree {
    window(
        "w1",
        "Corner Shop",
        None,
        "Corner Shop",
        vec![
            n("ecart", Role::List, "Cart").kids(vec![
                n("eline", Role::ListItem, "Gel pen").kids(vec![n(
                    "erm",
                    Role::Button,
                    "Remove one Gel pen",
                )]),
            ]),
            n("eemail", Role::TextField, "Email"),
            n("enews", Role::CheckBox, "Send me offers"),
            n("eplace", Role::Button, "Place order"),
            n("ehelp", Role::Link, "Help"),
            n("edlg", Role::Dialog, "Leave shop?").kids(vec![
                n("etxt", Role::StaticText, "Your cart will be kept."),
                n("eok", Role::Button, "OK"),
            ]),
        ],
    )
}

#[tokio::test]
async fn context_has_role_ancestors_fields_and_submits() {
    let kit = Kit::new(vec![shop()]);
    kit.look().await;
    let ctx = |id: &str| kit.tool("click").context(&json!({ "element": id }));

    let c = ctx("erm");
    assert_eq!(c.role.as_deref(), Some("button"));
    let anc: Vec<(String, Option<String>)> = c
        .ancestors
        .iter()
        .map(|a| (a.role.clone(), a.name.clone()))
        .collect();
    assert_eq!(
        anc,
        vec![
            ("list_item".into(), Some("Gel pen".into())),
            ("list".into(), Some("Cart".into())),
            ("window".into(), Some("Corner Shop".into())),
        ]
    );
    assert_eq!(c.form_fields, vec!["Email".to_string()]);
    assert_eq!(c.window_title.as_deref(), Some("Corner Shop"));
    assert_eq!(c.submits, None);
    assert_eq!(c.dialog_text, None);
    assert!(!c.secure_field);

    assert_eq!(ctx("eplace").submits, Some(true));
    assert_eq!(ctx("enews").submits, Some(false));
    assert_eq!(ctx("eline").submits, Some(false));
    assert_eq!(ctx("ehelp").submits, Some(false));
    assert_eq!(ctx("eemail").submits, Some(false));

    let ok = ctx("eok");
    assert_eq!(ok.submits, Some(true));
    assert_eq!(
        ok.dialog_text.as_deref(),
        Some("Leave shop?\nYour cart will be kept.")
    );
    assert_eq!(ok.window_title.as_deref(), Some("Corner Shop"));

    // Not in the last snapshot: an empty context.
    let none = ctx("enope");
    assert_eq!(none.role, None);
    assert!(none.ancestors.is_empty());
}
