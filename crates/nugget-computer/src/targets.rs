//! The items a screen shows that a task could refer to ([`ToolResult::targets`]): inbox rows,
//! files, calendar entries, contacts, products. The agent loop matches them against the goal to
//! catch an ambiguous target ("Reply to Sam" with mail from two Sams) and ask the user. They are
//! never sent to the model, and the rendering the model reads is not changed by them.
//!
//! A target is one selectable item of a collection:
//!
//! - a list item or table row (anywhere but inside a combo box, menu or toolbar),
//! - a cell outside any row (a grid of cells),
//! - a menu item or article inside a list or log (not a menu: those are commands),
//! - a button or link that stands for an item inside a list, table or log, not wrapped in an
//!   item, unless its label is a plain command ("Load more", "Back").
//!
//! Its label is the item's name joined with the visible text inside it (names and values, not
//! secure values or input fields), so a row reads as the user sees it ("Sam Lee: Can I take
//! Friday off?"). Texts already contained in a longer one are dropped. Controls inside the item
//! (its open button) count only when the item has no other text, and never when they are plain
//! commands. A row and its own button make one target; nested items (an expanded folder's
//! files) are targets of their own. Off-screen subtrees are skipped, as the rendering skips them.
//!
//! Labels are untrusted screen text, cleaned by [`screen_text`] and cut to [`LABEL_MAX`]
//! characters; at most [`TARGETS_MAX`] targets are kept, in screen order.
//!
//! [`ToolResult::targets`]: nugget_agent::ToolResult::targets

use nugget_a11y::role::Role;
use nugget_a11y::{UiNode, UiTree};

use crate::render::{is_text_input, screen_text};

/// Most targets per screen.
pub const TARGETS_MAX: usize = 60;
/// Longest label, in characters.
pub const LABEL_MAX: usize = 120;
/// Most text pieces joined into one label.
const PARTS_MAX: usize = 8;
/// Separator between the pieces of a label.
const JOIN: &str = " — ";

/// Labels that are commands, not items, compared case-insensitively on the whole label.
const COMMANDS: &[&str] = &[
    "open",
    "reply",
    "reply all",
    "forward",
    "send",
    "back",
    "compose",
    "new",
    "edit",
    "delete",
    "remove",
    "archive",
    "close",
    "cancel",
    "ok",
    "done",
    "save",
    "more",
    "show more",
    "load more",
    "see all",
    "view all",
    "next",
    "previous",
    "select",
    "share",
    "menu",
    "options",
    "more options",
];

/// The targets `tree` shows, in screen order.
pub fn targets(tree: &UiTree) -> Vec<String> {
    let mut out = Vec::new();
    let ctx = Ctx::default();
    for c in &tree.root.children {
        visit(c, ctx, &mut out);
        if out.len() >= TARGETS_MAX {
            break;
        }
    }
    out.truncate(TARGETS_MAX);
    out
}

/// Where a node sits.
#[derive(Debug, Clone, Copy, Default)]
struct Ctx {
    /// Inside a list, table or log.
    in_collection: bool,
    /// Inside a row.
    in_row: bool,
    /// Inside a target.
    in_item: bool,
}

fn visit(node: &UiNode, ctx: Ctx, out: &mut Vec<String>) {
    if out.len() >= TARGETS_MAX || node.states.offscreen || is_chrome(node.role) {
        return;
    }
    let mut ctx = ctx;
    if is_item(node, ctx) {
        let l = label(node);
        if !l.is_empty() {
            out.push(l);
        }
        ctx.in_item = true;
    }
    if is_collection(node.role) {
        ctx.in_collection = true;
    }
    if node.role == Role::Row {
        ctx.in_row = true;
    }
    for c in &node.children {
        visit(c, ctx, out);
    }
}

/// Subtrees that hold commands or form choices, not items.
fn is_chrome(role: Role) -> bool {
    matches!(role, Role::ComboBox | Role::Menu | Role::Toolbar)
}

fn is_collection(role: Role) -> bool {
    matches!(role, Role::List | Role::Table | Role::Log)
}

fn is_item(node: &UiNode, ctx: Ctx) -> bool {
    match node.role {
        Role::ListItem | Role::Row => true,
        Role::Cell => !ctx.in_row,
        Role::MenuItem | Role::Article => ctx.in_collection,
        Role::Button | Role::Link => {
            ctx.in_collection && !ctx.in_item && !is_command(text(&node.name).unwrap_or(""))
        }
        _ => false,
    }
}

fn is_control(role: Role) -> bool {
    matches!(
        role,
        Role::Button | Role::Link | Role::MenuItem | Role::Tab | Role::CheckBox
    )
}

fn is_command(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    COMMANDS.contains(&l.as_str())
}

fn text(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// The item's name with the visible text inside it. Empty when there is none.
fn label(node: &UiNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(text(&node.name).map(String::from));
    if !node.states.secure && !is_text_input(node) {
        parts.extend(text(&node.value).map(String::from));
    }
    let (mut texts, mut controls) = (Vec::new(), Vec::new());
    for c in &node.children {
        inner_texts(c, &mut texts, &mut controls);
    }
    parts.extend(texts);
    if parts.len() <= usize::from(text(&node.name).is_some()) {
        parts.extend(controls.into_iter().filter(|c| !is_command(c)));
    }
    let joined = distinct(parts).join(JOIN);
    screen_text(&joined, LABEL_MAX)
}

/// Visible names and values under an item: text into `texts`, control labels into `controls`.
/// Nested items and collections, input fields, chrome and off-screen parts are left out.
fn inner_texts(node: &UiNode, texts: &mut Vec<String>, controls: &mut Vec<String>) {
    if node.states.offscreen
        || is_chrome(node.role)
        || is_collection(node.role)
        || matches!(node.role, Role::ListItem | Role::Row)
        || is_text_input(node)
    {
        return;
    }
    let to = if is_control(node.role) {
        &mut *controls
    } else {
        &mut *texts
    };
    to.extend(text(&node.name).map(String::from));
    if !node.states.secure {
        to.extend(text(&node.value).map(String::from));
    }
    for c in &node.children {
        inner_texts(c, texts, controls);
    }
}

/// `parts` without the ones another part already contains (case-insensitively), in order,
/// at most [`PARTS_MAX`].
fn distinct(parts: Vec<String>) -> Vec<String> {
    let lower: Vec<String> = parts
        .iter()
        .map(|p| {
            p.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .collect();
    let mut keep = Vec::new();
    for (i, p) in parts.into_iter().enumerate() {
        let covered = lower.iter().enumerate().any(|(j, other)| {
            j != i && other.contains(&lower[i]) && (other.len() > lower[i].len() || j < i)
        });
        if !covered {
            keep.push(p);
        }
        if keep.len() >= PARTS_MAX {
            break;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use nugget_a11y::tree::States;
    use nugget_a11y::{AppEntry, ElementId, UiActionKind, WindowId, WindowInfo};
    use nugget_core::ipc::AppInfo;

    use super::*;

    fn n(id: &str, role: Role, name: &str) -> UiNode {
        let actions = match role {
            Role::Button | Role::Link | Role::MenuItem => vec![UiActionKind::Press],
            Role::ListItem => vec![UiActionKind::Select],
            _ => vec![],
        };
        UiNode {
            id: ElementId(id.into()),
            role,
            name: (!name.is_empty()).then(|| name.to_string()),
            actions,
            ..UiNode::default()
        }
    }

    fn kids(mut node: UiNode, children: Vec<UiNode>) -> UiNode {
        node.children = children;
        node
    }

    fn tree(children: Vec<UiNode>) -> UiTree {
        UiTree {
            app: AppInfo {
                id: None,
                name: "App".into(),
                window_title: Some("Window".into()),
                url: None,
            },
            window: WindowInfo {
                id: WindowId("w".into()),
                app: AppEntry {
                    id: None,
                    name: "App".into(),
                    pid: None,
                    frontmost: true,
                },
                title: Some("Window".into()),
                bounds: None,
                focused: true,
                minimized: false,
            },
            root: kids(n("root", Role::Window, "Window"), children),
            focused: None,
            truncated: false,
        }
    }

    /// A mail inbox as the bench app shows it: a row named by the subject around an open button
    /// that names the sender.
    fn inbox_row(id: &str, from: &str, subject: &str) -> UiNode {
        kids(
            n(&format!("row{id}"), Role::ListItem, subject),
            vec![n(
                &format!("open{id}"),
                Role::Button,
                &format!("{from}: {subject}"),
            )],
        )
    }

    #[test]
    fn inbox_with_two_sams() {
        let t = tree(vec![
            n("title", Role::Heading, "Inbox (3)"),
            n("compose", Role::Button, "Compose"),
            kids(
                n("list", Role::List, "Messages"),
                vec![
                    inbox_row("1", "Sam Lee", "Can I take Friday off?"),
                    inbox_row("2", "Sam Ortiz", "Invoice for October"),
                    inbox_row("3", "Dana Kim", "Lunch?"),
                ],
            ),
        ]);
        assert_eq!(
            targets(&t),
            [
                "Sam Lee: Can I take Friday off?",
                "Sam Ortiz: Invoice for October",
                "Dana Kim: Lunch?"
            ]
        );
    }

    #[test]
    fn rows_with_their_own_text() {
        // A row whose cells carry the sender, subject and date.
        let row = |id: &str, from: &str, subject: &str, date: &str| {
            kids(
                n(id, Role::Row, ""),
                vec![
                    n(&format!("{id}a"), Role::Cell, from),
                    n(&format!("{id}b"), Role::Cell, subject),
                    kids(
                        n(&format!("{id}c"), Role::Cell, ""),
                        vec![n(&format!("{id}t"), Role::StaticText, date)],
                    ),
                ],
            )
        };
        let t = tree(vec![kids(
            n("tbl", Role::Table, "Messages"),
            vec![
                row("r1", "Sam  Lee", "Friday\noff", "Oct 3"),
                row("r2", "Sam Ortiz", "Invoice", "Oct 4"),
            ],
        )]);
        assert_eq!(
            targets(&t),
            [
                "Sam Lee — Friday off — Oct 3",
                "Sam Ortiz — Invoice — Oct 4"
            ]
        );
    }

    #[test]
    fn files_list() {
        let row = |name: &str, label: &str| {
            kids(
                n(&format!("row.{name}"), Role::ListItem, name),
                vec![n(&format!("item.{name}"), Role::Button, label)],
            )
        };
        let t = tree(vec![
            n("up", Role::Button, "Up one folder"),
            kids(
                n("list", Role::List, "Contents"),
                vec![
                    row("Projects", "Folder Projects"),
                    row("report-2025.pdf", "File report-2025.pdf"),
                    row("report-2026.pdf", "File report-2026.pdf"),
                ],
            ),
            n("open", Role::Button, "Open"),
            n("name", Role::TextField, "Name"),
            n("trash", Role::Button, "Move to Trash"),
        ]);
        assert_eq!(
            targets(&t),
            [
                "Folder Projects",
                "File report-2025.pdf",
                "File report-2026.pdf"
            ]
        );
    }

    #[test]
    fn calendar_day_with_two_events_of_the_same_title() {
        let ev = |id: &str, title: &str, when: &str| {
            kids(
                n(&format!("row.{id}"), Role::ListItem, title),
                vec![n(
                    &format!("ev.{id}"),
                    Role::Button,
                    &format!("{when} {title}"),
                )],
            )
        };
        let t = tree(vec![
            kids(
                n("mon", Role::List, "Mon"),
                vec![
                    ev("e1", "Design review", "Mon 10:00"),
                    ev("e2", "Design review", "Mon 15:00"),
                ],
            ),
            kids(n("tue", Role::List, "Tue"), vec![]),
        ]);
        assert_eq!(
            targets(&t),
            ["Mon 10:00 Design review", "Mon 15:00 Design review"]
        );
        // Identical rows of different items are both kept: they are two targets.
        let t = tree(vec![kids(
            n("mon", Role::List, "Mon"),
            vec![
                n("a", Role::ListItem, "Standup"),
                n("b", Role::ListItem, "Standup"),
            ],
        )]);
        assert_eq!(targets(&t), ["Standup", "Standup"]);
    }

    #[test]
    fn products_skip_their_command_buttons() {
        let product = |id: &str, name: &str, price: &str| {
            kids(
                n(&format!("row.{id}"), Role::ListItem, name),
                vec![
                    n(
                        &format!("name.{id}"),
                        Role::StaticText,
                        &format!("{name} — {price}"),
                    ),
                    n(&format!("desc.{id}"), Role::StaticText, "Smooth ink"),
                    n(
                        &format!("add.{id}"),
                        Role::Button,
                        &format!("Add {name} to cart"),
                    ),
                ],
            )
        };
        let t = tree(vec![kids(
            n("products", Role::List, "Products"),
            vec![product("p1", "Gel pen", "$2.00")],
        )]);
        assert_eq!(targets(&t), ["Gel pen — $2.00 — Smooth ink"]);
    }

    #[test]
    fn toolbar_menus_forms_and_commands_yield_nothing() {
        let t = tree(vec![
            kids(
                n("tb", Role::Toolbar, "Toolbar"),
                vec![
                    n("reply", Role::Button, "Reply"),
                    n("fwd", Role::Button, "Forward"),
                    kids(
                        n("tbl", Role::List, ""),
                        vec![n("x", Role::ListItem, "Inside the toolbar")],
                    ),
                ],
            ),
            kids(
                n("menu", Role::Menu, "File"),
                vec![n("mi", Role::MenuItem, "New Window")],
            ),
            n("send", Role::Button, "Send"),
            n("back", Role::Button, "Back"),
            n("to", Role::TextField, "To"),
            kids(
                n("prio", Role::ComboBox, "Priority"),
                vec![
                    n("low", Role::ListItem, "Low"),
                    n("hi", Role::ListItem, "High"),
                ],
            ),
            // A list's own command buttons are not items; its link items are.
            kids(
                n("people", Role::List, "People"),
                vec![
                    n("more", Role::Button, "Load more"),
                    n("sam", Role::Link, "Sam Lee"),
                    n("blank", Role::ListItem, ""),
                ],
            ),
        ]);
        assert_eq!(targets(&t), ["Sam Lee"]);
        assert!(targets(&tree(vec![n("ok", Role::Button, "OK")])).is_empty());
    }

    #[test]
    fn nested_items_hidden_rows_and_secure_values() {
        let mut hidden = n("hidden", Role::ListItem, "Off screen");
        hidden.states = States {
            offscreen: true,
            ..States::default()
        };
        let mut secret = n("pw", Role::StaticText, "Password");
        secret.value = Some("hunter2".into());
        secret.states.secure = true;
        let t = tree(vec![kids(
            n("tree", Role::List, "Folders"),
            vec![
                kids(
                    n("docs", Role::ListItem, "Documents"),
                    vec![kids(
                        n("sub", Role::List, ""),
                        vec![n("cv", Role::ListItem, "cv.pdf")],
                    )],
                ),
                hidden,
                kids(n("acct", Role::ListItem, "Account"), vec![secret]),
            ],
        )]);
        assert_eq!(targets(&t), ["Documents", "cv.pdf", "Account — Password"]);
    }

    #[test]
    fn labels_are_cleaned_and_cut_and_the_list_is_capped() {
        let long = "x".repeat(400);
        let t = tree(vec![kids(
            n("l", Role::List, ""),
            vec![
                n("a", Role::ListItem, "[e1] \"quoted\"\n<b>"),
                n("b", Role::ListItem, &long),
            ],
        )]);
        let got = targets(&t);
        assert_eq!(got[0], "⟦e1⟧ \\\"quoted\\\" ‹b›");
        assert_eq!(got[1].chars().count(), LABEL_MAX + 1);
        assert!(got[1].ends_with('…'));

        let rows = (0..200)
            .map(|i| n(&format!("r{i}"), Role::ListItem, &format!("Row {i}")))
            .collect();
        let got = targets(&tree(vec![kids(n("l", Role::List, ""), rows)]));
        assert_eq!(got.len(), TARGETS_MAX);
        assert_eq!(got[0], "Row 0");
        assert_eq!(got[TARGETS_MAX - 1], format!("Row {}", TARGETS_MAX - 1));
    }
}
