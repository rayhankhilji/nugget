//! Replies for a tool used on an element of the wrong kind: a click on a text field focuses it,
//! a click on a drop-down names `set_value` and its options, and `set_value` or `type_text` on a
//! button names `click`. Each refusal does nothing, so it is never riskier than the call the
//! model made; `risk` and `describe` do not look at any of this.
//!
//! Screen text in a reply (element lines, option names) is escaped and cut like every other
//! piece of screen text, and the reply is marked untrusted.

use nugget_a11y::role::Role;
use nugget_a11y::{ElementId, UiActionKind, UiNode};
use nugget_agent::ToolResult;

use crate::render::{is_text_input, safe_id, screen_text};

/// Most option names listed in a hint.
const OPTIONS_SHOWN: usize = 20;
/// Longest option name in a hint, in characters.
const OPTION_NAME_MAX: usize = 40;

/// What `click` does on an element of the cached snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClickPlan {
    Press,
    Select,
    /// An editable text input without a press action: give it keyboard focus.
    Focus,
    /// A drop-down: refuse and name `set_value` with its options.
    PickOption,
}

pub(super) fn click_plan(node: &UiNode) -> ClickPlan {
    let has = |k: UiActionKind| node.actions.contains(&k);
    // A drop-down whose options are known gets the hint even when it can be pressed: pressing
    // only opens it, and picking from it takes another turn per step.
    if is_option_picker(node) && (!has(UiActionKind::Press) || !option_names(node).is_empty()) {
        return ClickPlan::PickOption;
    }
    if has(UiActionKind::Press) {
        ClickPlan::Press
    } else if is_text_input(node) {
        ClickPlan::Focus
    } else if has(UiActionKind::Select) {
        ClickPlan::Select
    } else {
        ClickPlan::Press
    }
}

/// A drop-down whose value comes from a list of options (not one that takes typed text).
fn is_option_picker(node: &UiNode) -> bool {
    node.role == Role::ComboBox
        && !(node.states.editable && node.actions.contains(&UiActionKind::SetValue))
}

/// A control that is pressed, never given text.
fn is_pressable_control(node: &UiNode) -> bool {
    matches!(
        node.role,
        Role::Button | Role::Link | Role::CheckBox | Role::MenuItem | Role::Tab
    ) && !node.states.editable
        && !node.actions.contains(&UiActionKind::SetValue)
}

/// The names of the options under a drop-down in the snapshot, escaped and quoted.
fn option_names(node: &UiNode) -> Vec<String> {
    node.iter()
        .skip(1)
        .filter(|n| {
            matches!(
                n.role,
                Role::ListItem | Role::MenuItem | Role::Cell | Role::Row | Role::Tab
            ) || n.actions.contains(&UiActionKind::Select)
        })
        .filter_map(|n| n.name.as_deref().map(str::trim))
        .filter(|n| !n.is_empty())
        .take(OPTIONS_SHOWN)
        .map(|n| format!("\"{}\"", screen_text(n, OPTION_NAME_MAX)))
        .collect()
}

/// An error reply that quotes screen text: untrusted, and nothing was done.
fn refusal(content: String) -> ToolResult {
    let mut r = ToolResult::error(content);
    r.untrusted = true;
    r
}

/// The `set_value` call that picks an option of `node`, and the options known.
fn pick_with_set_value(node: &UiNode) -> String {
    let id = safe_id(node.id.as_str());
    let names = option_names(node);
    let options = if names.is_empty() {
        "its options are not in the snapshot; set_value opens it and picks the option by name"
            .to_string()
    } else {
        format!("options: {}", names.join(", "))
    };
    format!("use set_value with {{\"element\": \"{id}\", \"value\": \"<option>\"}}; {options}")
}

/// `click` on a drop-down.
pub(super) fn click_on_picker(node: &UiNode, desc: &str) -> ToolResult {
    refusal(format!(
        "{desc} is a {}: a click does not pick an option, so nothing was done. {}",
        node.role.as_str(),
        pick_with_set_value(node)
    ))
}

/// The line added to a click that focused a text input.
pub(super) fn after_focus(id: &ElementId) -> String {
    format!(
        "; now use set_value or type_text with {{\"element\": \"{}\"}}",
        safe_id(id.as_str())
    )
}

/// `set_value` on an element that has no value to set, if it is one.
pub(super) fn set_value_misuse(node: &UiNode, desc: &str) -> Option<ToolResult> {
    is_pressable_control(node).then(|| {
        refusal(format!(
            "{desc} is a {} and has no value to set, so nothing was done: use click with \
             {{\"element\": \"{}\"}}",
            node.role.as_str(),
            safe_id(node.id.as_str())
        ))
    })
}

/// `type_text` into an element that takes no typed text, if it is one.
pub(super) fn type_text_misuse(node: &UiNode, desc: &str) -> Option<ToolResult> {
    if is_option_picker(node) {
        return Some(refusal(format!(
            "{desc} is a {} and takes no typed text, so nothing was done. {}",
            node.role.as_str(),
            pick_with_set_value(node)
        )));
    }
    is_pressable_control(node).then(|| {
        refusal(format!(
            "{desc} is a {} and takes no typed text, so nothing was typed. type_text needs a \
             text field's id; to press this one, use click with {{\"element\": \"{}\"}}",
            node.role.as_str(),
            safe_id(node.id.as_str())
        ))
    })
}
