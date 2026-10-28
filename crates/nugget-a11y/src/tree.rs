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
}
