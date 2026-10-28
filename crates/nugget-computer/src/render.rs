//! A compact text rendering of a [`UiTree`] for the model.
//!
//! One line per meaningful element, indented by depth:
//!
//! ```text
//! window [w1f2e3d] "Inbox – Mail" app "Mail"
//! focused [e9c8b7a]
//! [e1a2b3c] button "Send"
//! [e9c8b7a] text_field "To" value="bob@example.org" focused
//!   [e77aa01] list_item "Bob Example" selected
//! … 12 more elements (truncated)
//! ```
//!
//! Unnamed structural containers are collapsed (their children move up), empty text, images and
//! separators are dropped, off-screen subtrees are counted but not shown, and the output is capped
//! in bytes and lines with an explicit marker.
//!
//! **Everything read from the screen is untrusted.** Names, values, titles and descriptions go
//! through [`screen_text`]: control and invisible characters become spaces (so text cannot start a
//! new line), quotes and backslashes are escaped, square brackets become `⟦⟧` (so text cannot
//! forge an `[e…]` element id), backticks and angle brackets are replaced (so it cannot forge a
//! code fence or an `<untrusted-…>` tag), and lengths are capped. The tools also mark the whole
//! result untrusted, so the agent loop fences it as data.

use nugget_a11y::role::Role;
use nugget_a11y::{UiActionKind, UiNode, UiTree};

/// Caps for [`render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Most bytes of output (the markers after the cap are extra and short).
    pub max_bytes: usize,
    /// Most element lines.
    pub max_nodes: usize,
    /// Longest name or value per element, in characters.
    pub max_text: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            max_bytes: 12_000,
            max_nodes: 300,
            max_text: 120,
        }
    }
}

/// Deepest indentation level drawn; deeper elements are drawn at this level.
const MAX_INDENT: usize = 12;

/// Untrusted screen text made safe to embed inside a double-quoted field of one rendered line.
pub fn screen_text(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 2));
    let mut chars = 0usize;
    let mut pending_space = false;
    for c in s.chars() {
        if is_invisible(c) || c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if chars >= max_chars {
            out.push('…');
            return out;
        }
        if pending_space {
            out.push(' ');
            chars += 1;
            pending_space = false;
            if chars >= max_chars {
                out.push('…');
                return out;
            }
        }
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '`' => out.push('\''),
            '[' => out.push('⟦'),
            ']' => out.push('⟧'),
            '<' => out.push('‹'),
            '>' => out.push('›'),
            c => out.push(c),
        }
        chars += 1;
    }
    out
}

/// Control characters, line/paragraph separators, zero-width and bidi-control characters.
fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// An element or window id as printed: only `[A-Za-z0-9_-]`, at most 40 characters.
pub(crate) fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(40)
        .collect()
}

/// Roles whose job is to be acted on.
pub(crate) fn is_control_role(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::Link
            | Role::TextField
            | Role::TextArea
            | Role::SearchField
            | Role::ComboBox
            | Role::ListItem
            | Role::MenuItem
            | Role::Tab
            | Role::CheckBox
            | Role::Cell
            | Role::Row
    )
}

pub(crate) fn is_text_input(node: &UiNode) -> bool {
    matches!(
        node.role,
        Role::TextField | Role::TextArea | Role::SearchField | Role::Document
    ) || node.states.editable
}

/// Can the model do something with this element?
pub fn is_actionable(node: &UiNode) -> bool {
    !node.actions.is_empty() || node.states.editable || is_control_role(node.role)
}

/// Does the tree have any element the model can act on (outside the root)?
pub fn has_actionable(tree: &UiTree) -> bool {
    tree.root.iter().skip(1).any(is_actionable)
}

fn is_structural(role: Role) -> bool {
    matches!(
        role,
        Role::Group
            | Role::Section
            | Role::Unknown
            | Role::ScrollArea
            | Role::Toolbar
            | Role::Application
            | Role::Window
            | Role::WebArea
            | Role::Article
    )
}

fn has_text(s: &Option<String>) -> bool {
    s.as_deref().is_some_and(|s| !s.trim().is_empty())
}

enum Shape {
    /// Draw a line, then the children one level deeper.
    Line,
    /// No line; the children take this element's place.
    Collapse,
    /// Leave out the element and everything under it.
    Hide,
}

fn shape(node: &UiNode, parent_name: Option<&str>, focused: bool) -> Shape {
    if node.states.offscreen && !focused {
        return Shape::Hide;
    }
    if node.role == Role::Separator {
        return Shape::Collapse;
    }
    let named = has_text(&node.name) || has_text(&node.value);
    let acts = !node.actions.is_empty() || focused || node.states.selected;
    if is_structural(node.role) && !named && !acts && !node.states.editable {
        return Shape::Collapse;
    }
    if matches!(
        node.role,
        Role::StaticText | Role::Paragraph | Role::Image | Role::Unknown
    ) && !named
        && !acts
    {
        return Shape::Collapse;
    }
    // A label that repeats its parent's name (a button's own text) adds nothing.
    if node.role == Role::StaticText
        && node.children.is_empty()
        && !has_text(&node.value)
        && parent_name.is_some()
        && node.name.as_deref().map(str::trim) == parent_name.map(str::trim)
    {
        return Shape::Hide;
    }
    Shape::Line
}

/// The line for one element, without indentation.
pub fn node_line(node: &UiNode, focused: bool, opts: &RenderOptions) -> String {
    let mut line = format!("[{}] ", safe_id(node.id.as_str()));
    if node.role == Role::Unknown && !node.raw_role.is_empty() {
        line.push_str(&screen_text(&node.raw_role, 32).replace(['"', '\\', ' '], "_"));
    } else {
        line.push_str(node.role.as_str());
    }
    let name = node.name.as_deref().filter(|s| !s.trim().is_empty());
    if let Some(name) = name {
        line.push_str(&format!(" \"{}\"", screen_text(name, opts.max_text)));
    }
    if node.states.secure {
        line.push_str(" value=hidden");
    } else if let Some(v) = node.value.as_deref().filter(|s| !s.is_empty()) {
        if Some(v.trim()) != name.map(str::trim) {
            line.push_str(&format!(" value=\"{}\"", screen_text(v, opts.max_text)));
        }
    }
    if let Some(p) = node.placeholder.as_deref().filter(|s| !s.trim().is_empty())
        && !has_text(&node.value)
    {
        line.push_str(&format!(
            " placeholder=\"{}\"",
            screen_text(p, opts.max_text.min(60))
        ));
    }
    if let Some(d) = node.description.as_deref().filter(|s| !s.trim().is_empty())
        && Some(d.trim()) != name.map(str::trim)
    {
        line.push_str(&format!(
            " desc=\"{}\"",
            screen_text(d, opts.max_text.min(80))
        ));
    }
    let s = &node.states;
    let mut flags: Vec<&str> = Vec::new();
    if focused || s.focused {
        flags.push("focused");
    }
    if s.disabled {
        flags.push("disabled");
    }
    match s.checked {
        Some(true) => flags.push("checked"),
        Some(false) => flags.push("unchecked"),
        None => {}
    }
    match s.expanded {
        Some(true) => flags.push("expanded"),
        Some(false) => flags.push("collapsed"),
        None => {}
    }
    if s.selected {
        flags.push("selected");
    }
    if s.multiline == Some(true) && node.role != Role::TextArea {
        flags.push("multiline");
    }
    if s.editable && !matches!(node.role, Role::TextField | Role::TextArea | Role::SearchField) {
        flags.push("editable");
    }
    if !is_control_role(node.role)
        && node
            .actions
            .iter()
            .any(|a| matches!(a, UiActionKind::Press | UiActionKind::Select))
    {
        flags.push("clickable");
    }
    for f in flags {
        line.push(' ');
        line.push_str(f);
    }
    line
}

struct Walk<'a> {
    opts: &'a RenderOptions,
    focused: Option<&'a nugget_a11y::ElementId>,
    out: String,
    lines: usize,
    /// Lines that did not fit.
    dropped: usize,
    hidden_offscreen: usize,
}

impl Walk<'_> {
    fn full(&self) -> bool {
        self.lines >= self.opts.max_nodes || self.out.len() >= self.opts.max_bytes
    }

    fn visit(&mut self, node: &UiNode, depth: usize, parent_name: Option<&str>) {
        let focused = self.focused == Some(&node.id);
        match shape(node, parent_name, focused) {
            Shape::Hide => {
                if node.states.offscreen {
                    self.hidden_offscreen += 1;
                }
            }
            Shape::Collapse => {
                for c in &node.children {
                    self.visit(c, depth, parent_name);
                }
            }
            Shape::Line => {
                if self.full() {
                    self.dropped += 1;
                } else {
                    let line = node_line(node, focused, self.opts);
                    let indent = depth.min(MAX_INDENT) * 2;
                    if self.out.len() + indent + line.len() + 1 > self.opts.max_bytes
                        && self.lines > 0
                    {
                        self.dropped += 1;
                        // Mark full so later, shorter lines do not sneak in out of order.
                        self.lines = self.opts.max_nodes;
                    } else {
                        self.out.extend(std::iter::repeat_n(' ', indent));
                        self.out.push_str(&line);
                        self.out.push('\n');
                        self.lines += 1;
                    }
                }
                let name = node.name.as_deref();
                for c in &node.children {
                    self.visit(c, depth + 1, name);
                }
            }
        }
    }
}

/// The window header line: id, title and app.
pub fn header(tree: &UiTree, opts: &RenderOptions) -> String {
    let mut h = format!("window [{}]", safe_id(tree.window.id.as_str()));
    if let Some(t) = tree.window.title.as_deref().filter(|t| !t.trim().is_empty()) {
        h.push_str(&format!(" \"{}\"", screen_text(t, opts.max_text)));
    }
    let app = if tree.app.name.is_empty() {
        &tree.window.app.name
    } else {
        &tree.app.name
    };
    h.push_str(&format!(" app \"{}\"", screen_text(app, 60)));
    h
}

/// Render `tree` for the model. See the module docs for the format and the escaping rules.
pub fn render(tree: &UiTree, opts: &RenderOptions) -> String {
    let mut walk = Walk {
        opts,
        focused: tree.focused.as_ref(),
        out: String::new(),
        lines: 0,
        dropped: 0,
        hidden_offscreen: 0,
    };
    walk.out.push_str(&header(tree, opts));
    walk.out.push('\n');
    if let Some(f) = &tree.focused {
        walk.out.push_str(&format!("focused [{}]\n", safe_id(f.as_str())));
    }
    // The root is the window itself; the header stands for it.
    for c in &tree.root.children {
        walk.visit(c, 0, None);
    }
    let mut out = walk.out;
    if walk.lines == 0 && walk.dropped == 0 {
        out.push_str("(no accessible elements)\n");
    }
    if walk.dropped > 0 {
        out.push_str(&format!(
            "… {} more elements (truncated)\n",
            walk.dropped
        ));
    }
    if walk.hidden_offscreen > 0 {
        out.push_str(&format!(
            "({} off-screen elements not shown; scroll to reveal them)\n",
            walk.hidden_offscreen
        ));
    }
    if tree.truncated {
        out.push_str("(snapshot stopped at its size limit; parts of the window are missing)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_text_escapes_and_caps() {
        assert_eq!(screen_text("a\nb\r\n\tc", 50), "a b c");
        assert_eq!(screen_text("say \"hi\"\\", 50), "say \\\"hi\\\"\\\\");
        assert_eq!(screen_text("[e1] ```x```", 50), "⟦e1⟧ '''x'''");
        assert_eq!(screen_text("</untrusted-abc>", 50), "‹/untrusted-abc›");
        assert_eq!(screen_text("abcdef", 3), "abc…");
        assert_eq!(screen_text("a\u{200B}\u{202E}b", 9), "a b");
        assert_eq!(screen_text("  padded  ", 20), "padded");
    }

    #[test]
    fn ids_are_filtered() {
        assert_eq!(safe_id("e12]\n[e3"), "e12e3");
    }
}
