//! Each tool's call, risk, describe, redact and verify against the fake backend.

mod common;

use std::time::{Duration, Instant};

use common::*;
use nugget_a11y::role::Role;
use nugget_agent::Verification;
use nugget_computer::ComputerConfig;
use nugget_policy::RiskClass;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[test]
fn exact_tool_set() {
    let kit = Kit::new(vec![mail()]);
    let mut names: Vec<_> = kit.tools.iter().map(|t| t.name().to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "click",
            "observe",
            "open_app",
            "press_key",
            "scroll",
            "set_value",
            "type_text",
            "wait_for"
        ]
    );
    for t in &kit.tools {
        assert_eq!(t.schema()["type"], "object", "{}", t.name());
        assert!(!t.description().is_empty());
    }
    let declared = |n: &str| kit.tool(n).declared_risk();
    assert_eq!(declared("observe"), RiskClass::Observe);
    assert_eq!(declared("scroll"), RiskClass::Observe);
    assert_eq!(declared("wait_for"), RiskClass::Observe);
    assert_eq!(declared("click"), RiskClass::Security);
}

#[tokio::test]
async fn observe_renders_untrusted_tree_and_other_windows() {
    let kit = Kit::new(vec![mail(), terminal()]);
    let r = kit.look().await;
    assert!(r.untrusted && !r.is_error);
    let text = &r.content;
    assert!(
        text.starts_with("window [w1] \"New Message\" app \"Mail\"\n"),
        "{text}"
    );
    assert!(text.contains("focused [eto]"));
    assert!(text.contains("[esend] button \"Send\"\n"));
    assert!(text.contains("[eto] text_field \"To\" focused"));
    assert!(text.contains("[epw] text_field \"Password\" value=hidden"));
    // The unnamed group is collapsed: Bold sits at the top level.
    assert!(text.contains("\n[ebold] button \"Bold\""));
    assert!(text.contains("other windows:\n  window [w2] app \"Terminal\" \"bash\""));
    assert_eq!(r.data.as_ref().unwrap()["window"], "w1");
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());

    let r = kit.call("observe", json!({"app": "terminal"})).await;
    assert!(r.content.starts_with("window [w2]"));
    let r = kit.call("observe", json!({"window": "w2"})).await;
    assert!(r.content.starts_with("window [w2]"));
    let r = kit.call("observe", json!({"window": "nope"})).await;
    assert!(r.is_error);
}

#[tokio::test]
async fn risk_table() {
    let kit = Kit::new(vec![mail(), terminal()]);
    let risk = |n: &str, a: serde_json::Value| kit.tool(n).risk(&a);
    // Nothing observed yet: every element is unknown.
    assert_eq!(risk("click", json!({"element": "ebold"})), RiskClass::Send);
    assert_eq!(
        risk("set_value", json!({"element": "eto", "value": "x"})),
        RiskClass::Send
    );
    kit.look().await;
    let click = |id: &str| risk("click", json!({"element": id}));
    assert_eq!(click("esend"), RiskClass::Send);
    assert_eq!(click("edelete"), RiskClass::Destroy);
    assert_eq!(click("ebuy"), RiskClass::Spend);
    assert_eq!(click("eperm"), RiskClass::Security);
    assert_eq!(click("ebold"), RiskClass::Edit);
    assert_eq!(click("eto"), RiskClass::Edit);
    assert_eq!(click("em1"), RiskClass::Edit);
    // "OK" in a "Delete 3 files?" dialog.
    assert_eq!(click("eok"), RiskClass::Destroy);
    // Unknown id: fail safe.
    assert_eq!(click("e404"), RiskClass::Send);
    assert_eq!(click("[not an id]"), RiskClass::Send);

    let ty = |a| risk("type_text", a);
    assert_eq!(ty(json!({"text": "hi"})), RiskClass::Edit);
    assert_eq!(
        ty(json!({"text": "hi", "element": "ebody"})),
        RiskClass::Edit
    );
    assert_eq!(
        ty(json!({"text": "x", "element": "epw"})),
        RiskClass::Security
    );
    assert_eq!(ty(json!({"text": "x", "element": "e404"})), RiskClass::Send);
    assert_eq!(
        risk("set_value", json!({"element": "eprio", "value": "High"})),
        RiskClass::Edit
    );

    let keys = |k: &str, el: Option<&str>| {
        let mut a = json!({"keys": k});
        if let Some(e) = el {
            a["element"] = json!(e);
        }
        risk("press_key", a)
    };
    // Focus is the To field (single line).
    assert_eq!(keys("enter", None), RiskClass::Send);
    assert_eq!(keys("enter", Some("ebody")), RiskClass::Edit);
    assert_eq!(keys("ctrl+enter", Some("ebody")), RiskClass::Send);
    assert_eq!(keys("cmd+enter", Some("ebody")), RiskClass::Send);
    assert_eq!(keys("shift+delete", None), RiskClass::Destroy);
    assert_eq!(keys("ctrl+a", None), RiskClass::Edit);
    assert_eq!(keys("backspace", None), RiskClass::Edit);
    assert_eq!(keys("delete", Some("em1")), RiskClass::Destroy);
    assert_eq!(keys("space", Some("esend")), RiskClass::Send);
    assert_eq!(keys("enter", Some("e404")), RiskClass::Send);

    assert_eq!(
        risk("scroll", json!({"element": "elist", "direction": "down"})),
        RiskClass::Observe
    );
    assert_eq!(risk("wait_for", json!({"name": "x"})), RiskClass::Observe);
    assert_eq!(risk("observe", json!({})), RiskClass::Observe);
    assert_eq!(risk("open_app", json!({"name": "Notes"})), RiskClass::Edit);
    assert_eq!(
        risk("open_app", json!({"name": "Passwords"})),
        RiskClass::Security
    );
}

#[tokio::test]
async fn terminals_are_security() {
    let kit = Kit::new(vec![terminal(), mail()]);
    kit.look().await;
    // Focus is the terminal's text area.
    for (tool, args) in [
        ("type_text", json!({"text": "ls"})),
        ("type_text", json!({"text": "ls", "element": "eterm"})),
        ("press_key", json!({"keys": "tab"})),
        (
            "set_value",
            json!({"element": "eterm", "value": "rm -rf ~"}),
        ),
    ] {
        let t = kit.tool(tool);
        assert_eq!(t.risk(&args), RiskClass::Security, "{tool} {args}");
        let d = t.describe(&args);
        assert!(
            d.target_label.as_deref().unwrap().starts_with("terminal:"),
            "{d:?}"
        );
        assert_eq!(d.target_app, "com.apple.Terminal");
    }

    // A terminal the built-in list does not know, added by config; and a terminal role.
    let mut odd = window(
        "w9",
        "Shelly",
        None,
        "shelly",
        vec![
            n("es", Role::TextArea, "input"),
            n("ert", Role::Unknown, "").raw("terminal"),
        ],
    );
    odd.focused = Some(nugget_a11y::ElementId("es".into()));
    let plain = Kit::new(vec![odd.clone()]);
    plain.look().await;
    assert_eq!(
        plain.tool("type_text").risk(&json!({"text": "x"})),
        RiskClass::Edit
    );
    assert_eq!(
        plain
            .tool("press_key")
            .risk(&json!({"keys": "a", "element": "ert"})),
        RiskClass::Security
    );
    let cfg = ComputerConfig {
        terminal_apps: vec!["shelly".into()],
        ..ComputerConfig::default()
    };
    let kit = Kit::with_config(vec![odd], cfg);
    kit.look().await;
    assert_eq!(
        kit.tool("type_text").risk(&json!({"text": "x"})),
        RiskClass::Security
    );
}

#[tokio::test]
async fn describe_and_redact() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let d = kit.tool("click").describe(&json!({"element": "esend"}));
    assert_eq!(d.target_app, "com.apple.mail");
    assert_eq!(d.verb, "click");
    assert_eq!(d.target_label.as_deref(), Some("Send"));
    let d = kit.tool("click").describe(&json!({"element": "e404"}));
    assert_eq!(d.target_label.as_deref(), Some("unknown element e404"));

    let long = "x".repeat(500);
    let d = kit
        .tool("type_text")
        .describe(&json!({"text": long, "element": "esubject"}));
    assert_eq!(d.target_label.as_deref(), Some("Subject"));
    assert_eq!(d.text_preview.as_deref().unwrap().chars().count(), 121);
    let d = kit.tool("type_text").describe(&json!({"text": "hello"}));
    assert_eq!(d.target_label.as_deref(), Some("To"));
    assert_eq!(d.text_preview.as_deref(), Some("hello"));

    let d = kit
        .tool("type_text")
        .describe(&json!({"text": "hunter2", "element": "epw"}));
    assert!(!d.text_preview.unwrap().contains("hunter2"));
    let r = kit
        .tool("type_text")
        .redact(&json!({"text": "hunter2", "element": "epw"}));
    assert_eq!(r["text"], "[redacted]");
    let r = kit
        .tool("set_value")
        .redact(&json!({"value": "hunter2", "element": "epw"}));
    assert_eq!(r["value"], "[redacted]");
    let r = kit
        .tool("type_text")
        .redact(&json!({"text": "hello", "element": "eto"}));
    assert_eq!(r["text"], "hello");

    let d = kit
        .tool("press_key")
        .describe(&json!({"keys": "cmd+Enter"}));
    assert_eq!(d.text_preview.as_deref(), Some("meta+enter"));
    let d = kit.tool("open_app").describe(&json!({"name": "Notes"}));
    assert_eq!(
        (d.target_app.as_str(), d.verb.as_str()),
        ("Notes", "open_app")
    );
}

#[tokio::test]
async fn click_send_passes_when_the_window_closes() {
    let kit = Kit::new(vec![mail()]);
    kit.cu.on("esend", |w| w.close("w1"));
    kit.look().await;
    let (r, v) = kit.act("click", json!({"element": "esend"})).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.untrusted);
    assert!(
        matches!(v, Verification::Passed(ref d) if d.contains("window closed")),
        "{v:?}"
    );
    assert!(kit.cu.acts().contains(&"press:esend".to_string()));
}

#[tokio::test]
async fn click_verification_needs_backend_and_observation() {
    // Nothing happens and the backend agrees: failed.
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let (_, v) = kit.act("click", json!({"element": "ebold"})).await;
    assert!(matches!(v, Verification::Failed(_)), "{v:?}");

    // The backend claims success but nothing visible changed: unverifiable, never passed.
    kit.cu.with(|w| w.force_verified = Some(true));
    let (_, v) = kit.act("click", json!({"element": "ebold"})).await;
    assert_eq!(v, Verification::Unverifiable);

    // The window changed but the backend did not see it: unverifiable.
    kit.cu.with(|w| w.force_verified = Some(false));
    kit.cu.on("ebold", |w| {
        w.node_mut("ebody").unwrap().value = Some("**".into())
    });
    let (_, v) = kit.act("click", json!({"element": "ebold"})).await;
    assert_eq!(v, Verification::Unverifiable);

    // Both see it: passed.
    kit.cu.with(|w| w.force_verified = None);
    kit.cu.on("ebold", |w| {
        w.node_mut("ebody").unwrap().value = Some("****".into())
    });
    let (_, v) = kit.act("click", json!({"element": "ebold"})).await;
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");

    // A stale id fails without acting on anything else.
    let (r, v) = kit.act("click", json!({"element": "e404"})).await;
    assert!(r.is_error);
    assert!(matches!(v, Verification::Failed(_)));
}

#[tokio::test]
async fn click_selects_list_items_and_refuses_disabled() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let (r, v) = kit.act("click", json!({"element": "em2"})).await;
    assert_eq!(r.data.as_ref().unwrap()["action"], "select");
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
    assert!(kit.cu.with(|w| w.node("em2").unwrap().states.selected));

    kit.cu
        .with(|w| w.node_mut("ebold").unwrap().states.disabled = true);
    kit.look().await;
    let r = kit.call("click", json!({"element": "ebold"})).await;
    assert!(r.is_error && r.content.contains("disabled"));
    assert!(!kit.cu.acts().contains(&"press:ebold".to_string()));
}

#[tokio::test]
async fn type_text_verifies_by_reading_back() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let (r, v) = kit
        .act("type_text", json!({"text": "bob@example.org"}))
        .await;
    assert!(!r.is_error);
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
    assert_eq!(
        kit.cu.with(|w| w.node("eto").unwrap().value),
        Some("bob@example.org".into())
    );

    // The app swallows the text but the backend claims success: failed (contradiction).
    kit.cu.with(|w| {
        w.drop_typing = true;
        w.force_verified = Some(true);
    });
    let (_, v) = kit
        .act("type_text", json!({"text": "Hello", "element": "esubject"}))
        .await;
    assert!(matches!(v, Verification::Failed(_)), "{v:?}");
}

#[tokio::test]
async fn secure_fields_are_never_typed_into() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    for (tool, args) in [
        ("type_text", json!({"text": "hunter2", "element": "epw"})),
        ("set_value", json!({"value": "hunter2", "element": "epw"})),
    ] {
        let r = kit.call(tool, args).await;
        assert!(r.is_error && r.content.contains("secure"), "{}", r.content);
        assert!(!r.content.contains("hunter2"));
    }
    // Focus on the password field, no element given.
    kit.cu
        .with(|w| w.windows[0].focused = Some(nugget_a11y::ElementId("epw".into())));
    kit.look().await;
    let r = kit.call("type_text", json!({"text": "hunter2"})).await;
    assert!(r.is_error);
    assert!(kit.cu.acts().iter().all(|a| !a.contains("epw")));
}

#[tokio::test]
async fn set_value_on_fields_and_choices() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let (_, v) = kit
        .act("set_value", json!({"element": "esubject", "value": "Hi"}))
        .await;
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");

    let (r, v) = kit
        .act("set_value", json!({"element": "eprio", "value": "high"}))
        .await;
    assert_eq!(r.data.as_ref().unwrap()["method"], "select");
    assert_eq!(r.data.as_ref().unwrap()["option"], "ehigh");
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
    assert_eq!(
        kit.cu.with(|w| w.node("eprio").unwrap().value),
        Some("High".into())
    );

    // An option missing from the closed combo box appears once expanded.
    kit.cu.on("expand:eprio", |w| {
        w.node_mut("eprio")
            .unwrap()
            .children
            .push(n("eurgent", Role::ListItem, "Urgent"));
    });
    let (_, v) = kit
        .act("set_value", json!({"element": "eprio", "value": "Urgent"}))
        .await;
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
    assert!(kit.cu.acts().contains(&"expand:eprio".to_string()));

    let r = kit
        .call("set_value", json!({"element": "eprio", "value": "Nope"}))
        .await;
    assert!(
        r.is_error && r.content.contains("no option"),
        "{}",
        r.content
    );

    // The app writes something else: failed.
    kit.cu.with(|w| {
        w.wrong_value = Some("Hx".into());
        w.force_verified = Some(true);
    });
    let (_, v) = kit
        .act(
            "set_value",
            json!({"element": "esubject", "value": "Hello"}),
        )
        .await;
    assert!(
        matches!(v, Verification::Failed(ref d) if d.contains("Hx")),
        "{v:?}"
    );
}

#[tokio::test]
async fn press_key_is_strict_and_verified() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let r = kit.call("press_key", json!({"keys": "ctrl+banana"})).await;
    assert!(r.is_error && r.content.contains("Nothing was pressed"));
    let r = kit.call("press_key", json!({"keys": "hello world"})).await;
    assert!(r.is_error);
    assert!(kit.cu.acts().is_empty());

    kit.cu.on("key:enter", |w| w.close("w1"));
    let (r, v) = kit.act("press_key", json!({"keys": "enter"})).await;
    assert!(!r.is_error);
    assert_eq!(r.data.as_ref().unwrap()["keys"], json!(["enter"]));
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
}

#[tokio::test]
async fn scroll_and_open_app() {
    let kit = Kit::new(vec![mail(), terminal()]);
    kit.look().await;
    let r = kit
        .call(
            "scroll",
            json!({"element": "elist", "direction": "down", "amount": 500}),
        )
        .await;
    assert!(!r.is_error);
    assert!(kit.cu.acts().contains(&"scroll:elist".to_string()));

    let (r, v) = kit.act("open_app", json!({"name": "Terminal"})).await;
    assert!(!r.is_error);
    assert!(matches!(v, Verification::Passed(_)), "{v:?}");
    let (_, v) = kit.act("open_app", json!({"name": "Nonexistent"})).await;
    assert!(matches!(v, Verification::Failed(_)), "{v:?}");
}

#[tokio::test]
async fn wait_for_appears_disappears_times_out_and_cancels() {
    let cfg = ComputerConfig {
        wait_poll_ms: 10,
        ..ComputerConfig::default()
    };
    let kit = Kit::with_config(vec![mail()], cfg);
    kit.cu.with(|w| {
        w.on_snapshot = Some(Box::new(|w, count| {
            if count == 3 {
                w.windows[0]
                    .root
                    .children
                    .push(n("edone", Role::StaticText, "Message sent"));
            }
        }))
    });
    let r = kit.call("wait_for", json!({"name": "sent"})).await;
    assert!(!r.is_error && r.untrusted, "{}", r.content);
    assert!(r.content.contains("[edone] static_text \"Message sent\""));
    assert_eq!(kit.cu.with(|w| w.snapshots), 3);

    let r = kit
        .call("wait_for", json!({"role": "button", "name": "send"}))
        .await;
    assert!(!r.is_error);

    let r = kit
        .call(
            "wait_for",
            json!({"name": "Delete draft", "until": "disappears", "timeout_ms": 50}),
        )
        .await;
    assert!(
        r.is_error && r.content.contains("timed out after 50 ms"),
        "{}",
        r.content
    );

    kit.cu.on("esend", |w| w.close("w1"));
    kit.look().await;
    kit.call("click", json!({"element": "esend"})).await;
    let r = kit
        .call(
            "wait_for",
            json!({"name": "Send", "until": "disappears", "window": "w1"}),
        )
        .await;
    assert!(!r.is_error, "{}", r.content);

    let r = kit.call("wait_for", json!({})).await;
    assert!(r.is_error);
}

#[tokio::test]
async fn wait_for_cancel_and_cap() {
    let cfg = ComputerConfig {
        wait_poll_ms: 10,
        wait_max_timeout_ms: 100,
        ..ComputerConfig::default()
    };
    let kit = Kit::with_config(vec![mail()], cfg);
    // The timeout is capped.
    let t0 = Instant::now();
    let r = kit
        .call(
            "wait_for",
            json!({"name": "never", "timeout_ms": 3_600_000}),
        )
        .await;
    assert!(r.is_error && r.content.contains("100 ms"));
    assert!(t0.elapsed() < Duration::from_secs(5));

    let kit = Kit::new(vec![mail()]);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHORT).await;
        c2.cancel();
    });
    let t0 = Instant::now();
    let r = kit
        .tool("wait_for")
        .call(json!({"name": "never"}), &ctx_with(cancel))
        .await;
    assert!(r.is_error && r.content.contains("cancelled"));
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn screenshot_fallback_is_gated() {
    let empty = || {
        window(
            "w5",
            "Canvas",
            None,
            "Drawing",
            vec![n("", Role::Group, "").kids(vec![n("eimg", Role::Image, "")])],
        )
    };
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];

    // Off by default.
    let kit = Kit::new(vec![empty()]);
    kit.cu.with(|w| w.screenshot = Some(png.clone()));
    let r = kit.look().await;
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());
    assert!(r.content.contains("(no accessible elements)"));

    let on = ComputerConfig {
        screenshot_fallback: true,
        ..ComputerConfig::default()
    };
    let kit = Kit::with_config(vec![empty()], on.clone());
    kit.cu.with(|w| w.screenshot = Some(png.clone()));
    let r = kit.look().await;
    let shot = &r.data.as_ref().unwrap()["screenshot"];
    assert_eq!(shot["mime"], "image/png");
    assert_eq!(shot["base64"], "iVBORwECAw==");
    assert!(r.content.contains("screenshot"));
    assert!(!r.content.contains("iVBOR"));

    // A tree with something to act on never falls back.
    let kit = Kit::with_config(vec![mail()], on);
    kit.cu.with(|w| w.screenshot = Some(png));
    let r = kit.look().await;
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());
}
