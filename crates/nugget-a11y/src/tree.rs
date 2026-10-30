//! Element tree: the [`Element`] trait every backend implements over its live accessibility
//! objects, the owned [`Node`] snapshot the heuristics run on, and the bounded walk that turns
//! the one into the other.
//!
//! Snapshots are plain data (serde), so trees recorded from real apps can be replayed in tests
//! (`tests/fixtures/trees/*.json`) and shipped with bug reports after redaction.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Rect;
use crate::role::Role;
use crate::sanitize;

/// Boolean element states the crate cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct States {
    #[serde(skip_serializing_if = "is_false")]
    pub focused: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub editable: bool,
    /// Password or otherwise protected. The value is never read.
    #[serde(skip_serializing_if = "is_false")]
    pub secure: bool,
    /// `Some(true)` multi-line, `Some(false)` single-line, `None` unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiline: Option<bool>,
    #[serde(skip_serializing_if = "is_false")]
    pub selected: bool,
    /// Off screen or hidden.
    #[serde(skip_serializing_if = "is_false")]
    pub offscreen: bool,
    /// Greyed out; actions will be refused by the app (M4).
    #[serde(skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// Check box, switch, radio or toggle button state; `None` when not checkable (M4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Expandable elements: `Some(true)` expanded, `Some(false)` collapsed (M4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
}

impl States {
    fn is_default(&self) -> bool {
        *self == States::default()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// An owned snapshot of one element and (some of) its descendants. Every string is sanitised and
/// capped when it is built from a live element.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    pub role: Role,
    /// The platform role (`AXTextArea`, `50004`, `password text`...), for debugging and rules.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub raw_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Text content: a field's value, or the text of a static text / paragraph. Never set for a
    /// secure element.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "States::is_default")]
    pub states: States,
    #[serde(skip_serializing_if = "Option::is_none", with = "rect_serde")]
    pub bounds: Option<Rect>,
    /// A few extra attributes (`dom_classes`, `identifier`, `xml_roles`, `url`...).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

mod rect_serde {
    use super::Rect;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(r: &Option<Rect>, s: S) -> Result<S::Ok, S::Error> {
        r.map(|r| [r.x, r.y, r.width, r.height]).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rect>, D::Error> {
        Ok(
            Option::<[f64; 4]>::deserialize(d)?.map(|[x, y, width, height]| Rect {
                x,
                y,
                width,
                height,
            }),
        )
    }
}

impl Node {
    pub fn new(role: Role) -> Self {
        Node {
            role,
            ..Default::default()
        }
    }

    /// Sanitise every string in place and drop the value of a secure element. Backends call this
    /// on each node they build; [`snapshot_around`] calls it again as a backstop.
    pub fn sanitize(&mut self) {
        let line = |s: &mut Option<String>| {
            *s = sanitize::clean_opt(s.as_deref(), sanitize::MAX_SHORT * 4);
        };
        line(&mut self.name);
        line(&mut self.description);
        line(&mut self.placeholder);
        if self.states.secure {
            self.value = None;
        } else if let Some(v) = &self.value {
            let v = sanitize::clean_text(v, crate::thread::MAX_MESSAGE_BYTES);
            self.value = (!v.is_empty()).then_some(v);
        }
        self.raw_role = sanitize::clean_line(&self.raw_role, 64);
        self.attrs = std::mem::take(&mut self.attrs)
            .into_iter()
            .take(16)
            .map(|(k, v)| {
                (
                    sanitize::clean_line(&k, 64),
                    sanitize::clean_line(&v, sanitize::MAX_SHORT * 4),
                )
            })
            .collect();
    }

    /// The text this node shows by itself (not its children): the value, else the name for
    /// content roles.
    pub fn own_text(&self) -> Option<&str> {
        if self.states.secure {
            return None;
        }
        fn nonempty(s: &Option<String>) -> Option<&str> {
            s.as_deref().filter(|s| !s.trim().is_empty())
        }
        if let Some(v) = nonempty(&self.value) {
            return Some(v);
        }
        match self.role {
            r if r.is_text() => nonempty(&self.name),
            Role::Unknown | Role::ListItem | Role::Article | Role::Group | Role::Section
                if self.children.is_empty() =>
            {
                nonempty(&self.name)
            }
            _ => None,
        }
    }

    /// Depth-first pre-order iterator over this node and its descendants.
    pub fn iter(&self) -> impl Iterator<Item = &Node> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let n = stack.pop()?;
            stack.extend(n.children.iter().rev());
            Some(n)
        })
    }

    /// Number of nodes in this subtree.
    pub fn count(&self) -> usize {
        self.iter().count()
    }

    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }
}

/// A snapshot of the focused element's surroundings: the tree from the window (or the highest
/// ancestor reached) down, with the path to the focused element.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub root: Node,
    /// Child indices from `root` to the focused element.
    pub focus_path: Vec<usize>,
    /// The walk stopped early (node budget or deadline).
    pub truncated: bool,
}

impl Snapshot {
    pub fn focused(&self) -> Option<&Node> {
        self.node_at(&self.focus_path)
    }

    pub fn node_at(&self, path: &[usize]) -> Option<&Node> {
        let mut n = &self.root;
        for &i in path {
            n = n.children.get(i)?;
        }
        Some(n)
    }

    /// Ancestors of the focused element, nearest first, each with the index of the child on the
    /// path to the focused element.
    pub fn ancestors(&self) -> Vec<(&Node, usize)> {
        let mut out = Vec::with_capacity(self.focus_path.len());
        let mut n = &self.root;
        for &i in &self.focus_path {
            out.push((n, i));
            match n.children.get(i) {
                Some(c) => n = c,
                None => break,
            }
        }
        out.reverse();
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

/// A live accessibility element. Implemented by each backend; M4's computer use builds on it.
pub trait Element: Sized {
    /// A shallow snapshot of this element (no children). Implementations must not read the
    /// value of a secure element, and should batch attribute reads where the platform allows.
    fn node(&self) -> Node;
    /// Up to `max` children, in order.
    fn children(&self, max: usize) -> Vec<Self>;
    fn parent(&self) -> Option<Self>;
    /// Whether two handles refer to the same element.
    fn same(&self, other: &Self) -> bool;

    /// The actions this element supports (M4 computer use). Default: none advertised.
    fn actions(&self) -> Vec<crate::computer::UiActionKind> {
        Vec::new()
    }

    /// Perform `action` on this element through the accessibility API (M4 computer use). Only
    /// performs it; [`Backend`](crate::Backend) checks policy before and verifies after.
    fn perform(&self, _action: &crate::computer::UiAction) -> nugget_core::Result<()> {
        Err(nugget_core::Error::Unsupported(
            "this backend cannot act on elements".into(),
        ))
    }
}

/// Bounds for [`snapshot_around`].
#[derive(Debug, Clone, Copy)]
pub struct WalkLimits {
    /// Total nodes read.
    pub max_nodes: usize,
    /// Ancestors climbed above the focused element.
    pub max_ancestors: usize,
    /// Depth of each sibling subtree.
    pub max_depth: usize,
    /// Children read per element.
    pub max_children: usize,
    /// Wall-clock budget for the whole walk.
    pub deadline: Duration,
}

impl Default for WalkLimits {
    fn default() -> Self {
        WalkLimits {
            max_nodes: 1500,
            max_ancestors: 30,
            max_depth: 24,
            max_children: 400,
            deadline: Duration::from_millis(600),
        }
    }
}

struct Budget {
    nodes_left: usize,
    deadline: Instant,
}

impl Budget {
    fn take(&mut self) -> bool {
        if self.nodes_left == 0 || Instant::now() >= self.deadline {
            self.nodes_left = 0;
            return false;
        }
        self.nodes_left -= 1;
        true
    }
    fn exhausted(&self) -> bool {
        self.nodes_left == 0 || Instant::now() >= self.deadline
    }
}

fn read_node<E: Element>(el: &E) -> Node {
    let mut n = el.node();
    n.children.clear();
    n.sanitize();
    n
}

fn walk<E: Element>(el: &E, depth: usize, cap: usize, limits: &WalkLimits, b: &mut Budget) -> Node {
    let mut node = read_node(el);
    let mut used = 1;
    if depth >= limits.max_depth || node.states.secure {
        return node;
    }
    for child in el.children(limits.max_children) {
        if used >= cap || !b.take() {
            break;
        }
        let child_cap = cap - used;
        let before = b.nodes_left;
        node.children
            .push(walk(&child, depth + 1, child_cap, limits, b));
        used += 1 + before.saturating_sub(b.nodes_left);
    }
    node
}

/// Snapshot the neighbourhood of `focused`: climb its ancestors (stopping at the window), then
/// read sibling subtrees nearest-first so the region around the composer is captured before the
/// budget runs out.
pub fn snapshot_around<E: Element>(focused: &E, limits: &WalkLimits) -> Snapshot {
    let mut b = Budget {
        nodes_left: limits.max_nodes,
        deadline: Instant::now() + limits.deadline,
    };
    let mut truncated = false;

    // The focused element and a small subtree (mail composers hold the quoted original).
    b.take();
    let mut current = walk(focused, 0, 200.min(b.nodes_left + 1), limits, &mut b);
    let mut path_rev: Vec<usize> = Vec::new();

    let mut child_handle: Option<E> = None;
    let mut cursor = focused.parent();
    let mut levels = 0;
    while let Some(anc) = cursor {
        if levels >= limits.max_ancestors || b.exhausted() {
            truncated = true;
            break;
        }
        levels += 1;
        b.take();
        let mut anc_node = read_node(&anc);
        if anc_node.role == Role::Application {
            break;
        }
        let prev = child_handle.take();
        let kids = anc.children(limits.max_children);
        let on_path = kids
            .iter()
            .position(|k| match &prev {
                Some(p) => k.same(p),
                None => k.same(focused),
            })
            .unwrap_or(kids.len());
        // Read siblings nearest the path first; keep document order in the output.
        let mut order: Vec<usize> = (0..kids.len()).filter(|&i| i != on_path).collect();
        order.sort_by_key(|&i| i.abs_diff(on_path));
        let mut built: Vec<Option<Node>> = vec![None; kids.len()];
        for i in order {
            if !b.take() {
                truncated = true;
                break;
            }
            let cap = (b.nodes_left * 3 / 5).max(1);
            built[i] = Some(walk(&kids[i], 1, cap, limits, &mut b));
        }
        let mut children = Vec::with_capacity(kids.len() + 1);
        let mut index_of_path = 0;
        let mut placed = false;
        for (i, slot) in built.into_iter().enumerate() {
            if i == on_path {
                index_of_path = children.len();
                children.push(std::mem::take(&mut current));
                placed = true;
            } else if let Some(n) = slot {
                children.push(n);
            }
        }
        if !placed {
            index_of_path = children.len();
            children.push(std::mem::take(&mut current));
        }
        anc_node.children = children;
        path_rev.push(index_of_path);
        current = anc_node;
        let is_window = matches!(current.role, Role::Window | Role::Dialog);
        child_handle = Some(anc);
        if is_window {
            break;
        }
        cursor = child_handle.as_ref().and_then(Element::parent);
    }
    if b.exhausted() {
        truncated = true;
    }
    path_rev.reverse();
    Snapshot {
        root: current,
        focus_path: path_rev,
        truncated,
    }
}

// ---------------------------------------------------------------------------------------------
// Rich editors: the editor root and its text

/// How far [`editor_root`] climbs from the focused element.
const MAX_EDITOR_CLIMB: usize = 16;

/// Roles that bound a page: the editor root is never above one of these.
fn is_page_boundary(n: &Node) -> bool {
    matches!(n.role, Role::Window | Role::Dialog | Role::Application)
}

fn is_document(n: &Node) -> bool {
    matches!(n.role, Role::WebArea | Role::Document)
}

/// The topmost editable ancestor of `el` inside the same editor, or `None` when its parent is
/// not editable.
///
/// Browsers can put the focus on a paragraph or text run inside a contenteditable (Firefox over
/// AT-SPI always, Chromium sometimes after a click). Reading that element yields one paragraph,
/// and the thread walk would start inside the editor. The climb continues while the parent is
/// editable and stops below a window, dialog or application. A web area or document is taken
/// only when it is itself inside another document (an editable iframe body, `designMode`); the
/// page's own root never is, so an editable page does not swallow the whole tab. A secure
/// element never climbs.
pub fn editor_root<E: Element>(el: &E) -> Option<E> {
    if el.node().states.secure {
        return None;
    }
    editable_ancestor(el)
}

/// [`editor_root`] for an element already known not to be secure.
pub(crate) fn editable_ancestor<E: Element>(el: &E) -> Option<E> {
    let mut best: Option<E> = None;
    let mut cur = el.parent();
    for _ in 0..MAX_EDITOR_CLIMB {
        let Some(p) = cur else { break };
        let n = p.node();
        if !n.states.editable || n.states.secure || is_page_boundary(&n) {
            break;
        }
        if is_document(&n) {
            if has_document_ancestor(&p) {
                best = Some(p);
            }
            break;
        }
        cur = p.parent();
        best = Some(p);
    }
    best
}

fn has_document_ancestor<E: Element>(el: &E) -> bool {
    let mut cur = el.parent();
    for _ in 0..40 {
        let Some(p) = cur else { return false };
        let n = p.node();
        if is_document(&n) {
            return true;
        }
        if is_page_boundary(&n) {
            return false;
        }
        cur = p.parent();
    }
    false
}

/// [`editor_root`] on a snapshot: the path to the topmost editable ancestor of the focused
/// element (the focus path itself when there is none).
pub fn editor_root_path(snap: &Snapshot) -> Vec<usize> {
    let path = &snap.focus_path;
    if snap.focused().is_none_or(|f| f.states.secure) {
        return path.clone();
    }
    let mut best = path.len();
    for len in (0..path.len()).rev().take(MAX_EDITOR_CLIMB) {
        let Some(n) = snap.node_at(&path[..len]) else {
            break;
        };
        if !n.states.editable || n.states.secure || is_page_boundary(n) {
            break;
        }
        if is_document(n) {
            let nested = (0..len).any(|l| snap.node_at(&path[..l]).is_some_and(is_document));
            if nested {
                best = len;
            }
            break;
        }
        best = len;
    }
    path[..best].to_vec()
}

/// Bounds for [`flatten_text`].
#[derive(Debug, Clone, Copy)]
pub struct FlattenLimits {
    /// Elements read.
    pub max_nodes: usize,
    /// Depth below the element.
    pub max_depth: usize,
    /// Children read per element.
    pub max_children: usize,
    /// Cap on the text built (bytes).
    pub max_bytes: usize,
    /// Wall-clock budget.
    pub deadline: Duration,
}

impl Default for FlattenLimits {
    fn default() -> Self {
        FlattenLimits {
            max_nodes: 2000,
            max_depth: 32,
            max_children: 1000,
            max_bytes: sanitize::MAX_VALUE,
            deadline: Duration::from_millis(400),
        }
    }
}

/// The text of an element rebuilt from its descendants by [`flatten_text_with`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlatText {
    pub text: String,
    /// Character offset in `text` where the `mark` element's text starts, when it was reached.
    pub mark_start: Option<u32>,
    /// The walk stopped early (budget, deadline, byte cap) or an element's text looked cut at a
    /// platform read cap, so `text` may be incomplete.
    pub truncated: bool,
    /// For each character offset into the `own_text` passed in (one past the end included), the
    /// offset into `text`; `None` at an embedded object, where the caret may be inside it.
    own_offsets: Vec<Option<u32>>,
}

impl FlatText {
    /// Map a character offset into the `own_text` given to [`flatten_text_with`] to an offset
    /// into [`FlatText::text`]. `None` when it is out of range or at an embedded object.
    pub fn map_own_offset(&self, offset: u32) -> Option<u32> {
        self.own_offsets.get(offset as usize).copied().flatten()
    }
}

/// The object replacement character: AT-SPI (and sometimes AX) stands one in for each embedded
/// child element in its parent's text.
pub const EMBEDDED_OBJECT: char = '\u{FFFC}';

/// Text runs longer than this many characters may have been cut by a platform's per-element
/// read cap (the AT-SPI backend reads 4096 characters per element).
const ELEMENT_TEXT_CAP: usize = 4096;

/// The plain text of `el` built from its descendants, in order: text leaves as they are, block
/// elements (paragraphs, sections, list items, headings, groups) on their own lines with one
/// `\n` between them. An element whose text holds [`EMBEDDED_OBJECT`] characters has each one
/// replaced by the text of the matching child (when the counts agree). Secure elements add
/// nothing. Bounded by `limits`. This is how a rich editor's value is read when its own text is
/// only placeholders for its paragraphs.
pub fn flatten_text<E: Element>(el: &E, limits: &FlattenLimits) -> String {
    flatten_text_with(el, None, None, limits).text
}

/// [`flatten_text`] with two extras:
/// - `own_text` replaces `el`'s own text (callers pass the value they read with its embedded
///   objects kept; `Element::node` may have them stripped), and its offsets are mapped
///   ([`FlatText::map_own_offset`]);
/// - `mark` is a descendant whose start offset in the result is reported (the element with the
///   caret).
pub fn flatten_text_with<E: Element>(
    el: &E,
    own_text: Option<&str>,
    mark: Option<&E>,
    limits: &FlattenLimits,
) -> FlatText {
    let mut f = Flattener {
        out: String::new(),
        chars: 0,
        nodes_left: limits.max_nodes,
        deadline: Instant::now() + limits.deadline,
        limits,
        mark,
        mark_start: None,
        truncated: false,
        started: false,
        own_newline: false,
        after_block: false,
        own_offsets: Vec::new(),
    };
    f.visit(el, 0, own_text);
    FlatText {
        text: f.out,
        mark_start: f.mark_start,
        truncated: f.truncated,
        own_offsets: f.own_offsets,
    }
}

/// Elements that sit on their own line in an editor's text.
fn is_block(role: Role) -> bool {
    matches!(
        role,
        Role::Paragraph
            | Role::Heading
            | Role::Section
            | Role::Group
            | Role::List
            | Role::ListItem
            | Role::Article
            | Role::Table
            | Role::Row
            | Role::Log
            | Role::Document
            | Role::TextArea
    )
}

struct Flattener<'a, E> {
    out: String,
    chars: u32,
    nodes_left: usize,
    deadline: Instant,
    limits: &'a FlattenLimits,
    mark: Option<&'a E>,
    mark_start: Option<u32>,
    truncated: bool,
    /// Something (text or a block, even an empty one) was emitted.
    started: bool,
    /// The last character emitted was a `\n` from an element's own text, which already
    /// separates the next block.
    own_newline: bool,
    /// A block just ended: inline text that follows starts a new line.
    after_block: bool,
    own_offsets: Vec<Option<u32>>,
}

impl<E: Element> Flattener<'_, E> {
    fn full(&mut self) -> bool {
        let full = self.out.len() >= self.limits.max_bytes;
        self.truncated |= full;
        full
    }

    fn push(&mut self, c: char) {
        if self.out.len() + c.len_utf8() > self.limits.max_bytes {
            self.truncated = true;
            return;
        }
        self.out.push(c);
        self.chars += 1;
    }

    /// Emit one character of inline text.
    fn text_char(&mut self, c: char) {
        if self.after_block {
            self.after_block = false;
            if c == '\n' {
                self.push('\n');
                self.own_newline = true;
                return;
            }
            self.push('\n');
        }
        self.push(c);
        self.own_newline = c == '\n';
        self.started = true;
    }

    fn text(&mut self, s: &str) {
        if s.chars().count() >= ELEMENT_TEXT_CAP {
            self.truncated = true;
        }
        for c in sanitize::clean_value(s, self.limits.max_bytes).chars() {
            if c != EMBEDDED_OBJECT {
                self.text_char(c);
            }
        }
    }

    fn visit(&mut self, el: &E, depth: usize, own: Option<&str>) {
        if self.full() {
            return;
        }
        if self.nodes_left == 0 || Instant::now() >= self.deadline {
            self.truncated = true;
            return;
        }
        self.nodes_left -= 1;
        // The root's role and value come from the caller; only descendants are read.
        let node = if depth == 0 && own.is_some() {
            Node::default()
        } else {
            el.node()
        };
        if node.states.secure {
            return;
        }
        let block = depth > 0 && is_block(node.role);
        if block {
            if self.started && !self.own_newline {
                self.push('\n');
            }
            self.after_block = false;
            self.own_newline = false;
        }
        if self.mark_start.is_none() && depth > 0 && self.mark.is_some_and(|m| el.same(m)) {
            self.mark_start = Some(self.chars);
        }
        let start_len = self.out.len();
        let own_text: Option<String> = match own {
            Some(t) => Some(t.to_string()),
            None => node.value.clone().filter(|v| !v.is_empty()),
        };
        let kids = |s: &Self| {
            if depth < s.limits.max_depth {
                el.children(s.limits.max_children)
            } else {
                Vec::new()
            }
        };
        match own_text {
            Some(t) if t.contains(EMBEDDED_OBJECT) => {
                let children = kids(self);
                let objects = t.chars().filter(|&c| c == EMBEDDED_OBJECT).count();
                if objects == children.len() {
                    self.interleave(&t, &children, depth);
                } else {
                    // The placeholders do not line up with the children: rebuild from the
                    // children alone.
                    if depth == 0 {
                        self.own_offsets.clear();
                    }
                    for c in &children {
                        self.visit(c, depth + 1, None);
                    }
                }
            }
            Some(t) => {
                if depth == 0 {
                    self.own_offsets.reserve(t.chars().count() + 1);
                    for c in t.chars() {
                        let at = self.chars + u32::from(self.after_block && c != '\n');
                        self.own_offsets.push(Some(at));
                        self.text_char(c);
                    }
                    self.own_offsets.push(Some(self.chars));
                } else {
                    self.text(&t);
                }
            }
            None => {
                let children = kids(self);
                if children.is_empty() {
                    if node.role.is_text()
                        && let Some(n) = node.name.as_deref()
                    {
                        self.text(n);
                    }
                } else {
                    for c in &children {
                        self.visit(c, depth + 1, None);
                    }
                }
            }
        }
        if block {
            // A trailing break inside a block (`<p>text<br></p>`) is presentational.
            if self.out.len() > start_len && self.out.ends_with('\n') {
                self.out.pop();
                self.chars -= 1;
            }
            self.started = true;
            self.after_block = true;
            self.own_newline = false;
        }
    }

    /// Own text with embedded objects replaced by the matching children's text.
    fn interleave(&mut self, own: &str, children: &[E], depth: usize) {
        let mut k = 0;
        for c in own.chars() {
            if self.full() {
                break;
            }
            if c == EMBEDDED_OBJECT {
                if depth == 0 {
                    self.own_offsets.push(None);
                }
                if let Some(child) = children.get(k) {
                    self.visit(child, depth + 1, None);
                }
                k += 1;
            } else {
                if depth == 0 {
                    let at = self.chars + u32::from(self.after_block && c != '\n');
                    self.own_offsets.push(Some(at));
                }
                for c in sanitize::clean_value(c.encode_utf8(&mut [0; 4]), 4).chars() {
                    self.text_char(c);
                }
            }
        }
        if depth == 0 {
            self.own_offsets.push(Some(self.chars));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::rc::Rc;

    /// An in-memory element over a `Node` tree, for exercising the walk.
    #[derive(Clone)]
    pub struct MemEl {
        root: Rc<Node>,
        path: Vec<usize>,
    }

    impl MemEl {
        pub fn new(root: Node, path: Vec<usize>) -> Self {
            MemEl {
                root: Rc::new(root),
                path,
            }
        }
        fn get(&self) -> &Node {
            let mut n = &*self.root;
            for &i in &self.path {
                n = &n.children[i];
            }
            n
        }
    }

    impl Element for MemEl {
        fn node(&self) -> Node {
            let mut n = self.get().clone();
            n.children.clear();
            n
        }
        fn children(&self, max: usize) -> Vec<Self> {
            (0..self.get().children.len().min(max))
                .map(|i| {
                    let mut path = self.path.clone();
                    path.push(i);
                    MemEl {
                        root: self.root.clone(),
                        path,
                    }
                })
                .collect()
        }
        fn parent(&self) -> Option<Self> {
            let mut path = self.path.clone();
            path.pop()?;
            Some(MemEl {
                root: self.root.clone(),
                path,
            })
        }
        fn same(&self, other: &Self) -> bool {
            self.path == other.path
        }
    }

    fn text(s: &str) -> Node {
        Node {
            role: Role::StaticText,
            value: Some(s.into()),
            ..Default::default()
        }
    }

    fn sample() -> Node {
        let mut composer = Node::new(Role::TextArea);
        composer.states.focused = true;
        composer.states.editable = true;
        composer.value = Some("draft".into());
        let mut list = Node::new(Role::List);
        list.children = (0..5)
            .map(|i| {
                let mut item = Node::new(Role::ListItem);
                item.children = vec![text(&format!("message {i}"))];
                item
            })
            .collect();
        let mut pane = Node::new(Role::Group);
        pane.children = vec![list, composer];
        let mut win = Node::new(Role::Window);
        win.name = Some("Chat".into());
        win.children = vec![Node::new(Role::Toolbar), pane];
        let mut app = Node::new(Role::Application);
        app.children = vec![win];
        app
    }

    #[test]
    fn snapshot_keeps_path_and_stops_at_window() {
        let el = MemEl::new(sample(), vec![0, 1, 1]);
        let snap = snapshot_around(&el, &WalkLimits::default());
        assert_eq!(snap.root.role, Role::Window);
        assert_eq!(snap.focus_path, vec![1, 1]);
        let f = snap.focused().unwrap();
        assert_eq!(f.value.as_deref(), Some("draft"));
        assert!(!snap.truncated);
        assert_eq!(snap.root.count(), sample().children[0].count());
        let anc = snap.ancestors();
        assert_eq!(anc.len(), 2);
        assert_eq!(anc[0].0.role, Role::Group);
        assert_eq!(anc[0].1, 1);
    }

    #[test]
    fn snapshot_respects_node_budget() {
        let el = MemEl::new(sample(), vec![0, 1, 1]);
        let limits = WalkLimits {
            max_nodes: 4,
            ..Default::default()
        };
        let snap = snapshot_around(&el, &limits);
        assert!(snap.truncated);
        assert!(snap.root.count() <= 6, "{}", snap.root.count());
        assert!(snap.focused().is_some());
    }

    #[test]
    fn secure_values_are_dropped_and_strings_cleaned() {
        let mut tree = sample();
        let pw = &mut tree.children[0].children[1].children[1];
        pw.states.secure = true;
        pw.value = Some("hunter2".into());
        tree.children[0].name = Some("Chat\u{202E}\u{0007}".into());
        let el = MemEl::new(tree, vec![0, 1, 1]);
        let snap = snapshot_around(&el, &WalkLimits::default());
        assert_eq!(snap.focused().unwrap().value, None);
        assert_eq!(snap.root.name.as_deref(), Some("Chat"));
    }

    #[test]
    fn json_round_trip() {
        let el = MemEl::new(sample(), vec![0, 1, 1]);
        let snap = snapshot_around(&el, &WalkLimits::default());
        let back = Snapshot::from_json(&snap.to_json()).unwrap();
        assert_eq!(back, snap);
    }

    fn para(role: Role, value: &str) -> Node {
        let mut n = Node::new(role);
        n.states.editable = true;
        n.value = (!value.is_empty()).then(|| value.to_string());
        n
    }

    /// A Firefox-style editor: the root's text is one U+FFFC per paragraph, a paragraph's text
    /// holds one for an inline link.
    fn hypertext_editor() -> Node {
        let mut link = Node::new(Role::Link);
        link.value = Some("the docs".into());
        let mut with_link = para(Role::Paragraph, "see \u{FFFC} now");
        with_link.children = vec![link];
        let mut root = para(Role::Section, "");
        root.states.multiline = Some(true);
        root.children = vec![
            para(Role::Paragraph, "Draft: hello"),
            para(Role::Paragraph, "\n"),
            para(Role::Paragraph, "second line"),
            with_link,
        ];
        let mut web = Node::new(Role::WebArea);
        web.children = vec![Node::new(Role::Heading), root];
        let mut win = Node::new(Role::Window);
        win.children = vec![web];
        win
    }

    #[test]
    fn flatten_replaces_embedded_objects_in_order() {
        let tree = hypertext_editor();
        let root = MemEl::new(tree.clone(), vec![0, 1]);
        let own = "\u{FFFC}\u{FFFC}\u{FFFC}\u{FFFC}";
        let mark = MemEl::new(tree, vec![0, 1, 2]);
        let f = flatten_text_with(&root, Some(own), Some(&mark), &FlattenLimits::default());
        // The empty paragraph (a lone trailing break) is an empty line.
        assert_eq!(f.text, "Draft: hello\n\nsecond line\nsee the docs now");
        assert_eq!(f.mark_start, Some(14));
        assert!(!f.truncated);
        // Offsets at an embedded object are ambiguous; the end maps to the end.
        assert_eq!(f.map_own_offset(0), None);
        assert_eq!(f.map_own_offset(4), Some(f.text.chars().count() as u32));
        assert_eq!(f.map_own_offset(5), None);
    }

    #[test]
    fn flatten_from_children_and_mixed_text() {
        // Chromium-style: no own text, paragraphs are groups of static text.
        let mut root = para(Role::TextArea, "");
        root.children = ["one", "two"]
            .iter()
            .map(|t| {
                let mut g = Node::new(Role::Group);
                g.children = vec![text(t)];
                g
            })
            .collect();
        let el = MemEl::new(root, vec![]);
        assert_eq!(flatten_text(&el, &FlattenLimits::default()), "one\ntwo");

        // Inline text around a block, with the offsets of the own text mapped.
        let mut root = para(Role::Section, "");
        root.children = vec![para(Role::Paragraph, "X")];
        let el = MemEl::new(root, vec![]);
        let f = flatten_text_with(&el, Some("ab\u{FFFC}cd"), None, &FlattenLimits::default());
        assert_eq!(f.text, "ab\nX\ncd");
        let mapped: Vec<_> = (0..6).map(|i| f.map_own_offset(i)).collect();
        assert_eq!(mapped, [Some(0), Some(1), None, Some(5), Some(6), Some(7)]);

        // Placeholders that do not match the children: rebuilt from the children, no offsets.
        let f = flatten_text_with(
            &el,
            Some("\u{FFFC}\u{FFFC}"),
            None,
            &FlattenLimits::default(),
        );
        assert_eq!(f.text, "X");
        assert_eq!(f.map_own_offset(0), None);
    }

    #[test]
    fn flatten_is_bounded_and_skips_secure() {
        let mut root = para(Role::Section, "");
        let mut pw = para(Role::TextField, "hunter2");
        pw.states.secure = true;
        root.children = vec![para(Role::Paragraph, "hello world"), pw];
        let el = MemEl::new(root.clone(), vec![]);
        assert_eq!(flatten_text(&el, &FlattenLimits::default()), "hello world");
        let limits = FlattenLimits {
            max_bytes: 5,
            ..Default::default()
        };
        let f = flatten_text_with(&el, None, None, &limits);
        assert_eq!(f.text, "hello");
        assert!(f.truncated);
        let limits = FlattenLimits {
            max_nodes: 1,
            ..Default::default()
        };
        assert!(flatten_text_with(&el, None, None, &limits).truncated);
        // A text run at a platform's per-element cap may be cut.
        root.children = vec![para(Role::Paragraph, &"x".repeat(ELEMENT_TEXT_CAP))];
        let el = MemEl::new(root, vec![]);
        assert!(flatten_text_with(&el, None, None, &FlattenLimits::default()).truncated);
    }

    #[test]
    fn editor_root_climbs_editable_ancestors_only() {
        let tree = hypertext_editor();
        let p = MemEl::new(tree.clone(), vec![0, 1, 2]);
        let root = editor_root(&p).unwrap();
        assert!(root.same(&MemEl::new(tree.clone(), vec![0, 1])));
        // The root itself has no editable parent.
        assert!(editor_root(&root).is_none());
        // An editable page root is not taken; an editable nested document (iframe body) is.
        let mut t = tree.clone();
        t.children[0].states.editable = true;
        let p = MemEl::new(t.clone(), vec![0, 1, 2]);
        assert!(editor_root(&p).unwrap().same(&MemEl::new(t, vec![0, 1])));
        let mut frame_doc = Node::new(Role::WebArea);
        frame_doc.states.editable = true;
        frame_doc.children = vec![para(Role::Paragraph, "body text")];
        let mut t = tree.clone();
        t.children[0].children[0] = frame_doc;
        let p = MemEl::new(t.clone(), vec![0, 0, 0]);
        assert!(editor_root(&p).unwrap().same(&MemEl::new(t, vec![0, 0])));
        // A secure element never climbs.
        let mut t = tree;
        t.children[0].children[1].children[0].states.secure = true;
        assert!(editor_root(&MemEl::new(t, vec![0, 1, 0])).is_none());
    }

    #[test]
    fn editor_root_path_on_snapshots() {
        let snap = Snapshot {
            root: hypertext_editor(),
            focus_path: vec![0, 1, 2],
            truncated: false,
        };
        assert_eq!(editor_root_path(&snap), vec![0, 1]);
        let snap = Snapshot {
            focus_path: vec![0, 1],
            ..snap
        };
        assert_eq!(editor_root_path(&snap), vec![0, 1]);
    }
}
