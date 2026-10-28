//! Before/after observation of one action ([`Obs`]) and the checks [`Tool::verify`] makes on it.
//!
//! [`Tool::verify`]: nugget_agent::Tool::verify

use std::sync::Arc;

use nugget_a11y::role::Role;
use nugget_a11y::{ElementId, UiNode, UiTree, WindowSel};
use nugget_agent::{Snapshot, ToolResult, Verification};
use nugget_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{Shared, blocking};

/// Longest value kept in an observation (the checks themselves use the full value).
const OBS_VALUE_MAX: usize = 500;

/// What an observation is about.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    Element(ElementId),
    /// Whatever has keyboard focus in the front window.
    Focused,
    /// The frontmost app, compared with this name.
    App(String),
}

/// What to check on the target's value.
#[derive(Debug, Clone, Default)]
pub(crate) struct Probe {
    /// The value contains this text (typed text), ignoring line breaks.
    pub contains: Option<String>,
    /// The value equals this (set_value); case-insensitive for choice controls. Also looks for
    /// an option with this name.
    pub equals: Option<String>,
}

/// The observed state around one action. Serialized as the tool's [`Snapshot`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Obs {
    pub window: Option<String>,
    /// The window no longer exists.
    pub window_gone: bool,
    /// Hash over every element of the window snapshot.
    pub fingerprint: Option<String>,
    pub nodes: usize,
    pub focused: Option<String>,
    /// The target element, shallow (no children, value cut).
    pub element: Option<UiNode>,
    /// Hash over the element's name, value, states and actions.
    pub element_sig: Option<String>,
    /// Hash of the element's full value.
    pub value_sig: Option<String>,
    /// The probe's verdict on the value; `None` when the value is not readable.
    pub value_matches: Option<bool>,
    /// For `equals` probes: the option with that name, shallow.
    pub option: Option<UiNode>,
    pub frontmost_app: Option<String>,
    /// The observation itself failed.
    pub error: Option<String>,
}

/// FNV-1a, stable across runs (unlike `DefaultHasher`).
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn add(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        // Field separator.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
    }
    fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

fn node_sig(n: &UiNode, h: &mut Fnv) {
    h.add(n.id.as_str().as_bytes());
    h.add(n.role.as_str().as_bytes());
    h.add(n.name.as_deref().unwrap_or_default().as_bytes());
    h.add(n.value.as_deref().unwrap_or_default().as_bytes());
    h.add(format!("{:?}{:?}", n.states, n.actions).as_bytes());
}

fn fingerprint(tree: &UiTree) -> String {
    let mut h = Fnv::new();
    for n in tree.root.iter() {
        node_sig(n, &mut h);
        h.add(&n.children.len().to_le_bytes());
    }
    h.hex()
}

fn shallow(n: &UiNode) -> UiNode {
    let mut s = n.clone();
    s.children.clear();
    if let Some(v) = &mut s.value {
        *v = nugget_agent::tool::truncate(v, OBS_VALUE_MAX);
    }
    s
}

pub(crate) fn is_choice(n: &UiNode) -> bool {
    matches!(n.role, Role::ComboBox | Role::List | Role::Menu)
}

fn strip_breaks(s: &str) -> String {
    s.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

/// An option named `value` under `root` (not `root` itself).
pub(crate) fn find_option<'a>(root: &'a UiNode, value: &str) -> Option<&'a UiNode> {
    let want = value.trim();
    let candidates = || {
        root.iter().skip(1).filter(|n| {
            matches!(
                n.role,
                Role::ListItem | Role::MenuItem | Role::Cell | Role::Row | Role::Tab
            ) || n
                .actions
                .contains(&nugget_a11y::UiActionKind::Select)
        })
    };
    let name = |n: &&UiNode| n.name.as_deref().map(str::trim).unwrap_or_default().to_string();
    candidates()
        .find(|n| name(n) == want)
        .or_else(|| candidates().find(|n| name(n).eq_ignore_ascii_case(want)))
}

/// Observe `target` in a fresh snapshot of its window (blocking).
fn observe_blocking(s: &Shared, target: &Target, probe: &Probe) -> Obs {
    let mut obs = Obs::default();
    if let Target::App(_) = target {
        match s.cu.windows() {
            Ok(ws) => {
                obs.frontmost_app = ws
                    .iter()
                    .find(|w| w.focused)
                    .or_else(|| ws.first())
                    .map(|w| match &w.app.id {
                        Some(id) => format!("{} ({id})", w.app.name),
                        None => w.app.name.clone(),
                    });
            }
            Err(e) => obs.error = Some(e.to_string()),
        }
        return obs;
    }
    let window = match target {
        Target::Element(id) => s.find(id).map(|f| f.window.id),
        _ => None,
    };
    let sel = match (&window, target) {
        (Some(w), _) => WindowSel::Id(w.clone()),
        (None, _) => WindowSel::Focused,
    };
    let tree = match s.snapshot(&sel) {
        Ok(t) => t,
        Err(Error::NotFound(_)) if window.is_some() => {
            obs.window = window.map(|w| w.0);
            obs.window_gone = true;
            if let Some(w) = &obs.window {
                s.forget(&nugget_a11y::WindowId(w.clone()));
            }
            return obs;
        }
        Err(e) => {
            obs.error = Some(e.to_string());
            return obs;
        }
    };
    obs.window = Some(tree.window.id.0.clone());
    obs.fingerprint = Some(fingerprint(&tree));
    obs.nodes = tree.count();
    obs.focused = tree.focused.as_ref().map(|f| f.0.clone());
    let id = match target {
        Target::Element(id) => Some(id.clone()),
        _ => tree.focused.clone(),
    };
    let Some(node) = id.as_ref().and_then(|id| tree.find(id)) else {
        return obs;
    };
    let mut h = Fnv::new();
    node_sig(node, &mut h);
    obs.element_sig = Some(h.hex());
    let value = (!node.states.secure).then_some(node.value.as_deref()).flatten();
    if let Some(v) = value {
        let mut vh = Fnv::new();
        vh.add(v.as_bytes());
        obs.value_sig = Some(vh.hex());
    }
    if let Some(want) = &probe.contains {
        obs.value_matches = value.map(|v| strip_breaks(v).contains(&strip_breaks(want)));
    }
    if let Some(want) = &probe.equals {
        obs.value_matches = value.map(|v| {
            v.trim() == want.trim() || (is_choice(node) && v.trim().eq_ignore_ascii_case(want.trim()))
        });
        obs.option = find_option(node, want).map(shallow);
    }
    obs.element = Some(shallow(node));
    obs
}

pub(crate) async fn observe(shared: &Arc<Shared>, target: Target, probe: Probe) -> Snapshot {
    let obs = blocking(shared, move |s| Ok(observe_blocking(s, &target, &probe)))
        .await
        .unwrap_or_else(|e| Obs {
            error: Some(e.to_string()),
            ..Obs::default()
        });
    serde_json::to_value(obs).ok()
}

pub(crate) fn parse(s: &Snapshot) -> Option<Obs> {
    serde_json::from_value(s.clone()?).ok()
}

/// What the backend said after re-reading: `None` when the call did not reach it.
pub(crate) fn backend_verified(result: &ToolResult) -> Option<bool> {
    result.data.as_ref()?.get("outcome")?.get("verified")?.as_bool()
}

pub(crate) fn result_str<'a>(result: &'a ToolResult, key: &str) -> Option<&'a str> {
    result.data.as_ref()?.get(key).and_then(Value::as_str)
}

/// Did anything visibly change between the observations?
pub(crate) fn visible_change(b: &Obs, a: &Obs) -> Option<&'static str> {
    if a.window_gone && !b.window_gone {
        Some("the window closed")
    } else if b.element.is_some() && a.element.is_none() && !a.window_gone {
        Some("the element went away")
    } else if b.element_sig.is_some() && a.element_sig.is_some() && b.element_sig != a.element_sig
    {
        Some("the element changed")
    } else if b.focused != a.focused && a.fingerprint.is_some() {
        Some("focus moved")
    } else if b.fingerprint.is_some() && a.fingerprint.is_some() && b.fingerprint != a.fingerprint
    {
        Some("the window changed")
    } else {
        None
    }
}

/// The common preamble of every check: the call failed, or the observations are missing.
pub(crate) fn precheck(
    before: &Snapshot,
    after: &Snapshot,
    result: &ToolResult,
) -> Result<(Obs, Obs, bool), Verification> {
    if result.is_error {
        return Err(Verification::Failed(format!(
            "the action did not complete: {}",
            nugget_agent::tool::truncate(&result.content, 200)
        )));
    }
    let Some(verified) = backend_verified(result) else {
        return Err(Verification::Unverifiable);
    };
    let (Some(b), Some(a)) = (parse(before), parse(after)) else {
        return Err(Verification::Unverifiable);
    };
    if b.error.is_some() || a.error.is_some() {
        return Err(Verification::Unverifiable);
    }
    Ok((b, a, verified))
}

/// Press-like actions (click, keys): passed when the backend saw the effect and so do we; failed
/// when neither did.
pub(crate) fn verify_change(b: &Obs, a: &Obs, verified: bool, what: &str) -> Verification {
    match (visible_change(b, a), verified) {
        (Some(change), true) => Verification::Passed(format!("{what}: {change}")),
        (None, false) => Verification::Failed(format!("{what}: no visible effect")),
        _ => Verification::Unverifiable,
    }
}
