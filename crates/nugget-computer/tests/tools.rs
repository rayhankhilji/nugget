//! Each tool's call, risk, describe, redact and verify against the fake backend.

mod common;

use common::*;
use nugget_a11y::role::Role;
use nugget_agent::Verification;
use nugget_computer::ComputerConfig;
use nugget_policy::RiskClass;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[test]
fn exact_tool_set() {
    let kit = Kit::new(vec![mail()]);
    let mut names: Vec<_> = kit.tools.iter().map(|t| t.name().to_string()).collect();
    names.sort();
    let want = [
        "click",
        "observe",
        "open_app",
        "press_key",
        "scroll",
        "set_value",
        "type_text",
        "wait_for",
    ];
    assert_eq!(names, want);
    for t in &kit.tools {
        assert_eq!(t.schema()["type"], "object", "{}", t.name());
        assert!(!t.description().is_empty());
    }
    for (name, risk) in [
        ("observe", RiskClass::Observe),
        ("scroll", RiskClass::Observe),
        ("wait_for", RiskClass::Observe),
        ("click", RiskClass::Security),
    ] {
        assert_eq!(kit.tool(name).declared_risk(), risk, "{name}");
    }
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
    for want in [
        "focused [eto]",
        "[esend] button \"Send\"\n",
        "[eto] text_field \"To\" focused",
        "[epw] text_field \"Password\" value=hidden",
        // The unnamed group is collapsed: Bold sits at the top level.
        "\n[ebold] button \"Bold\"",
        "other windows:\n  window [w2] app \"Terminal\" \"bash\"",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert_eq!(r.data.as_ref().unwrap()["window"], "w1");
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());

    for args in [json!({"app": "terminal"}), json!({"window": "w2"})] {
        assert!(
            kit.call("observe", args)
                .await
                .content
                .starts_with("window [w2]")
        );
    }
    assert!(
        kit.call("observe", json!({"window": "nope"}))
            .await
            .is_error
    );
}

#[tokio::test]
async fn risk_table() {
    let kit = Kit::new(vec![mail(), terminal()]);
    let risk = |n: &str, a: Value| kit.tool(n).risk(&a);
    // Nothing observed yet: every element is unknown. Edit, not Send (GG3-13): `call` refuses
    // an id that is not on the cached screen, so there is nothing to confirm.
    assert_eq!(risk("click", json!({"element": "ebold"})), RiskClass::Edit);
    assert_eq!(
        risk("set_value", json!({"element": "eto", "value": "x"})),
        RiskClass::Edit
    );
    kit.look().await;
    for (id, want) in [
        ("esend", RiskClass::Send),
        ("edelete", RiskClass::Destroy),
        ("ebuy", RiskClass::Spend),
        ("eperm", RiskClass::Security),
        ("ebold", RiskClass::Edit),
        ("eto", RiskClass::Edit),
        ("em1", RiskClass::Edit),
        // "OK" in a "Delete 3 files?" dialog.
        ("eok", RiskClass::Destroy),
        // Unknown or malformed id: Edit, and never acted on (GG3-13).
        ("e404", RiskClass::Edit),
        ("[not an id]", RiskClass::Edit),
    ] {
        assert_eq!(risk("click", json!({"element": id})), want, "click {id}");
    }
    for (args, want) in [
        (json!({"text": "hi"}), RiskClass::Edit),
        (json!({"text": "hi", "element": "ebody"}), RiskClass::Edit),
        (json!({"text": "x", "element": "epw"}), RiskClass::Security),
        (json!({"text": "x", "element": "e404"}), RiskClass::Edit),
    ] {
        assert_eq!(risk("type_text", args.clone()), want, "{args}");
    }
    assert_eq!(
        risk("set_value", json!({"element": "eprio", "value": "High"})),
        RiskClass::Edit
    );

    // Focus is the To field (single line).
    for (k, el, want) in [
        ("enter", None, RiskClass::Send),
        ("enter", Some("ebody"), RiskClass::Edit),
        ("ctrl+enter", Some("ebody"), RiskClass::Send),
        ("cmd+enter", Some("ebody"), RiskClass::Send),
        ("shift+delete", None, RiskClass::Destroy),
        ("ctrl+a", None, RiskClass::Edit),
        ("backspace", None, RiskClass::Edit),
        ("delete", Some("em1"), RiskClass::Destroy),
        ("space", Some("esend"), RiskClass::Send),
        // An unknown element: Edit, refused by `call`.
        ("enter", Some("e404"), RiskClass::Edit),
    ] {
        let mut a = json!({"keys": k});
        if let Some(e) = el {
            a["element"] = json!(e);
        }
        assert_eq!(risk("press_key", a), want, "{k} {el:?}");
    }
    let r = kit
        .call("press_key", json!({"keys": "enter", "element": "e404"}))
        .await;
    assert!(r.is_error);
    assert!(kit.cu.acts().is_empty());

    for (tool, args, want) in [
        (
            "scroll",
            json!({"element": "elist", "direction": "down"}),
            RiskClass::Observe,
        ),
        ("wait_for", json!({"name": "x"}), RiskClass::Observe),
        ("observe", json!({}), RiskClass::Observe),
        ("open_app", json!({"name": "Notes"}), RiskClass::Edit),
        (
            "open_app",
            json!({"name": "Passwords"}),
            RiskClass::Security,
        ),
    ] {
        assert_eq!(risk(tool, args), want, "{tool}");
    }
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
    let kids = vec![
        n("es", Role::TextArea, "input"),
        n("ert", Role::Unknown, "").raw("terminal"),
    ];
    let mut odd = window("w9", "Shelly", None, "shelly", kids);
    odd.focused = Some(nugget_a11y::ElementId("es".into()));
    let plain = Kit::new(vec![odd.clone()]);
    plain.look().await;
    assert_eq!(
        plain.tool("type_text").risk(&json!({"text": "x"})),
        RiskClass::Edit
    );
    let keys = json!({"keys": "a", "element": "ert"});
    assert_eq!(plain.tool("press_key").risk(&keys), RiskClass::Security);
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
    let describe = |tool: &str, args: Value| kit.tool(tool).describe(&args);
    let d = describe("click", json!({"element": "esend"}));
    assert_eq!(
        (d.target_app.as_str(), d.verb.as_str()),
        ("com.apple.mail", "click")
    );
    assert_eq!(d.target_label.as_deref(), Some("Send"));
    // No label made from the model's own id (GG3-13).
    assert_eq!(
        describe("click", json!({"element": "e404"})).target_label,
        None
    );

    let d = describe(
        "type_text",
        json!({"text": "x".repeat(500), "element": "esubject"}),
    );
    assert_eq!(d.target_label.as_deref(), Some("Subject"));
    assert_eq!(d.text_preview.as_deref().unwrap().chars().count(), 121);
    let d = describe("type_text", json!({"text": "hello"}));
    assert_eq!(d.target_label.as_deref(), Some("To"));
    assert_eq!(d.text_preview.as_deref(), Some("hello"));

    let secret = json!({"text": "hunter2", "value": "hunter2", "element": "epw"});
    let d = describe("type_text", secret.clone());
    assert!(!d.text_preview.unwrap().contains("hunter2"));
    assert_eq!(kit.tool("type_text").redact(&secret)["text"], "[redacted]");
    assert_eq!(kit.tool("set_value").redact(&secret)["value"], "[redacted]");
    let plain = kit
        .tool("type_text")
        .redact(&json!({"text": "hello", "element": "eto"}));
    assert_eq!(plain["text"], "hello");

    // Cmd+Enter in a compose field sends it: the keys, then what they send (GG3-5).
    let d = describe("press_key", json!({"keys": "cmd+Enter"}));
    let p = d.text_preview.as_deref().unwrap();
    assert_eq!(p.lines().next(), Some("meta+enter"), "{p}");
    assert!(p.contains("To: "), "{p}");
    let d = describe("open_app", json!({"name": "Notes"}));
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
    assert!(!r.is_error && r.untrusted, "{}", r.content);
    assert!(
        matches!(v, Verification::Passed(ref d) if d.contains("window closed")),
        "{v:?}"
    );
    assert!(kit.cu.acts().contains(&"press:esend".to_string()));
}

#[tokio::test]
async fn click_verification_needs_backend_and_observation() {
    let bold = || json!({"element": "ebold"});
    // Nothing happens and the backend agrees: failed.
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    failed(&kit.act("click", bold()).await.1);

    // The backend claims success but nothing visible changed: unverifiable, never passed.
    kit.cu.with(|w| w.force_verified = Some(true));
    assert_eq!(kit.act("click", bold()).await.1, Verification::Unverifiable);

    // The window changed but the backend did not see it: unverifiable.
    kit.cu.with(|w| w.force_verified = Some(false));
    kit.cu.on("ebold", |w| {
        w.node_mut("ebody").unwrap().value = Some("**".into())
    });
    assert_eq!(kit.act("click", bold()).await.1, Verification::Unverifiable);

    // Both see it: passed.
    kit.cu.with(|w| w.force_verified = None);
    kit.cu.on("ebold", |w| {
        w.node_mut("ebody").unwrap().value = Some("****".into())
    });
    passed(&kit.act("click", bold()).await.1);

    // A stale id fails without acting on anything else.
    let (r, v) = kit.act("click", json!({"element": "e404"})).await;
    assert!(r.is_error);
    failed(&v);
}

#[tokio::test]
async fn click_selects_list_items_and_refuses_disabled() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let (r, v) = kit.act("click", json!({"element": "em2"})).await;
    assert_eq!(r.data.as_ref().unwrap()["action"], "select");
    passed(&v);
    assert!(kit.cu.with(|w| w.node("em2").unwrap().states.selected));

    kit.cu
        .with(|w| w.node_mut("ebold").unwrap().states.disabled = true);
    kit.look().await;
    refused(
        &kit.call("click", json!({"element": "ebold"})).await,
        "disabled",
    );
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
    passed(&v);
    let to = kit.cu.with(|w| w.node("eto").unwrap().value);
    assert_eq!(to.as_deref(), Some("bob@example.org"));

    // The app swallows the text but the backend claims success: failed (contradiction).
    kit.cu.with(|w| {
        w.drop_typing = true;
        w.force_verified = Some(true);
    });
    let args = json!({"text": "Hello", "element": "esubject"});
    failed(&kit.act("type_text", args).await.1);
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
        refused(&r, "secure");
        assert!(!r.content.contains("hunter2"));
    }
    // Focus on the password field, no element given.
    kit.cu
        .with(|w| w.windows[0].focused = Some(nugget_a11y::ElementId("epw".into())));
    kit.look().await;
    assert!(
        kit.call("type_text", json!({"text": "hunter2"}))
            .await
            .is_error
    );
    assert!(kit.cu.acts().iter().all(|a| !a.contains("epw")));
}

#[tokio::test]
async fn set_value_on_fields_and_choices() {
    let kit = Kit::new(vec![mail()]);
    kit.look().await;
    let set = |el: &str, value: &str| json!({"element": el, "value": value});
    passed(&kit.act("set_value", set("esubject", "Hi")).await.1);

    let (r, v) = kit.act("set_value", set("eprio", "high")).await;
    let data = r.data.as_ref().unwrap();
    assert_eq!(
        (&data["method"], &data["option"]),
        (&json!("select"), &json!("ehigh"))
    );
    passed(&v);
    let prio = kit.cu.with(|w| w.node("eprio").unwrap().value);
    assert_eq!(prio.as_deref(), Some("High"));

    // An option missing from the closed combo box appears once expanded.
    kit.cu.on("expand:eprio", |w| {
        let urgent = n("eurgent", Role::ListItem, "Urgent");
        w.node_mut("eprio").unwrap().children.push(urgent);
    });
    passed(&kit.act("set_value", set("eprio", "Urgent")).await.1);
    assert!(kit.cu.acts().contains(&"expand:eprio".to_string()));
    refused(
        &kit.call("set_value", set("eprio", "Nope")).await,
        "no option",
    );

    // The app writes something else: failed.
    kit.cu.with(|w| {
        w.wrong_value = Some("Hx".into());
        w.force_verified = Some(true);
    });
    let (_, v) = kit.act("set_value", set("esubject", "Hello")).await;
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
    refused(&r, "Nothing was pressed");
    assert!(
        kit.call("press_key", json!({"keys": "hello world"}))
            .await
            .is_error
    );
    assert!(kit.cu.acts().is_empty());

    kit.cu.on("key:enter", |w| w.close("w1"));
    let (r, v) = kit.act("press_key", json!({"keys": "enter"})).await;
    assert!(!r.is_error);
    assert_eq!(r.data.as_ref().unwrap()["keys"], json!(["enter"]));
    passed(&v);
}

#[tokio::test]
async fn scroll_and_open_app() {
    let kit = Kit::new(vec![mail(), terminal()]);
    kit.look().await;
    let args = json!({"element": "elist", "direction": "down", "amount": 500});
    assert!(!kit.call("scroll", args).await.is_error);
    assert!(kit.cu.acts().contains(&"scroll:elist".to_string()));

    let (r, v) = kit.act("open_app", json!({"name": "Terminal"})).await;
    assert!(!r.is_error);
    passed(&v);
    failed(&kit.act("open_app", json!({"name": "Nonexistent"})).await.1);
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
                let done = n("edone", Role::StaticText, "Message sent");
                w.windows[0].root.children.push(done);
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

    let gone = json!({"name": "Delete draft", "until": "disappears", "timeout_ms": 50});
    refused(&kit.call("wait_for", gone).await, "timed out after 50 ms");

    kit.cu.on("esend", |w| w.close("w1"));
    kit.look().await;
    kit.call("click", json!({"element": "esend"})).await;
    let gone = json!({"name": "Send", "until": "disappears", "window": "w1"});
    let r = kit.call("wait_for", gone).await;
    assert!(!r.is_error, "{}", r.content);

    assert!(kit.call("wait_for", json!({})).await.is_error);
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
    let args = json!({"name": "never", "timeout_ms": 3_600_000});
    refused(&kit.call("wait_for", args).await, "100 ms");

    let kit = Kit::new(vec![mail()]);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHORT).await;
        c2.cancel();
    });
    let r = kit
        .tool("wait_for")
        .call(json!({"name": "never"}), &ctx_with(cancel))
        .await;
    refused(&r, "cancelled");
}

#[tokio::test]
async fn screenshot_fallback_is_gated() {
    let empty = || {
        let kids = vec![n("", Role::Group, "").kids(vec![n("eimg", Role::Image, "")])];
        window("w5", "Canvas", None, "Drawing", kids)
    };
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
    let on = ComputerConfig {
        screenshot_fallback: true,
        ..ComputerConfig::default()
    };
    let look = async |tree, cfg| {
        let kit = Kit::with_config(vec![tree], cfg);
        kit.cu.with(|w| w.screenshot = Some(png.clone()));
        kit.look().await
    };

    // Off by default.
    let r = look(empty(), ComputerConfig::default()).await;
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());
    assert!(r.content.contains("(no accessible elements)"));

    let r = look(empty(), on.clone()).await;
    let shot = &r.data.as_ref().unwrap()["screenshot"];
    assert_eq!(shot["mime"], "image/png");
    assert_eq!(shot["base64"], "iVBORwECAw==");
    assert!(r.content.contains("screenshot") && !r.content.contains("iVBOR"));

    // A tree with something to act on never falls back.
    let r = look(mail(), on).await;
    assert!(r.data.as_ref().unwrap().get("screenshot").is_none());
}
