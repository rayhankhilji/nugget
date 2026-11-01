//! State shared by the tools: the backend, the configuration and the last snapshot of each
//! window, so `risk` and `describe` can look an element up by id without touching the OS.

use std::collections::{HashSet, VecDeque};
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
    /// The window that last had keyboard focus as far as a snapshot showed: the one a
    /// `WindowSel::Focused` snapshot returned, or one the backend marked focused. Keyboard tools
    /// without an `element` are described and observed from it, not from whichever window was
    /// observed last.
    keyboard: Mutex<Option<WindowId>>,
    /// Element ids the latest risk classification could not find in the cache. `call` refuses
    /// them even if a later refresh shows them: what appeared after the gate was never
    /// classified. An id leaves the set when a classification finds it.
    unclassified: Mutex<HashSet<ElementId>>,
}

/// The fields a send or submit was described with, so `call` presses only if they still hold
/// exactly what the user approved.
#[derive(Debug, Clone)]
pub(crate) struct Frozen {
    /// The control that sends.
    pub element: ElementId,
    pub window: WindowId,
    /// What was shown, for naming what changed.
    pub fields: Vec<FieldValue>,
    /// [`FreezeView::digest`] at describe time.
    pub digest: String,
    /// The digest binds the item the action applies to (a Delete).
    pub with_subject: bool,
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
            keyboard: Mutex::new(None),
            unclassified: Mutex::new(HashSet::new()),
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

    /// The [`FreezeView`] of `id`, from the cached snapshots.
    pub fn freeze_view(
        &self,
        id: &ElementId,
        with_subject: bool,
    ) -> Option<(WindowId, FreezeView)> {
        let c = self.cache();
        c.iter()
            .find_map(|t| freeze_view(t, id, with_subject).map(|v| (t.window.id.clone(), v)))
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
        if tree.window.focused {
            self.set_keyboard(&tree.window.id);
        }
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

    /// Is `id` in a cached snapshot?
    pub fn knows(&self, id: &ElementId) -> bool {
        self.cache().iter().any(|t| t.find(id).is_some())
    }

    /// Record what a risk classification saw for an explicit element id: found in the cache,
    /// or not (see [`Shared::unclassified`]).
    pub fn note_classified(&self, id: &ElementId, found: bool) {
        let mut u = self.unclassified.lock().unwrap_or_else(|e| e.into_inner());
        if found {
            u.remove(id);
        } else {
            u.insert(id.clone());
        }
    }

    /// Was `id` unknown when it was last classified?
    pub fn was_unclassified(&self, id: &ElementId) -> bool {
        self.unclassified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }

    fn set_keyboard(&self, id: &WindowId) {
        *self.keyboard.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
    }

    /// The cached snapshot of the window that has keyboard focus (as last seen), else the most
    /// recent snapshot.
    fn keyboard_tree<R>(&self, f: impl FnOnce(&UiTree) -> Option<R>) -> Option<R> {
        let k = self
            .keyboard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let c = self.cache();
        let t = k
            .and_then(|k| c.iter().find(|t| t.window.id == k))
            .or_else(|| c.front())?;
        f(t)
    }

    /// The focused element of the keyboard window's snapshot, if it is known.
    pub fn focused(&self) -> Option<Found> {
        self.keyboard_tree(|t| found_in(t, t.focused.as_ref()?))
    }

    /// The window keyboard input goes to, as last seen (else the most recent snapshot's).
    pub fn keyboard_window(&self) -> Option<WindowInfo> {
        self.keyboard_tree(|t| Some(t.window.clone()))
    }

    /// The window of the most recent snapshot.
    pub fn latest_window(&self) -> Option<WindowInfo> {
        self.cache().front().map(|t| t.window.clone())
    }

    /// A fresh, bounded snapshot (blocking; call from a blocking task). Kept in the cache; a
    /// snapshot of the focused window also records it as the keyboard window.
    pub fn snapshot(&self, sel: &WindowSel) -> Result<UiTree> {
        let tree = self.cu.snapshot(sel, &self.config.tree_limits())?;
        if matches!(sel, WindowSel::Focused) {
            self.set_keyboard(&tree.window.id);
        }
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

/// What a control at the end of `path` sends: the nearest container around it that holds
/// fields (its form or pane), stopping at its dialog; the window when nothing nearer does. So a
/// page-controlled "To" field elsewhere in the window is not mixed in.
fn send_scope<'a>(tree: &'a UiTree, path: &[&'a UiNode]) -> &'a UiNode {
    for n in path.iter().rev().skip(1) {
        if n.role == Role::Dialog || n.iter().any(is_sent_field) {
            return n;
        }
    }
    &tree.root
}

/// Is `n` a field whose contents a send or submit carries? Search fields are not.
fn is_sent_field(n: &UiNode) -> bool {
    matches!(n.role, Role::TextField | Role::TextArea | Role::ComboBox)
        || (n.states.editable && matches!(n.role, Role::Document | Role::Unknown))
}

fn field_value(n: &UiNode) -> FieldValue {
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
}

/// The editable fields that activating `id` sends (see [`send_scope`]), all of them, in screen
/// order, with their full values. `None` when `id` is not in `tree`.
pub(crate) fn send_fields(tree: &UiTree, id: &ElementId) -> Option<Vec<FieldValue>> {
    let mut path = Vec::new();
    if !path_to(&tree.root, id, &mut path) {
        return None;
    }
    Some(
        send_scope(tree, &path)
            .iter()
            .filter(|n| is_sent_field(n))
            .map(field_value)
            .collect(),
    )
}

/// What a confirmation of an activation of `id` binds: the control itself (role, name and the
/// item it acts on) and the fields it sends.
#[derive(Debug, Clone)]
pub(crate) struct FreezeView {
    pub control: Vec<String>,
    pub fields: Vec<FieldValue>,
}

impl FreezeView {
    pub fn digest(&self) -> String {
        let control: Vec<&str> = self.control.iter().map(String::as_str).collect();
        nugget_policy::preview::freeze_digest(&control, &self.fields)
    }
}

/// The [`FreezeView`] of `id` in `tree`, `None` when it is not there.
/// `with_subject` also binds the item the action applies to ([`subject_item`]), for a Delete.
pub(crate) fn freeze_view(tree: &UiTree, id: &ElementId, with_subject: bool) -> Option<FreezeView> {
    let node = tree.find(id)?;
    let subject = with_subject
        .then(|| subject_item(tree, id))
        .flatten()
        .map(|(how, item)| format!("{how}: {item}"))
        .unwrap_or_default();
    Some(FreezeView {
        control: vec![
            node.role.as_str().to_string(),
            node.name.clone().unwrap_or_default(),
            subject,
        ],
        fields: send_fields(tree, id)?,
    })
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
const COLLECTION_ROLES: [Role; 5] = [
    Role::List,
    Role::ListItem,
    Role::Table,
    Role::Row,
    Role::Cell,
];

/// What an action on `id` (a Delete, say) applies to, for its confirmation:
/// - `"in"`: the row the control sits in (a per-row button);
/// - `"selected"`: the selected row of a list, table or tree in its dialog or window;
/// - `"open"`: the heading of its dialog, or of the window when the window clearly shows one
///   item (a single heading and no list or table: a message or document view).
///
/// The text is screen text, sanitized and bounded.
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
    if let Some(row) = path
        .iter()
        .rev()
        .skip(1)
        .find(|n| matches!(n.role, Role::ListItem | Role::Row))
        && let Some(t) = row_name(row).and_then(clean)
        && own.name.as_deref().is_none_or(|n| n.trim() != t)
    {
        return Some(("in", t));
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
    let heading = |n: &UiNode| n.name.clone().or_else(|| n.value.clone()).and_then(clean);
    let in_dialog = scope.role == Role::Dialog;
    let headings: Vec<&UiNode> = scope.iter().filter(|n| n.role == Role::Heading).collect();
    let item_view = in_dialog
        || (headings.len() == 1 && !scope.iter().any(|n| COLLECTION_ROLES.contains(&n.role)));
    if item_view {
        return headings
            .first()
            .and_then(|n| heading(n))
            .map(|t| ("open", t));
    }
    None
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
