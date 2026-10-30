//! Results that show a screen carry its targets (the items a task could refer to), and the text
//! the model reads does not change because of them.

mod common;

use common::*;
use nugget_a11y::role::Role;
use nugget_a11y::{ElementId, UiTree};
use nugget_computer::{RenderOptions, render};
use serde_json::json;

fn row(id: &str, from: &str, subject: &str) -> nugget_a11y::UiNode {
    n(&format!("erow{id}"), Role::ListItem, subject).kids(vec![n(
        &format!("eopen{id}"),
        Role::Button,
        &format!("{from}: {subject}"),
    )])
}

/// An inbox with mail from two people named Sam.
fn inbox() -> UiTree {
    let mut t = window(
        "w1",
        "Mail",
        Some("com.apple.mail"),
        "Inbox",
        vec![
            n("ecompose", Role::Button, "Compose"),
            n("esearch", Role::SearchField, "Search"),
            n("emsgs", Role::List, "Messages").kids(vec![
                row("1", "Sam Lee", "Can I take Friday off?"),
                row("2", "Sam Ortiz", "Invoice for October"),
                row("3", "Dana Kim", "Lunch on Thursday?"),
            ]),
        ],
    );
    t.focused = Some(ElementId("esearch".into()));
    t
}

#[tokio::test]
async fn observe_lists_the_inbox_rows_as_targets() {
    let kit = Kit::new(vec![inbox()]);
    let r = kit.look().await;
    assert!(!r.is_error && r.untrusted, "{}", r.content);
    assert_eq!(
        r.targets,
        [
            "Sam Lee: Can I take Friday off?",
            "Sam Ortiz: Invoice for October",
            "Dana Kim: Lunch on Thursday?",
        ]
    );
    // The model reads the same screen it would without targets.
    assert!(
        r.content
            .contains("[eopen1] button \"Sam Lee: Can I take Friday off?\"")
    );
    assert!(!r.content.contains("targets"));
    // Targets are not serialized into the data either.
    assert!(r.data.as_ref().is_some_and(|d| d.get("targets").is_none()));
}

#[tokio::test]
async fn observe_skips_commands_fields_and_options() {
    let kit = Kit::new(vec![mail()]);
    let r = kit.look().await;
    // The compose window's message list has two rows; its toolbar buttons, fields, combo box
    // options and dialog are not targets.
    assert_eq!(r.targets, ["Hello", "World"]);
    let kit = Kit::new(vec![terminal()]);
    assert!(kit.look().await.targets.is_empty());
}

#[tokio::test]
async fn an_action_that_changes_the_screen_carries_the_new_targets() {
    let kit = Kit::new(vec![inbox()]);
    // Opening a message replaces the list with the message view.
    kit.cu.on("eopen1", |w| {
        let t = &mut w.windows[0];
        t.root.children = vec![
            n("eback", Role::Button, "Back to inbox"),
            n("esubj", Role::Heading, "Can I take Friday off?"),
            n("ereply", Role::Button, "Reply"),
        ];
    });
    // Archiving one row leaves the other two.
    kit.cu.on("ecompose", |w| w.remove("erow3"));

    kit.look().await;
    let (r, _) = kit.act("click", json!({"element": "ecompose"})).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("the window changed"), "{}", r.content);
    assert_eq!(
        r.targets,
        [
            "Sam Lee: Can I take Friday off?",
            "Sam Ortiz: Invoice for October",
        ]
    );

    let (r, _) = kit.act("click", json!({"element": "eopen1"})).await;
    assert!(r.content.contains("the window changed"), "{}", r.content);
    assert!(r.targets.is_empty(), "{:?}", r.targets);

    // An action that changes nothing beyond its element shows no screen and no targets.
    let kit = Kit::new(vec![inbox()]);
    kit.look().await;
    let (r, _) = kit
        .act("type_text", json!({"element": "esearch", "text": "Sam"}))
        .await;
    assert!(!r.content.contains("the window changed"), "{}", r.content);
    assert!(r.targets.is_empty());
}

#[test]
fn rendering_is_unchanged_by_targets() {
    // The renderer is the only source of the model's text; it knows nothing of targets.
    let t = inbox();
    let opts = RenderOptions::default();
    let text = render(&t, &opts);
    for target in nugget_computer::targets::targets(&t) {
        assert!(text.contains(&target), "{target} not on screen:\n{text}");
    }
}
