//! Unit tests of the tools' internals: id normalisation, text chunking and observe hints.

use nugget_a11y::role::Role;
use nugget_a11y::{AppEntry, ElementId, UiNode, UiTree, WindowId, WindowInfo};
use nugget_core::Error;
use nugget_core::ipc::AppInfo;

use super::normalize_id;

pub(crate) fn window(id: &str, app: &str, children: Vec<UiNode>) -> UiTree {
    UiTree {
        app: AppInfo {
            id: None,
            name: app.into(),
            window_title: Some(format!("{app} window")),
            url: None,
        },
        window: WindowInfo {
            id: WindowId(id.into()),
            app: AppEntry {
                id: None,
                name: app.into(),
                pid: None,
                frontmost: false,
            },
            title: Some(format!("{app} window")),
            bounds: None,
            focused: false,
            minimized: false,
        },
        root: UiNode {
            id: ElementId(format!("{id}root")),
            role: Role::Window,
            children,
            ..UiNode::default()
        },
        focused: None,
        truncated: false,
    }
}

pub(crate) fn kids(mut node: UiNode, children: Vec<UiNode>) -> UiNode {
    node.children = children;
    node
}

/// How a model may write `mail-compose`.
const SPELLINGS: &[&str] = &[
    "mail-compose",
    "[ mail-compose ]",
    "\"mail-compose\"",
    "`mail-compose`",
    "[[mail-compose]]",
    "⟦mail-compose⟧",
    "[mail-compose] button \"Compose\"",
    "mail-compose\n",
];

#[test]
fn ids_are_normalised() {
    for s in SPELLINGS {
        assert_eq!(normalize_id(s), "mail-compose", "{s:?}");
    }
    // Nothing but the wrapping goes: inner text that is not an id stays and is rejected.
    assert_eq!(normalize_id("mail compose"), "mail compose");
    assert_eq!(normalize_id("Compose button"), "Compose button");
    assert_eq!(normalize_id("[]"), "");
    assert_eq!(normalize_id("\"\""), "");
}

// Review items 11 and 15: how text is cut into pieces.
#[test]
fn text_chunks_edge_cases() {
    use super::text_chunks;
    // CR LF stays whole and never starts a piece; no piece is only a line break.
    let t = format!("{}\r\nnext", "a".repeat(4));
    let c = text_chunks(&t, 4);
    assert_eq!(c.concat(), t);
    assert!(c.iter().all(|p| !p.starts_with(['\r', '\n'])), "{c:?}");
    assert_eq!(text_chunks("abcd\n", 4), ["abcd\n"]);
    // A combining mark stays with its letter; an emoji modifier with its emoji.
    assert_eq!(text_chunks("abce\u{301}fg", 4)[0], "abce\u{301}");
    assert_eq!(text_chunks("abc👍🏽x", 4)[0], "abc👍🏽");
    assert!(text_chunks("", 4).is_empty());
}

// GG3-24 (regression): on Linux a missing accessibility bus is named as such, not as a
// permission the user could grant.
#[test]
fn gg3_24_linux_bus_failure_is_not_a_permission() {
    use super::observe_hint;
    let bus = Error::Other(
        "AT-SPI: failed to connect to the accessibility bus: org.freedesktop.DBus.Error".into(),
    );
    let linux = observe_hint(&bus, true, true);
    assert!(
        linux.contains("Linux accessibility bus (AT-SPI)"),
        "{linux}"
    );
    assert!(!linux.contains("permission"), "{linux}");
    // Even when permission() cannot tell why, Denied on Linux means the bus.
    assert!(observe_hint(&Error::Other("x".into()), true, true).contains("AT-SPI"));
    assert!(observe_hint(&bus, false, true).contains("AT-SPI"));
    // Elsewhere, Denied is a permission.
    assert!(observe_hint(&bus, true, false).contains("permission is not granted"));
    assert!(observe_hint(&Error::NotFound("w".into()), false, true).contains("without arguments"));
    assert_eq!(observe_hint(&Error::Other("x".into()), false, true), "");
}
