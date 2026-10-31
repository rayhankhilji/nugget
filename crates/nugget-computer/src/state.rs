//! State shared by the tools: the backend, the configuration and the last snapshot of each
//! window, so `risk` and `describe` can look an element up by id without touching the OS.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use nugget_a11y::role::Role;
use nugget_a11y::{ComputerUse, ElementId, UiNode, UiTree, WindowId, WindowInfo, WindowSel};
use nugget_core::{Error, Result};
use nugget_policy::preview::{FieldValue, display_text};

use crate::config::ComputerConfig;

/// Longest piece of dialog text kept as risk context, and how many pieces.
const CONTEXT_TEXT_MAX: usize = 200;
const CONTEXT_ITEMS_MAX: usize = 24;
/// How many send confirmations are remembered between `describe` and `call`.
const FROZEN_MAX: usize = 8;
/// Longest item name added to a Delete confirmation, in characters.
const ITEM_NAME_MAX: usize = 80;

pub(crate) struct Shared {
    pub cu: Arc<dyn ComputerUse>,
    pub config: ComputerConfig,
    /// Most recent first; one tree per window.
    cache: Mutex<VecDeque<UiTree>>,
    /// What a send's confirmation showed, by call (see [`Frozen`]), oldest first.
    frozen: Mutex<VecDeque<(String, Frozen)>>,
}

/// The fields a send or submit was described with, so `call` presses only if they still hold
/// exactly what the user approved.
#[derive(Debug, Clone)]
pub(crate) struct Frozen {
    /// The control that sends.
    pub element: ElementId,
    pub window: WindowId,
    pub fields: Vec<FieldValue>,
    pub digest: String,
}

/// An element found in a cached snapshot.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    /// The element with its subtree.
    pub node: UiNode,
    pub window: WindowInfo,
    /// App id when known, else its name (what policy rules match).
    pub app: String,
    /// Text around the element for risk: its dialog's name and text, and the window title.
    pub context: Vec<String>,
    /// The element's containers, innermost first, with their names (truncated).
    pub ancestors: Vec<(Role, Option<String>)>,
    /// Labels of the editable fields in the element's dialog, or in its window when it is in
    /// none (truncated, at most [`CONTEXT_ITEMS_MAX`]).
    pub fields: Vec<String>,
}

impl Shared {
    pub fn new(cu: Arc<dyn ComputerUse>, config: ComputerConfig) -> Arc<Self> {
        Arc::new(Self {
            cu,
            config,
            cache: Mutex::new(VecDeque::new()),
            frozen: Mutex::new(VecDeque::new()),
        })
    }

    fn frozen(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, Frozen)>> {
        self.frozen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remember what the confirmation of the call `key` showed (replacing an older one).
    pub fn freeze(&self, key: String, f: Frozen) {
        let mut q = self.frozen();
        q.retain(|(k, _)| k != &key);
        while q.len() >= FROZEN_MAX {
            q.pop_front();
        }
        q.push_back((key, f));
    }

    /// Take (and forget) what the call `key` was described with.
    pub fn thaw(&self, key: &str) -> Option<Frozen> {
        let mut q = self.frozen();
        let i = q.iter().position(|(k, _)| k == key)?;
        q.remove(i).map(|(_, f)| f)
    }

    /// The fields an activation of `id` sends, from the cached snapshots.
    pub fn send_fields(&self, id: &ElementId) -> Option<(WindowId, Vec<FieldValue>)> {
        let c = self.cache();
        c.iter()
            .find_map(|t| send_fields(t, id).map(|f| (t.window.id.clone(), f)))
    }

    /// The item an action on `id` applies to (see [`subject_item`]), from the cached snapshots.
    pub fn subject_item(&self, id: &ElementId) -> Option<(&'static str, String)> {
        let c = self.cache();
        c.iter().find_map(|t| subject_item(t, id))
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, VecDeque<UiTree>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keep `tree` as the latest snapshot of its window.
    pub fn remember(&self, tree: UiTree) {
        let mut c = self.cache();
        c.retain(|t| t.window.id != tree.window.id);
        c.push_front(tree);
        c.truncate(self.config.cached_windows.max(1));
    }

    pub fn forget(&self, window: &WindowId) {
        self.cache().retain(|t| &t.window.id != window);
    }

    /// Look `id` up in the cached snapshots, most recent first.
    pub fn find(&self, id: &ElementId) -> Option<Found> {
        let c = self.cache();
        c.iter().find_map(|t| found_in(t, id))
    }

    /// The focused element of the most recent snapshot, if it is known.
    pub fn focused(&self) -> Option<Found> {
        let c = self.cache();
        let t = c.front()?;
        found_in(t, t.focused.as_ref()?)
    }

    /// The window of the most recent snapshot.
    pub fn latest_window(&self) -> Option<WindowInfo> {
        self.cache().front().map(|t| t.window.clone())
    }

    /// A fresh, bounded snapshot (blocking; call from a blocking task). Kept in the cache.
    pub fn snapshot(&self, sel: &WindowSel) -> Result<UiTree> {
        let tree = self.cu.snapshot(sel, &self.config.tree_limits())?;
        self.remember(tree.clone());
        Ok(tree)
    }
}

/// App id when known, else the name.
pub(crate) fn app_key(tree_app_id: Option<&str>, name: &str) -> String {
    match tree_app_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => name.to_string(),
    }
}

fn path_to<'a>(node: &'a UiNode, id: &ElementId, path: &mut Vec<&'a UiNode>) -> bool {
    path.push(node);
    if &node.id == id {
        return true;
    }
    for c in &node.children {
        if path_to(c, id, path) {
            return true;
        }
    }
    path.pop();
    false
}

fn found_in(tree: &UiTree, id: &ElementId) -> Option<Found> {
    let mut path = Vec::new();
    if !path_to(&tree.root, id, &mut path) {
        return None;
    }
    let node = (*path.last()?).clone();
    let mut context = Vec::new();
    if let Some(dialog) = path.iter().rev().find(|n| n.role == Role::Dialog) {
        context.extend(dialog.name.iter().cloned());
        context.extend(dialog.description.iter().cloned());
        context.extend(
            dialog
                .iter()
                .filter(|n| matches!(n.role, Role::StaticText | Role::Paragraph | Role::Heading))
                .filter_map(|n| n.name.clone().or_else(|| n.value.clone()))
                .take(CONTEXT_ITEMS_MAX),
        );
    }
    context.extend(tree.window.title.iter().cloned());
    for s in &mut context {
        *s = nugget_agent::tool::truncate(s, CONTEXT_TEXT_MAX);
    }
    let short = |s: &Option<String>| {
        s.as_deref()
            .map(|s| nugget_agent::tool::truncate(s, CONTEXT_TEXT_MAX))
    };
    let ancestors = path
        .iter()
        .rev()
        .skip(1)
        .map(|n| (n.role, short(&n.name)))
        .collect();
    let scope = path
        .iter()
        .rev()
        .find(|n| n.role == Role::Dialog)
        .copied()
        .unwrap_or(&tree.root);
    let fields = scope
        .iter()
        .filter(|n| {
            matches!(
                n.role,
                Role::TextField | Role::TextArea | Role::SearchField | Role::ComboBox
            )
        })
        .filter_map(|n| short(&n.name).or_else(|| short(&n.placeholder)))
        .take(CONTEXT_ITEMS_MAX)
        .collect();
    let app_id = tree.app.id.as_deref().or(tree.window.app.id.as_deref());
    let name = if tree.app.name.is_empty() {
        &tree.window.app.name
    } else {
        &tree.app.name
    };
    Some(Found {
        node,
        window: tree.window.clone(),
        app: app_key(app_id, name),
        context,
        ancestors,
        fields,
    })
}

/// The dialog around `path`'s last node, else the window's root.
fn scope_of<'a>(tree: &'a UiTree, path: &[&'a UiNode]) -> &'a UiNode {
    path.iter()
        .rev()
        .find(|n| n.role == Role::Dialog)
        .copied()
        .unwrap_or(&tree.root)
}

/// Is `n` a field whose contents a send or submit carries? Search fields are not.
fn is_sent_field(n: &UiNode) -> bool {
    matches!(n.role, Role::TextField | Role::TextArea | Role::ComboBox)
        || (n.states.editable && matches!(n.role, Role::Document | Role::Unknown))
}

/// The editable fields in the dialog around `id` (else its window), in screen order, with
/// their full values: what activating `id` sends. `None` when `id` is not in `tree`.
pub(crate) fn send_fields(tree: &UiTree, id: &ElementId) -> Option<Vec<FieldValue>> {
    let mut path = Vec::new();
    if !path_to(&tree.root, id, &mut path) {
        return None;
    }
    let fields = scope_of(tree, &path)
        .iter()
        .filter(|n| is_sent_field(n))
        .take(CONTEXT_ITEMS_MAX)
        .map(|n| {
            let label = [&n.name, &n.placeholder, &n.description]
                .into_iter()
                .flatten()
                .find(|s| !s.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| n.role.as_str().replace('_', " "));
            let secret = crate::risk::is_secret_field(n);
            let value = if n.states.secure {
                None
            } else {
                n.value.clone()
            };
            FieldValue::new(label, value, secret)
        })
        .collect();
    Some(fields)
}

/// A row's own name, else the text of its first text children.
fn row_name(n: &UiNode) -> Option<String> {
    if let Some(name) = n.name.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(name.to_string());
    }
    let text: Vec<&str> = n
        .iter()
        .skip(1)
        .filter(|c| c.role.is_text() || c.role == Role::Heading)
        .filter_map(|c| c.name.as_deref().or(c.value.as_deref()))
        .filter(|s| !s.trim().is_empty())
        .take(3)
        .collect();
    (!text.is_empty()).then(|| text.join(" "))
}

const ROW_ROLES: [Role; 3] = [Role::ListItem, Role::Row, Role::Cell];

/// What an action on `id` (a Delete, say) applies to, for its confirmation: the row the control
/// sits in, else the selected row of a list, table or tree in its dialog or window
/// (`"selected"`), else the heading of the open item (`"open"`). The text is screen text,
/// sanitized and bounded.
pub(crate) fn subject_item(tree: &UiTree, id: &ElementId) -> Option<(&'static str, String)> {
    let mut path = Vec::new();
    if !path_to(&tree.root, id, &mut path) {
        return None;
    }
    let clean = |s: String| {
        let t = display_text(&s, ITEM_NAME_MAX);
        (!t.is_empty()).then_some(t)
    };
    let own = path.last()?;
    // The row a per-row button sits in.
    if let Some(row) = path
        .iter()
        .rev()
        .skip(1)
        .find(|n| matches!(n.role, Role::ListItem | Role::Row))
        && let Some(t) = row_name(row).and_then(clean)
        && own.name.as_deref().is_none_or(|n| n.trim() != t)
    {
        return Some(("selected", t));
    }
    let scope = scope_of(tree, &path);
    if let Some(t) = scope
        .iter()
        .find(|n| n.states.selected && ROW_ROLES.contains(&n.role) && n.id != own.id)
        .and_then(row_name)
        .and_then(clean)
    {
        return Some(("selected", t));
    }
    scope
        .iter()
        .filter(|n| n.role == Role::Heading)
        .find_map(|n| n.name.clone().or_else(|| n.value.clone()).and_then(clean))
        .map(|t| ("open", t))
}

/// Run a backend call on the blocking pool (backends talk to the OS synchronously).
pub(crate) async fn blocking<T, F>(shared: &Arc<Shared>, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Shared) -> Result<T> + Send + 'static,
{
    let s = shared.clone();
    match tokio::task::spawn_blocking(move || f(&s)).await {
        Ok(r) => r,
        Err(e) => Err(Error::Other(format!("computer-use task failed: {e}"))),
    }
}
