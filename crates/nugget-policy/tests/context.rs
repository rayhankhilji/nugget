//! Context rules of the risk classifier: wording alone must not escalate a reversible click,
//! while real commands in the same places still do. Each category has its de-escalated cases
//! and their still-escalated counterparts.

use RiskClass::*;
use nugget_policy::{
    ActionContext, ActionDescriptor, ContextNode, Decision, KillSwitch, PolicyEngine, PolicyGate,
    RiskClass, RiskClassifier,
};

fn click(app: &str, label: &str, declared: RiskClass) -> ActionDescriptor {
    ActionDescriptor {
        target_app: app.into(),
        verb: "click".into(),
        target_label: Some(label.into()),
        text_preview: None,
        declared_risk: declared,
        task_id: "t".into(),
    }
}

fn risk(app: &str, label: &str, ctx: &ActionContext) -> RiskClass {
    RiskClassifier::global()
        .classify_with_context(&click(app, label, Edit), ctx)
        .risk
}

fn node(role: &str, name: Option<&str>) -> ContextNode {
    ContextNode::new(role, name.map(Into::into))
}

fn button() -> ActionContext {
    ActionContext::default().with_role("button")
}

fn assert_risks(app: &str, ctx: &ActionContext, cases: &[(&str, RiskClass)]) {
    for (label, want) in cases {
        assert_eq!(risk(app, label, ctx), *want, "{app} {label:?} in {ctx:?}");
    }
}

#[test]
fn toggles_ignore_send_and_spend_words_only() {
    for role in [
        "check_box",
        "checkbox",
        "switch",
        "radio_button",
        "menu_item_checkbox",
    ] {
        let ctx = ActionContext::default()
            .with_role(role)
            .with_window_title("Preferences");
        assert_risks(
            "forms",
            &ctx,
            &[
                ("Send me the city newsletter", Edit),
                ("Share usage data with partners", Edit),
                ("Auto-renew my plan", Edit),
                ("Subscribe to updates", Edit),
                ("Delete my account when I close the app", Destroy),
                ("Delete my data", Destroy),
                ("Allow remote access", Security),
                ("Send crash reports and delete logs", Destroy),
            ],
        );
    }
    // The same label on a button is a command.
    assert_eq!(
        risk("forms", "Send me the city newsletter", &button()),
        Send
    );
}

fn inbox_row(subject: &str) -> ActionContext {
    button().with_window_title("Mail").with_ancestors([
        node("list_item", Some(subject)),
        node("list", Some("Messages")),
        node("window", Some("Mail")),
    ])
}

#[test]
fn content_rows_do_not_escalate_on_their_subject_but_commands_do() {
    for (label, subject) in [
        (
            "Sam Ortiz: Renew the contract at +20%?",
            "Renew the contract at +20%?",
        ),
        (
            "Dana Lee: Please delete the old drafts before Friday",
            "Please delete the old drafts before Friday",
        ),
        (
            "IT desk: Your password was reset",
            "Your password was reset",
        ),
    ] {
        assert_eq!(risk("mail", label, &inbox_row(subject)), Edit, "{label}");
    }
    // A `Name: subject` label in a table cell; length alone does not make a label content.
    let cell = ActionContext::default().with_role("link").with_ancestors([
        node("cell", None),
        node("row", None),
        node("table", None),
    ]);
    assert_eq!(risk("tracker", "Ticket 42: remove the banner", &cell), Edit);
    assert_eq!(
        risk("tracker", "Ticket 42 asks us to remove the banner", &cell),
        Destroy
    );

    assert_risks(
        "mail",
        &inbox_row("Renew the contract at +20%?"),
        &[
            ("Delete", Destroy),
            ("Delete message", Destroy),
            ("🗑 Delete", Destroy),
            ("button \"Delete\"", Destroy),
            ("Archive", Destroy),
            ("Forward to Dana", Send),
            ("Reply all", Send),
            ("Pay", Spend),
            ("Pay invoice now", Spend),
            ("Renew the contract at +20%?", Spend),
            ("Allow", Security),
            // Long is not content: a listed command still counts.
            ("Click here to delete your account", Destroy),
        ],
    );
    // Outside any collection, wording still escalates.
    assert_eq!(
        risk("mail", "Sam Ortiz: Renew the contract at +20%?", &button()),
        Spend
    );
}

fn cart_line() -> ActionContext {
    button().with_window_title("Corner Shop").with_ancestors([
        node("list_item", Some("Gel pen ×2")),
        node("list", Some("Cart")),
        node("window", Some("Corner Shop")),
    ])
}

#[test]
fn removing_cart_lines_is_reversible_other_removals_are_not() {
    assert_risks(
        "shop",
        &cart_line(),
        &[
            ("Remove one Gel pen", Edit),
            ("Remove", Edit),
            ("Delete", Edit),
            ("Remove Gel pen – $2.50", Edit),
            ("Clear all", Edit),
            ("Delete account", Destroy),
            ("Remove card", Destroy),
            ("Remove saved address", Destroy),
            ("Permanently remove", Destroy),
            ("Remove payment method", Spend),
            ("Unsubscribe", Destroy),
            ("Pay $12.99", Spend),
        ],
    );
    // The cart can show in the URL, the title or the label instead of the ancestors.
    let by_url = button().with_url("https://shop.example.com/cart");
    assert_eq!(risk("shop.example.com", "Remove", &by_url), Edit);
    let by_title = ActionContext::default().with_window_title("Your basket");
    assert_eq!(risk("shop", "Delete item", &by_title), Edit);
    assert_eq!(
        risk("shop", "Remove from cart", &ActionContext::default()),
        Edit
    );
    let german = ActionContext::default().with_window_title("Warenkorb");
    assert_eq!(risk("shop", "Entfernen", &german), Edit);
    // A removal outside a cart is still Destroy.
    assert_eq!(risk("shop", "Remove one Gel pen", &button()), Destroy);
    assert_eq!(risk("files", "Delete", &button()), Destroy);
}

fn cart_view() -> ActionContext {
    button()
        .with_window_title("Corner Shop")
        .with_form_fields(["Search"])
        .with_ancestors([node("window", Some("Corner Shop"))])
}

#[test]
fn checkout_from_the_cart_is_navigation_unless_it_commits() {
    assert_risks(
        "shop",
        &cart_view(),
        &[
            ("Checkout", Edit),
            ("Check out", Edit),
            ("Proceed to checkout", Edit),
            ("Go to checkout", Edit),
            ("Checkout (3 items)", Edit),
            ("Afrekenen", Edit),
            ("去结账", Edit),
            ("Checkout – $49.99", Spend),
            ("Complete checkout", Spend),
            ("Checkout and pay", Spend),
            ("Place order", Spend),
            ("Buy now", Spend),
        ],
    );
    // Without any context too (documented).
    assert_eq!(risk("shop", "Checkout", &ActionContext::default()), Edit);
    // Payment fields, the submit button of a form with real fields, a dialog about paying.
    let pay_form = button().with_form_fields(["Card number", "CVV", "Expiry"]);
    let submit = button()
        .with_form_fields(["Full name", "Shipping address"])
        .with_submits(true);
    let dialog = ActionContext::default().with_dialog_text("You will be charged $49.99");
    for ctx in [pay_form, submit, dialog] {
        assert_eq!(risk("shop", "Checkout", &ctx), Spend, "{ctx:?}");
    }
    let form = button()
        .with_window_title("Corner Shop")
        .with_form_fields(["Name", "Address", "Card number"])
        .with_submits(true);
    assert_eq!(risk("shop", "Pay $12.99", &form), Spend);
    // A checkout URL is still Spend for navigation.
    let nav = ActionDescriptor {
        verb: "navigate".into(),
        target_label: None,
        text_preview: Some("https://shop.example.com/checkout".into()),
        ..click("x", "", Observe)
    };
    assert_eq!(RiskClassifier::global().classify(&nav).risk, Spend);
}

fn de_escalated() -> [(&'static str, &'static str, ActionContext); 4] {
    [
        ("shop", "Checkout", cart_view()),
        ("shop", "Remove one Gel pen", cart_line()),
        (
            "mail",
            "Sam Ortiz: Renew the contract at +20%?",
            inbox_row("Renew the contract at +20%?"),
        ),
        (
            "forms",
            "Send me the city newsletter",
            ActionContext::default().with_role("check_box"),
        ),
    ]
}

#[test]
fn context_never_lowers_the_declared_risk() {
    for declared in RiskClass::ALL {
        for (app, label, ctx) in de_escalated() {
            let c =
                RiskClassifier::global().classify_with_context(&click(app, label, declared), &ctx);
            assert!(c.risk >= declared, "{label}: {c:?}");
        }
    }
}

#[test]
fn engine_allows_the_measured_false_confirmations() {
    let e = PolicyEngine::builder()
        .kill_switch(KillSwitch::new())
        .build();
    for (app, label, ctx) in de_escalated() {
        let d = e.evaluate_with_context(&click(app, label, Edit), &ctx);
        assert_eq!(d, Decision::Allow, "{app} {label}");
    }
    let d = e.evaluate_with_context(
        &click("shop", "Pay $12.99", Spend),
        &ActionContext::default().with_form_fields(["Address", "Card number"]),
    );
    assert!(matches!(d, Decision::Confirm(_)), "{d:?}");
}
