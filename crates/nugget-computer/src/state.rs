//! State shared by the tools: the backend, the configuration and the last snapshot of each
//! window, so `risk` and `describe` can look an element up by id without touching the OS.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use nugget_a11y::role::Role;
use nugget_a11y::{ComputerUse, ElementId, UiNode, UiTree, WindowId, WindowInfo, WindowSel};
use nugget_core::{Error, Result};

use crate::config::ComputerConfig;

/// Longest piece of dialog text kept as risk context, and how many pieces.
const CONTEXT_TEXT_MAX: usize = 200;
const CONTEXT_ITEMS_MAX: usize = 24;

pub(crate) struct Shared {
    pub cu: Arc<dyn ComputerUse>,
    pub config: ComputerConfig,
    /// Most recent first; one tree per window.
    cache: Mutex<VecDeque<UiTree>>,
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
            keyboard: Mutex::new(None),
            unclassified: Mutex::new(HashSet::new()),
        })
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
