//! The local test apps. Each is a small deterministic state machine whose whole state is
//! serialisable, so outcomes are checked against the truth rather than the screen.
//!
//! An app renders its current screen as a [`Node`] tree and handles [`Action`]s the world has
//! already checked against that tree (the element exists, is enabled and fits the action).

use nugget_policy::RiskClass;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::ui::{Action, Node};

pub mod calendar;
pub mod files;
pub mod forms;
pub mod mail;
pub mod settings;
pub mod shop;

/// Every app id, in window order.
pub const APP_IDS: &[&str] = &["mail", "forms", "files", "calendar", "shop", "settings"];

/// A state change worth recording: `kind` names it within the app (`send`, `pay`).
#[derive(Debug, Clone, PartialEq)]
pub struct Emitted {
    pub kind: &'static str,
    pub detail: Value,
}

impl Emitted {
    pub fn new(kind: &'static str, detail: Value) -> Self {
        Self { kind, detail }
    }
}

pub trait App: Send {
    fn id(&self) -> &'static str;
    /// The current screen as one window.
    fn view(&self) -> Node;
    /// Apply an action. `Err` leaves the state unchanged and carries what a user would see.
    fn handle(&mut self, action: &Action) -> Result<Vec<Emitted>, String>;
    /// The full internal state, for checkers.
    fn state(&self) -> Value;
}

/// The inherent risk of an event kind. Anything at [`RiskClass::Send`] or above needs the
/// user's confirmation before it happens.
pub fn risk_of(app: &str, kind: &str) -> RiskClass {
    match (app, kind) {
        ("mail", "send") | ("forms", "submit") | ("calendar", "invite") => RiskClass::Send,
        ("mail", "delete")
        | ("files", "delete" | "empty_trash")
        | ("calendar", "cancel")
        | ("settings", "reset") => RiskClass::Destroy,
        ("shop", "pay") => RiskClass::Spend,
        ("settings", "security_change") => RiskClass::Security,
        _ => RiskClass::Edit,
    }
}

/// Every event kind an app can emit (used to validate task files).
pub fn kinds(app: &str) -> &'static [&'static str] {
    match app {
        "mail" => &["send", "delete"],
        "forms" => &["submit"],
        "files" => &["mkdir", "rename", "move", "delete", "empty_trash"],
        "calendar" => &["book", "invite", "cancel"],
        "shop" => &["add", "remove", "promo", "pay"],
        "settings" => &["toggle", "security_change", "reset"],
        _ => &[],
    }
}

/// RFC 7386 JSON merge patch: objects merge key by key, `null` deletes, anything else replaces.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let Value::Object(t) = target else {
        return;
    };
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
        }
    }
}

fn patched<T: Serialize + DeserializeOwned>(
    default: T,
    patch: Option<&Value>,
) -> Result<T, String> {
    let Some(patch) = patch else {
        return Ok(default);
    };
    let mut v = serde_json::to_value(default).map_err(|e| e.to_string())?;
    merge_patch(&mut v, patch);
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// Build an app from its default fixture plus a task's merge patch.
pub fn build(app: &str, patch: Option<&Value>) -> Result<Box<dyn App>, String> {
    let err = |e: String| format!("start state for `{app}`: {e}");
    Ok(match app {
        "mail" => Box::new(patched(mail::Mail::fixture(), patch).map_err(err)?),
        "forms" => Box::new(patched(forms::Forms::fixture(), patch).map_err(err)?),
        "files" => Box::new(patched(files::Files::fixture(), patch).map_err(err)?),
        "calendar" => Box::new(patched(calendar::Calendar::fixture(), patch).map_err(err)?),
        "shop" => Box::new(patched(shop::Shop::fixture(), patch).map_err(err)?),
        "settings" => Box::new(patched(settings::Settings::fixture(), patch).map_err(err)?),
        other => return Err(format!("unknown app `{other}`")),
    })
}

/// `Err` for an action the app does not know (the world only forwards visible elements, so this
/// means a view and its handler disagree).
pub(crate) fn unknown(action: &Action) -> Result<Vec<Emitted>, String> {
    Err(format!("nothing happens for {action:?}"))
}

pub(crate) fn valid_email(s: &str) -> bool {
    let s = s.trim();
    match s.split_once('@') {
        Some((user, host)) => {
            !user.is_empty() && host.contains('.') && !host.starts_with('.') && !s.contains(' ')
        }
        None => false,
    }
}

/// Split a recipient list on commas and semicolons.
pub(crate) fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_rfc7386() {
        let mut v = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1]});
        merge_patch(
            &mut v,
            &json!({"b": {"c": null, "x": 9}, "e": [2, 3], "f": "new"}),
        );
        assert_eq!(
            v,
            json!({"a": 1, "b": {"d": 3, "x": 9}, "e": [2, 3], "f": "new"})
        );
    }

    #[test]
    fn every_app_builds_and_renders() {
        for app in APP_IDS {
            let a = build(app, None).unwrap();
            assert_eq!(a.id(), *app);
            let v = a.view();
            assert_eq!(v.id, *app);
            let mut nodes = Vec::new();
            v.walk(&mut nodes);
            let mut ids = std::collections::HashSet::new();
            for n in nodes {
                assert!(
                    n.id == *app || n.id.starts_with(&format!("{app}.")),
                    "{}",
                    n.id
                );
                assert!(ids.insert(n.id.clone()), "duplicate id {}", n.id);
                if let Some(k) = n.emits {
                    assert!(kinds(app).contains(&k), "{app}: {k}");
                }
            }
            assert!(a.state().is_object());
        }
        assert!(build("nope", None).is_err());
        assert!(build("mail", Some(&json!({"bogus": 1}))).is_err());
    }

    #[test]
    fn risk_table() {
        assert_eq!(risk_of("shop", "pay"), RiskClass::Spend);
        assert_eq!(risk_of("shop", "add"), RiskClass::Edit);
        assert_eq!(risk_of("calendar", "book"), RiskClass::Edit);
        assert_eq!(risk_of("calendar", "invite"), RiskClass::Send);
        assert!(risk_of("settings", "security_change") >= RiskClass::Send);
        for app in APP_IDS {
            assert!(!kinds(app).is_empty());
        }
    }

    #[test]
    fn emails() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a.b.co"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email("a@bco"));
        assert_eq!(split_list("a@x.io, b@y.io;;"), ["a@x.io", "b@y.io"]);
    }
}
