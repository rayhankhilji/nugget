//! What the model reads of a window: a compacted copy of the snapshot, rendered by
//! [`crate::render::render`]. Only the copy is changed; the cached snapshot that `risk`,
//! `describe` and verification use stays exactly as the backend returned it, and element ids are
//! never rewritten.
//!
//! The compaction only drops what the model would read twice, never text it would otherwise see
//! once, so the injection detector and the untrusted fence see the same text the model does:
//!
//! - A list item, row or cell that cannot be acted on and whose label is repeated by an element
//!   inside it (a message row around its "Open" button) loses its own line; its children move up.
//! - A combo box whose description already lists every option by name ("options: A | B | C")
//!   drops the option lines; `set_value` picks an option by name, and the option ids stay valid.
//! - A combo box's "collapsed" state is left out (`set_value` expands it when needed).
//! - Labels of controls (buttons, links, tabs, …) are cut shorter than body text.

use nugget_a11y::role::Role;
use nugget_a11y::{ElementId, UiNode, UiTree};

use crate::render::RenderOptions;

/// Longest label of a control (button, link, tab, …), in characters. Body text keeps the
/// configured `max_text`.
pub(crate) const CONTROL_LABEL_MAX: usize = 80;
/// The rendering caps a description at this many characters (see `render::node_line`).
const DESC_SHOWN_MAX: usize = 80;

/// The copy of `tree` the model reads.
pub(crate) fn compact(tree: &UiTree, opts: &RenderOptions) -> UiTree {
    let mut view = tree.clone();
    let focused = tree.focused.clone();
    let desc_max = opts.max_text.min(DESC_SHOWN_MAX);
    for c in &mut view.root.children {
        compact_node(c, focused.as_ref(), desc_max);
    }
    view
}

fn compact_node(node: &mut UiNode, focused: Option<&ElementId>, desc_max: usize) {
    let is_focused = focused == Some(&node.id) || node.states.focused;
    if node.role == Role::ComboBox {
        if node.states.expanded == Some(false) {
            node.states.expanded = None;
        }
        if options_listed(node, focused, desc_max) {
            node.children.clear();
        }
    }
    if !is_focused && is_redundant_wrapper(node) {
        // An unnamed group with no actions is collapsed by the renderer.
        node.role = Role::Group;
        node.raw_role.clear();
        node.name = None;
        node.description = None;
    }
    if is_labelled_control(node.role) {
        cut(&mut node.name, CONTROL_LABEL_MAX);
    }
    for c in &mut node.children {
        compact_node(c, focused, desc_max);
    }
}

fn text(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Cut `s` to `max` characters, marking the cut.
fn cut(s: &mut Option<String>, max: usize) {
    if let Some(v) = s
        && v.chars().count() > max
    {
        let mut short: String = v.chars().take(max).collect();
        short.push('…');
        *v = short;
    }
}

/// Every child of the combo box is a plain option whose name its (fully shown) description
/// lists, and none of them carries state the combo box's own line does not show.
fn options_listed(node: &UiNode, focused: Option<&ElementId>, desc_max: usize) -> bool {
    let Some(desc) = text(&node.description) else {
        return false;
    };
    if node.children.is_empty() || desc.chars().count() > desc_max {
        return false;
    }
    let value = text(&node.value);
    node.children.iter().all(|c| {
        let Some(name) = text(&c.name) else {
            return false;
        };
        c.children.is_empty()
            && focused != Some(&c.id)
            && !c.states.focused
            && (!c.states.selected || value == Some(name))
            && text(&c.value).is_none_or(|v| v == name)
            && text(&c.description).is_none()
            && desc.contains(name)
    })
}

/// A list item, row or cell the model cannot act on, whose texts all appear inside an element
/// within it that is drawn.
fn is_redundant_wrapper(node: &UiNode) -> bool {
    if !matches!(node.role, Role::ListItem | Role::Row | Role::Cell)
        || !node.actions.is_empty()
        || node.states.selected
        || node.states.editable
        || node.states.offscreen
        || node.states.checked.is_some()
        || node.states.expanded.is_some()
        || text(&node.value).is_some()
        || text(&node.placeholder).is_some()
        || node.children.is_empty()
    {
        return false;
    }
    let mut inner = Vec::new();
    for c in &node.children {
        visible_texts(c, &mut inner);
    }
    let covered = |t: Option<&str>| {
        t.is_none_or(|t| {
            let t = t.to_lowercase();
            inner.iter().any(|i| i.contains(&t))
        })
    };
    covered(text(&node.name)) && covered(text(&node.description))
}

/// Lowercased names and values of `node` and its descendants that are not off screen.
fn visible_texts(node: &UiNode, out: &mut Vec<String>) {
    if node.states.offscreen {
        return;
    }
    out.extend(text(&node.name).map(str::to_lowercase));
    if !node.states.secure {
        out.extend(text(&node.value).map(str::to_lowercase));
    }
    for c in &node.children {
        visible_texts(c, out);
    }
}

fn is_labelled_control(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::Link
            | Role::MenuItem
            | Role::Tab
            | Role::CheckBox
            | Role::ComboBox
            | Role::ListItem
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::render;
    use nugget_a11y::UiActionKind;
    use nugget_a11y::tree::States;

    fn n(id: &str, role: Role, name: &str) -> UiNode {
        UiNode {
            id: ElementId(id.into()),
            role,
            name: (!name.is_empty()).then(|| name.to_string()),
            actions: if role == Role::Button {
                vec![UiActionKind::Press]
            } else {
                Vec::new()
            },
            ..UiNode::default()
        }
    }

    fn kids(mut node: UiNode, children: Vec<UiNode>) -> UiNode {
        node.children = children;
        node
    }

    fn tree(children: Vec<UiNode>) -> UiTree {
        crate::tools::tests::window("w", "App", children)
    }

    fn day_combo() -> UiNode {
        let mut combo = n("day", Role::ComboBox, "Day");
        combo.value = Some("Monday".into());
        combo.description = Some("options: Monday | Tuesday".into());
        combo.states.expanded = Some(false);
        let mut mon = n("day-opt0", Role::ListItem, "Monday");
        mon.states.selected = true;
        mon.actions = vec![UiActionKind::Select];
        let mut tue = n("day-opt1", Role::ListItem, "Tuesday");
        tue.actions = vec![UiActionKind::Select];
        kids(combo, vec![mon, tue])
    }

    #[test]
    fn wrapper_rows_collapse_when_their_label_is_repeated() {
        let t = tree(vec![kids(
            n("list", Role::List, "Messages"),
            vec![
                kids(
                    n("row1", Role::ListItem, "Lunch?"),
                    vec![n("open1", Role::Button, "Priya: Lunch?")],
                ),
                // Its label is not repeated inside: kept.
                kids(
                    n("row2", Role::ListItem, "Offsite"),
                    vec![n("open2", Role::Button, "Open")],
                ),
            ],
        )]);
        let opts = RenderOptions::default();
        let out = render(&compact(&t, &opts), &opts);
        assert!(!out.contains("[row1]"), "{out}");
        assert!(out.contains("  [open1] button \"Priya: Lunch?\""), "{out}");
        assert!(out.contains("[row2] list_item \"Offsite\""), "{out}");
        // The snapshot itself is untouched.
        assert!(t.find(&ElementId("row1".into())).unwrap().name.is_some());
    }

    #[test]
    fn rows_that_act_or_carry_state_are_kept() {
        let mut selectable = kids(
            n("row", Role::ListItem, "Lunch?"),
            vec![n("open", Role::Button, "Lunch?")],
        );
        selectable.actions = vec![UiActionKind::Select];
        let mut selected = selectable.clone();
        selected.id = ElementId("sel".into());
        selected.actions.clear();
        selected.states = States {
            selected: true,
            ..States::default()
        };
        let mut focus = selected.clone();
        focus.id = ElementId("foc".into());
        focus.states = States::default();
        let mut t = tree(vec![selectable, selected, focus]);
        t.focused = Some(ElementId("foc".into()));
        let opts = RenderOptions::default();
        let out = render(&compact(&t, &opts), &opts);
        for id in ["[row]", "[sel]", "[foc]"] {
            assert!(out.contains(id), "{id}: {out}");
        }
    }

    #[test]
    fn listed_combo_options_are_dropped() {
        let t = tree(vec![day_combo()]);
        let opts = RenderOptions::default();
        let out = render(&compact(&t, &opts), &opts);
        assert!(
            out.contains(
                "[day] combo_box \"Day\" value=\"Monday\" desc=\"options: Monday | Tuesday\"\n"
            ),
            "{out}"
        );
        assert!(!out.contains("day-opt"), "{out}");

        // Options the description does not list stay.
        let mut partial = day_combo();
        partial.description = Some("options: Monday".into());
        let out = render(&compact(&tree(vec![partial]), &opts), &opts);
        assert!(out.contains("[day-opt1] list_item \"Tuesday\""), "{out}");

        // A focused option stays, and so does an option list without a description.
        let mut t = tree(vec![day_combo()]);
        t.focused = Some(ElementId("day-opt1".into()));
        assert!(render(&compact(&t, &opts), &opts).contains("[day-opt1]"));
        let mut bare = day_combo();
        bare.description = None;
        assert!(render(&compact(&tree(vec![bare]), &opts), &opts).contains("[day-opt0]"));
    }

    #[test]
    fn control_labels_are_cut_and_body_text_is_not() {
        let long = "x".repeat(110);
        let t = tree(vec![
            n("b", Role::Button, &long),
            n("t", Role::StaticText, &long),
        ]);
        let opts = RenderOptions::default();
        let out = render(&compact(&t, &opts), &opts);
        assert!(
            out.contains(&format!("[b] button \"{}…\"", "x".repeat(80))),
            "{out}"
        );
        assert!(
            out.contains(&format!("[t] static_text \"{long}\"")),
            "{out}"
        );
    }
}
