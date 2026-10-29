//! Context rules of the risk classifier: wording alone must not escalate a reversible click,
//! while real commands in the same places still do. Each category has its de-escalated cases
//! and their still-escalated counterparts.

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
        .classify_with_context(&click(app, label, RiskClass::Edit), ctx)
        .risk
}

fn node(role: &str, name: Option<&str>) -> ContextNode {
    ContextNode::new(role, name.map(Into::into))
}

fn assert_risks(app: &str, ctx: &ActionContext, cases: &[(&str, RiskClass)]) {
    for (label, want) in cases {
        assert_eq!(risk(app, label, ctx), *want, "{app} {label:?} in {ctx:?}");
    }
}

// --- Toggles -------------------------------------------------------------------------------

#[test]
fn toggles_ignore_send_and_spend_words() {
    use RiskClass::*;
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
            ],
        );
    }
}

#[test]
fn toggles_still_escalate_destroy_and_security_words() {
    use RiskClass::*;
    let ctx = ActionContext::default().with_role("check_box");
    assert_risks(
        "forms",
        &ctx,
        &[
            ("Delete my account when I close the app", Destroy),
            ("Delete my data", Destroy),
            ("Allow remote access", Security),
            ("Send crash reports and delete logs", Destroy),
        ],
    );
    // The same label on a button is a command.
    let button = ActionContext::default().with_role("button");
    assert_eq!(risk("forms", "Send me the city newsletter", &button), Send);
}

// --- Content rows --------------------------------------------------------------------------

fn inbox_row(subject: &str) -> ActionContext {
    ActionContext::default()
        .with_role("button")
        .with_window_title("Mail")
        .with_ancestors([
            node("list_item", Some(subject)),
            node("list", Some("Messages")),
            node("window", Some("Mail")),
        ])
}

#[test]
fn content_rows_do_not_escalate_on_their_subject() {
    use RiskClass::*;
    let ctx = inbox_row("Renew the contract at +20%?");
    assert_eq!(
        risk("mail", "Sam Ortiz: Renew the contract at +20%?", &ctx),
        Edit
    );
    let ctx = inbox_row("Please delete the old drafts before Friday");
    assert_eq!(
        risk(
            "mail",
            "Dana Lee: Please delete the old drafts before Friday",
            &ctx
        ),
        Edit
    );
    let ctx = inbox_row("Your password was reset");
    assert_eq!(risk("mail", "IT desk: Your password was reset", &ctx), Edit);
    // A `Name: subject` label in a table cell.
    let ctx = ActionContext::default().with_role("link").with_ancestors([
        node("cell", None),
        node("row", None),
        node("table", None),
    ]);
    assert_eq!(risk("tracker", "Ticket 42: remove the banner", &ctx), Edit);
    // Length alone does not make a label content.
    assert_eq!(
        risk("tracker", "Ticket 42 asks us to remove the banner", &ctx),
        Destroy
    );
}

#[test]
fn commands_in_rows_still_escalate() {
    use RiskClass::*;
    let ctx = inbox_row("Renew the contract at +20%?");
    assert_risks(
        "mail",
        &ctx,
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
    // Outside any collection, wording still escalates as before.
    let plain = ActionContext::default().with_role("button");
    assert_eq!(
        risk("mail", "Sam Ortiz: Renew the contract at +20%?", &plain),
        Spend
    );
}

// --- Cart lines ----------------------------------------------------------------------------

fn cart_line() -> ActionContext {
    ActionContext::default()
        .with_role("button")
        .with_window_title("Corner Shop")
        .with_ancestors([
            node("list_item", Some("Gel pen ×2")),
            node("list", Some("Cart")),
            node("window", Some("Corner Shop")),
        ])
}

#[test]
fn removing_cart_lines_is_reversible() {
    use RiskClass::*;
    assert_risks(
        "shop",
        &cart_line(),
        &[
            ("Remove one Gel pen", Edit),
            ("Remove", Edit),
            ("Delete", Edit),
            ("Remove Gel pen – $2.50", Edit),
            ("Clear all", Edit),
        ],
    );
    // The cart can show in the URL, the title or the label instead of the ancestors.
    let by_url = ActionContext::default()
        .with_role("button")
        .with_url("https://shop.example.com/cart");
    assert_eq!(risk("shop.example.com", "Remove", &by_url), Edit);
    let by_title = ActionContext::default().with_window_title("Your basket");
    assert_eq!(risk("shop", "Delete item", &by_title), Edit);
    assert_eq!(
        risk("shop", "Remove from cart", &ActionContext::default()),
        Edit
    );
    let german = ActionContext::default().with_window_title("Warenkorb");
    assert_eq!(risk("shop", "Entfernen", &german), Edit);
}

#[test]
fn cart_pages_still_escalate_other_removals() {
    use RiskClass::*;
    assert_risks(
        "shop",
        &cart_line(),
        &[
            ("Delete account", Destroy),
            ("Remove card", Destroy),
            ("Remove saved address", Destroy),
            ("Permanently remove", Destroy),
            ("Remove payment method", Spend),
            ("Unsubscribe", Destroy),
            ("Pay $12.99", Spend),
        ],
    );
    // A removal outside a cart is still Destroy.
    let plain = ActionContext::default().with_role("button");
    assert_eq!(risk("shop", "Remove one Gel pen", &plain), Destroy);
    assert_eq!(risk("files", "Delete", &plain), Destroy);
}

// --- Checkout navigation -------------------------------------------------------------------

fn cart_view() -> ActionContext {
    ActionContext::default()
        .with_role("button")
        .with_window_title("Corner Shop")
        .with_form_fields(["Search"])
        .with_ancestors([node("window", Some("Corner Shop"))])
}

#[test]
fn checkout_from_the_cart_is_navigation() {
    use RiskClass::*;
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
        ],
    );
    // Without any context too (documented).
    assert_eq!(risk("shop", "Checkout", &ActionContext::default()), Edit);
}

#[test]
fn checkout_that_commits_stays_spend() {
    use RiskClass::*;
    // A price in the label.
    assert_eq!(risk("shop", "Checkout – $49.99", &cart_view()), Spend);
    // Payment fields in the form.
    let pay_form = ActionContext::default()
        .with_role("button")
        .with_form_fields(["Card number", "CVV", "Expiry"]);
    assert_eq!(risk("shop", "Checkout", &pay_form), Spend);
    // The submit button of a form with real fields.
    let submit = ActionContext::default()
        .with_role("button")
        .with_form_fields(["Full name", "Shipping address"])
        .with_submits(true);
    assert_eq!(risk("shop", "Checkout", &submit), Spend);
    // A dialog about paying.
    let dialog = ActionContext::default().with_dialog_text("You will be charged $49.99");
    assert_eq!(risk("shop", "Checkout", &dialog), Spend);
    // More than a bare checkout step.
    assert_risks(
        "shop",
        &cart_view(),
        &[
            ("Complete checkout", Spend),
            ("Checkout and pay", Spend),
            ("Place order", Spend),
            ("Buy now", Spend),
        ],
    );
    // A checkout URL is still Spend for navigation.
    let nav = ActionDescriptor {
        verb: "navigate".into(),
        target_label: None,
        text_preview: Some("https://shop.example.com/checkout".into()),
        ..click("x", "", RiskClass::Observe)
    };
    assert_eq!(RiskClassifier::global().classify(&nav).risk, Spend);
}

#[test]
fn pay_button_in_the_checkout_form_stays_spend() {
    let form = ActionContext::default()
        .with_role("button")
        .with_window_title("Corner Shop")
        .with_form_fields(["Name", "Address", "Card number"])
        .with_submits(true);
    let c = RiskClassifier::global()
        .classify_with_context(&click("shop", "Pay $12.99", RiskClass::Spend), &form);
    assert_eq!(c.risk, RiskClass::Spend);
}

// --- Invariants ----------------------------------------------------------------------------

#[test]
fn context_never_lowers_the_declared_risk() {
    for declared in RiskClass::ALL {
        for (label, ctx) in [
            ("Checkout", cart_view()),
            ("Remove one Gel pen", cart_line()),
            (
                "Sam Ortiz: Renew the contract at +20%?",
                inbox_row("Renew the contract at +20%?"),
            ),
            (
                "Send me the city newsletter",
                ActionContext::default().with_role("check_box"),
            ),
        ] {
            let c = RiskClassifier::global()
                .classify_with_context(&click("shop", label, declared), &ctx);
            assert!(c.risk >= declared, "{label}: {c:?}");
        }
    }
}

#[test]
fn engine_allows_the_measured_false_confirmations() {
    let e = PolicyEngine::builder()
        .kill_switch(KillSwitch::new())
        .build();
    for (app, label, ctx) in [
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
    ] {
        let d = e.evaluate_with_context(&click(app, label, RiskClass::Edit), &ctx);
        assert_eq!(d, Decision::Allow, "{app} {label}");
    }
    let d = e.evaluate_with_context(
        &click("shop", "Pay $12.99", RiskClass::Spend),
        &ActionContext::default().with_form_fields(["Address", "Card number"]),
    );
    assert!(matches!(d, Decision::Confirm(_)), "{d:?}");
}
