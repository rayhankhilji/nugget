mod common;

use common::{checkout, form, id};
use nugget_browser::action::{
    BrowserAction, ValidationError, Verification, WaitCondition, keyword_risk, verify,
};
use nugget_browser::protocol::ActResult;
use nugget_browser::snapshot::Sensitive;
use nugget_policy::RiskClass;

#[test]
fn extension_snapshots_deserialize() {
    let s = form();
    assert_eq!(s.title, "Sign up");
    assert_eq!(s.host().as_deref(), Some("shop.example"));
    let pw = s.element(&id(&s, "Password")).unwrap();
    assert_eq!(pw.sensitive, Some(Sensitive::Password));
    assert_eq!(pw.value, None);
    let c = checkout();
    assert!(
        c.elements
            .iter()
            .filter(|e| e.sensitive == Some(Sensitive::Payment))
            .count()
            >= 4
    );
    // Round trip keeps the wire format.
    let again: nugget_browser::PageSnapshot =
        serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(again, s);
}

#[test]
fn validation_mirrors_the_extension() {
    let s = form();
    let ok = |a: BrowserAction| a.validate(Some(&s));
    assert_eq!(
        ok(BrowserAction::Click {
            element: id(&s, "Create account")
        }),
        Ok(())
    );
    assert_eq!(
        ok(BrowserAction::Type {
            element: id(&s, "Email address"),
            text: "a".into(),
            clear: None
        }),
        Ok(())
    );
    assert_eq!(
        ok(BrowserAction::Select {
            element: id(&s, "Plan"),
            value: "Free".into()
        }),
        Ok(())
    );
    assert_eq!(
        ok(BrowserAction::Navigate {
            url: "/next".into()
        }),
        Ok(())
    );

    assert!(matches!(
        ok(BrowserAction::Click {
            element: "n999".into()
        }),
        Err(ValidationError::UnknownElement(_))
    ));
    assert!(matches!(
        ok(BrowserAction::Click {
            element: id(&s, "Ghost")
        }),
        Err(ValidationError::NotVisible(_))
    ));
    assert!(matches!(
        ok(BrowserAction::Click {
            element: id(&s, "Disabled")
        }),
        Err(ValidationError::Disabled(_))
    ));
    assert!(matches!(
        ok(BrowserAction::Type {
            element: id(&s, "Password"),
            text: "x".into(),
            clear: None
        }),
        Err(ValidationError::SensitiveField {
            kind: Sensitive::Password,
            ..
        })
    ));
    let c = checkout();
    assert!(matches!(
        BrowserAction::Type {
            element: id(&c, "Card number"),
            text: "4".into(),
            clear: None
        }
        .validate(Some(&c)),
        Err(ValidationError::SensitiveField {
            kind: Sensitive::Payment,
            ..
        })
    ));
    assert!(matches!(
        ok(BrowserAction::Type {
            element: id(&s, "Create account"),
            text: "x".into(),
            clear: None
        }),
        Err(ValidationError::NotEditable(_))
    ));
    assert!(matches!(
        ok(BrowserAction::Select {
            element: id(&s, "Plan"),
            value: "gold".into()
        }),
        Err(ValidationError::BadOption { .. })
    ));
    assert!(matches!(
        ok(BrowserAction::Submit {
            element: id(&s, "Log in")
        }),
        Err(ValidationError::NotInForm(_))
    ));
    assert!(matches!(
        ok(BrowserAction::WaitFor {
            until: WaitCondition::TextPresent("x".into()),
            timeout_ms: Some(60_000)
        }),
        Err(ValidationError::Invalid(_))
    ));
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
        "https://u:p@x.example/",
    ] {
        assert!(
            matches!(
                ok(BrowserAction::Navigate { url: bad.into() }),
                Err(ValidationError::BadUrl(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        BrowserAction::Click {
            element: "n1".into()
        }
        .validate(None),
        Err(ValidationError::NoSnapshot)
    );
    assert_eq!(
        BrowserAction::Navigate {
            url: "https://a.example/".into()
        }
        .validate(None),
        Ok(())
    );
}

#[test]
fn risk_classification() {
    let s = form();
    let risk = |a: BrowserAction| a.classify(Some(&s));
    assert_eq!(
        risk(BrowserAction::Scroll {
            element: None,
            dx: None,
            dy: None
        }),
        RiskClass::Observe
    );
    assert_eq!(
        risk(BrowserAction::Click {
            element: id(&s, "Log in")
        }),
        RiskClass::Observe
    );
    assert_eq!(
        risk(BrowserAction::Click {
            element: id(&s, "I agree to the terms")
        }),
        RiskClass::Edit
    );
    assert_eq!(
        risk(BrowserAction::Type {
            element: id(&s, "Email address"),
            text: "a".into(),
            clear: None
        }),
        RiskClass::Edit
    );
    // The signup form holds a password field: submitting it is a security action.
    assert_eq!(
        risk(BrowserAction::Click {
            element: id(&s, "Create account")
        }),
        RiskClass::Security
    );
    assert_eq!(
        risk(BrowserAction::Submit {
            element: id(&s, "Email address")
        }),
        RiskClass::Security
    );
    let c = checkout();
    assert_eq!(
        BrowserAction::Submit {
            element: id(&c, "Coupon")
        }
        .classify(Some(&c)),
        RiskClass::Spend
    );
    assert_eq!(
        BrowserAction::Click {
            element: id(&c, "Pay $20.00")
        }
        .classify(Some(&c)),
        RiskClass::Spend
    );

    assert_eq!(keyword_risk("Pay now"), Some(RiskClass::Spend));
    assert_eq!(keyword_risk("Display settings"), None);
    assert_eq!(keyword_risk("Delete account"), Some(RiskClass::Destroy));
    assert_eq!(keyword_risk("Send"), Some(RiskClass::Send));
    assert_eq!(keyword_risk("Change password"), Some(RiskClass::Security));
}

#[test]
fn descriptors_name_the_site_and_preview_text() {
    let s = form();
    let d = BrowserAction::Type {
        element: id(&s, "Email address"),
        text: "x".repeat(200),
        clear: None,
    }
    .describe(Some(&s), "t1");
    assert_eq!(d.target_app, "shop.example");
    assert_eq!(d.verb, "set_value");
    assert_eq!(d.target_label.as_deref(), Some("textbox \"Email address\""));
    assert_eq!(d.text_preview.as_ref().unwrap().chars().count(), 81);
    assert_eq!(d.task_id, "t1");
    let n = BrowserAction::Navigate {
        url: "https://Bank.Example/login".into(),
    }
    .describe(Some(&s), "t1");
    assert_eq!(n.target_app, "bank.example");
    let rel = BrowserAction::Navigate {
        url: "/cart".into(),
    }
    .describe(Some(&s), "t1");
    assert_eq!(rel.target_app, "shop.example");
}

#[test]
fn verification_uses_the_post_action_snapshot() {
    let s = form();
    let r: ActResult = serde_json::from_str(&common::fixture("form-act-type.json")).unwrap();
    let email = id(&s, "Email address");
    let typed = BrowserAction::Type {
        element: email.clone(),
        text: "grace@example.com".into(),
        clear: None,
    };
    assert!(verify(&typed, Some(&s), &r.snapshot, &r.diff).is_confirmed());
    let wrong = BrowserAction::Type {
        element: email,
        text: "other".into(),
        clear: None,
    };
    assert!(matches!(
        verify(&wrong, Some(&s), &r.snapshot, &r.diff),
        Verification::Mismatch(_)
    ));
    let click = BrowserAction::Click {
        element: id(&s, "Log in"),
    };
    assert_eq!(
        verify(&click, Some(&s), &s, &Default::default()),
        Verification::NoVisibleEffect
    );
    assert!(verify(&click, Some(&s), &r.snapshot, &r.diff).is_confirmed());
    let nav = BrowserAction::Navigate {
        url: "/next".into(),
    };
    assert!(matches!(
        verify(&nav, Some(&s), &s, &Default::default()),
        Verification::Mismatch(_)
    ));
}
