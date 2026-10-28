//! Computer use over a scripted in-memory desktop: ids, policy, bounds and verification.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};

use super::*;
use crate::clipboard::Clipboard;
use crate::deny::DenyRule;
use crate::engine::{A11yConfig, Backend, FieldRead, Platform, PlatformWindow, Target};
use crate::tree::{Element, Node};
use crate::verify::FieldState;

// --- The fake desktop -------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FNode {
    uid: u64,
    node: Node,
    actions: Vec<UiActionKind>,
    children: Vec<FNode>,
}

impl FNode {
    fn find(&self, uid: u64) -> Option<&FNode> {
        if self.uid == uid {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(uid))
    }
    fn find_mut(&mut self, uid: u64) -> Option<&mut FNode> {
        if self.uid == uid {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(uid))
    }
    fn parent_of(&self, uid: u64) -> Option<u64> {
        for c in &self.children {
            if c.uid == uid {
                return Some(self.uid);
            }
            if let Some(p) = c.parent_of(uid) {
                return Some(p);
            }
        }
        None
    }
    fn walk_mut(&mut self, f: &mut dyn FnMut(&mut FNode)) {
        f(self);
        for c in &mut self.children {
            c.walk_mut(f);
        }
    }
    fn walk<'a>(&'a self, out: &mut Vec<&'a FNode>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

struct FWin {
    app: AppEntry,
    native: String,
    title: String,
    focused: bool,
    root: FNode,
}

type Hook = fn(&mut World, u64);

#[derive(Default)]
struct World {
    windows: Vec<FWin>,
    frontmost: String,
    next_uid: u64,
    /// Actions and key input are accepted but do nothing.
    inert: bool,
    performed: Vec<(u64, UiAction)>,
    typed: Vec<KeyInput>,
    opened: Vec<String>,
    /// Apps `open_app` can launch.
    installed: Vec<AppEntry>,
    press: HashMap<u64, Hook>,
}

impl World {
    fn find(&self, uid: u64) -> Option<&FNode> {
        self.windows.iter().find_map(|w| w.root.find(uid))
    }
    fn find_mut(&mut self, uid: u64) -> Option<&mut FNode> {
        self.windows.iter_mut().find_map(|w| w.root.find_mut(uid))
    }
    fn uid_named(&self, name: &str) -> u64 {
        let mut all = Vec::new();
        for w in &self.windows {
            w.root.walk(&mut all);
        }
        all.iter()
            .find(|n| n.node.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no node {name}"))
            .uid
    }
    fn focused_uid(&self) -> Option<u64> {
        let w = self.windows.iter().find(|w| w.focused)?;
        let mut all = Vec::new();
        w.root.walk(&mut all);
        all.iter()
            .rev()
            .find(|n| n.node.states.focused)
            .map(|n| n.uid)
    }
    fn set_focus(&mut self, uid: u64) {
        for w in &mut self.windows {
            w.root
                .walk_mut(&mut |n| n.node.states.focused = n.uid == uid);
        }
    }
    fn remove(&mut self, uid: u64) {
        for w in &mut self.windows {
            w.root
                .walk_mut(&mut |n| n.children.retain(|c| c.uid != uid));
        }
    }
    fn number(&mut self, n: &mut FNode) {
        self.next_uid += 1;
        n.uid = self.next_uid;
        for c in &mut n.children {
            self.number(c);
        }
    }
    /// Insert `node` as child `at` of `parent`.
    fn insert(&mut self, parent: u64, at: usize, mut node: FNode) -> u64 {
        self.number(&mut node);
        let uid = node.uid;
        let p = self.find_mut(parent).unwrap();
        p.children.insert(at.min(p.children.len()), node);
        uid
    }
}

#[derive(Clone)]
struct Fake(Arc<Mutex<World>>);

impl Fake {
    fn w(&self) -> MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }
}

struct FEl {
    w: Arc<Mutex<World>>,
    uid: u64,
}

impl FEl {
    fn w(&self) -> MutexGuard<'_, World> {
        self.w.lock().unwrap()
    }
}

impl Element for FEl {
    fn node(&self) -> Node {
        // A dead element reads as an empty node, as real backends tend to.
        self.w()
            .find(self.uid)
            .map(|n| n.node.clone())
            .unwrap_or_default()
    }
    fn children(&self, max: usize) -> Vec<Self> {
        let uids: Vec<u64> = self
            .w()
            .find(self.uid)
            .map(|n| n.children.iter().take(max).map(|c| c.uid).collect())
            .unwrap_or_default();
        uids.into_iter()
            .map(|uid| FEl {
                w: self.w.clone(),
                uid,
            })
            .collect()
    }
    fn parent(&self) -> Option<Self> {
        let p = self
            .w()
            .windows
            .iter()
            .find_map(|w| w.root.parent_of(self.uid))?;
        Some(FEl {
            w: self.w.clone(),
            uid: p,
        })
    }
    fn same(&self, other: &Self) -> bool {
        self.uid == other.uid
    }
    fn actions(&self) -> Vec<UiActionKind> {
        self.w()
            .find(self.uid)
            .map(|n| n.actions.clone())
            .unwrap_or_default()
    }
    fn perform(&self, action: &UiAction) -> Result<()> {
        let mut w = self.w();
        w.performed.push((self.uid, action.clone()));
        if w.find(self.uid).is_none() {
            return Err(Error::NotFound("element is gone".into()));
        }
        if w.inert {
            return Ok(());
        }
        if let UiAction::Press = action
            && let Some(hook) = w.press.get(&self.uid).copied()
        {
            hook(&mut w, self.uid);
            return Ok(());
        }
        if let UiAction::Focus = action {
            w.set_focus(self.uid);
            return Ok(());
        }
        let n = w.find_mut(self.uid).unwrap();
        match action {
            UiAction::Press => {
                if let Some(c) = n.node.states.checked {
                    n.node.states.checked = Some(!c);
                }
            }
            UiAction::SetValue { value } => n.node.value = Some(value.clone()),
            UiAction::Select => n.node.states.selected = true,
            UiAction::Expand { expand } => n.node.states.expanded = Some(*expand),
            UiAction::Scroll { amount, .. } => {
                for c in &mut n.children {
                    if let Some(b) = &mut c.node.bounds {
                        b.y -= 10.0 * f64::from(*amount);
                    }
                }
            }
            UiAction::Focus => unreachable!(),
        }
        Ok(())
    }
}

fn app_of(w: &FWin) -> AppInfo {
    AppInfo {
        id: w.app.id.clone(),
        name: w.app.name.clone(),
        window_title: Some(w.title.clone()),
        url: None,
    }
}

impl Platform for Fake {
    type Element = FEl;
    fn permission(&self) -> Permission {
        Permission::Granted
    }
    fn request_permission(&self) -> Result<()> {
        Ok(())
    }
    fn focus(&self) -> Result<Option<Target<FEl>>> {
        let w = self.w();
        let Some(uid) = w.focused_uid() else {
            return Ok(None);
        };
        let win = w.windows.iter().find(|w| w.focused).unwrap();
        Ok(Some(Target {
            app: app_of(win),
            element: FEl {
                w: self.0.clone(),
                uid,
            },
        }))
    }
    fn read_field(&self, el: &FEl, _max: usize) -> Result<FieldRead> {
        let node = el.node();
        Ok(FieldRead {
            state: FieldState {
                value: node.value.clone().unwrap_or_default(),
                selection: None,
                truncated: false,
            },
            node,
            label: None,
        })
    }
    fn caret_bounds(&self, _el: &FEl, _s: &FieldState) -> Option<Rect> {
        None
    }
    fn insert_text(&self, _el: &FEl, _t: &str, _b: &FieldState) -> Result<()> {
        Err(Error::Unsupported("not in these tests".into()))
    }
    fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
        Err(Error::Unsupported("not in these tests".into()))
    }
    fn paste(&self, _el: &FEl) -> Result<()> {
        Ok(())
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        let w = self.w();
        let mut out: Vec<AppEntry> = Vec::new();
        for win in &w.windows {
            if !out.iter().any(|a| a.name == win.app.name) {
                let mut a = win.app.clone();
                a.frontmost = a.name == w.frontmost;
                out.push(a);
            }
        }
        Ok(out)
    }
    fn windows(&self) -> Result<Vec<PlatformWindow<FEl>>> {
        let w = self.w();
        Ok(w.windows
            .iter()
            .map(|win| PlatformWindow {
                app: AppEntry {
                    frontmost: win.app.name == w.frontmost,
                    ..win.app.clone()
                },
                native_id: win.native.clone(),
                title: Some(win.title.clone()),
                bounds: None,
                focused: win.focused,
                minimized: false,
                element: FEl {
                    w: self.0.clone(),
                    uid: win.root.uid,
                },
            })
            .collect())
    }
    fn key_input(&self, input: &KeyInput) -> Result<()> {
        let mut w = self.w();
        w.typed.push(input.clone());
        if w.inert {
            return Ok(());
        }
        let Some(f) = w.focused_uid() else {
            return Ok(());
        };
        match input {
            KeyInput::Text(t) => {
                let n = w.find_mut(f).unwrap();
                let mut v = n.node.value.clone().unwrap_or_default();
                v.push_str(t);
                n.node.value = Some(v);
            }
            KeyInput::Keys(keys) => {
                for k in keys {
                    if k.key == Key::Named(NamedKey::Tab) {
                        // Next focusable element in the focused window.
                        let win = w.windows.iter().find(|w| w.focused).unwrap();
                        let mut all = Vec::new();
                        win.root.walk(&mut all);
                        let focusable: Vec<u64> = all
                            .iter()
                            .filter(|n| n.actions.contains(&UiActionKind::Focus))
                            .map(|n| n.uid)
                            .collect();
                        let i = focusable.iter().position(|&u| u == f);
                        let next = focusable[i.map_or(0, |i| (i + 1) % focusable.len())];
                        w.set_focus(next);
                    }
                }
            }
        }
        Ok(())
    }
    fn open_app(&self, name: &str) -> Result<()> {
        let mut w = self.w();
        w.opened.push(name.to_string());
        let Some(app) = w
            .installed
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
        else {
            return Err(Error::NotFound(format!("no app {name}")));
        };
        if w.inert {
            return Ok(());
        }
        if !w.windows.iter().any(|x| x.app.name == app.name) {
            let mut root = node(Role::Window, &app.name);
            w.number(&mut root);
            for x in &mut w.windows {
                x.focused = false;
            }
            w.windows.insert(
                0,
                FWin {
                    native: format!("win-{}", app.name),
                    title: app.name.clone(),
                    focused: true,
                    app: app.clone(),
                    root,
                },
            );
        }
        w.frontmost = app.name;
        Ok(())
    }
    fn screenshot(&self, window: &FEl) -> Result<Option<Vec<u8>>> {
        Ok(Some(vec![0x89, b'P', window.uid as u8]))
    }
}

// --- Fixtures ---------------------------------------------------------------------------------

fn node(role: Role, name: &str) -> FNode {
    FNode {
        node: Node {
            role,
            name: (!name.is_empty()).then(|| name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn with(mut n: FNode, actions: &[UiActionKind], f: impl FnOnce(&mut Node)) -> FNode {
    n.actions = actions.to_vec();
    f(&mut n.node);
    n
}

fn kids(mut n: FNode, children: Vec<FNode>) -> FNode {
    n.children = children;
    n
}

fn rect(y: f64) -> Option<Rect> {
    Some(Rect {
        x: 0.0,
        y,
        width: 100.0,
        height: 20.0,
    })
}

use UiActionKind as K;

fn editor_window() -> FNode {
    let toolbar = kids(
        node(Role::Toolbar, "Tools"),
        vec![
            with(node(Role::Button, "Save"), &[K::Press], |_| {}),
            with(node(Role::Button, "Save"), &[K::Press], |_| {}),
            with(node(Role::CheckBox, "Bold"), &[K::Press], |n| {
                n.states.checked = Some(false)
            }),
        ],
    );
    let title = with(
        node(Role::TextField, "Title"),
        &[K::SetValue, K::Focus],
        |n| {
            n.value = Some("draft".into());
            n.states.editable = true;
            n.states.focused = true;
            n.states.multiline = Some(false);
        },
    );
    let body = with(
        node(Role::TextArea, "Body"),
        &[K::SetValue, K::Focus],
        |n| {
            n.states.editable = true;
            n.states.multiline = Some(true);
        },
    );
    let password = kids(
        with(
            node(Role::TextField, "Password"),
            &[K::SetValue, K::Focus],
            |n| {
                n.value = Some("hunter2".into());
                n.states.secure = true;
                n.states.editable = true;
            },
        ),
        vec![with(node(Role::StaticText, "inner"), &[], |n| {
            n.value = Some("hunter2".into())
        })],
    );
    let items = (0..5)
        .map(|i| {
            let item = with(
                node(Role::ListItem, &format!("Item {i}")),
                &[K::Select],
                |n| {
                    n.bounds = rect(f64::from(i) * 20.0);
                    n.states.offscreen = i == 4;
                },
            );
            kids(item, vec![node(Role::StaticText, &format!("detail {i}"))])
        })
        .collect();
    let list = kids(
        with(node(Role::List, "Items"), &[K::Scroll], |n| {
            n.bounds = rect(0.0)
        }),
        items,
    );
    let folder = with(node(Role::Group, "Folder"), &[K::Expand], |n| {
        n.states.expanded = Some(false)
    });
    let disabled = with(node(Role::Button, "Disabled"), &[K::Press], |n| {
        n.states.disabled = true
    });
    let status = with(node(Role::StaticText, "Status"), &[], |n| {
        n.value = Some("Ready".into())
    });
    kids(
        node(Role::Window, "Editor"),
        vec![
            toolbar,
            title,
            body,
            password,
            list,
            folder,
            disabled,
            with(node(Role::Button, "Noop"), &[K::Press], |_| {}),
            with(node(Role::Button, "Close"), &[K::Press], |_| {}),
            with(node(Role::Button, "Apply"), &[K::Press], |_| {}),
            status,
        ],
    )
}

fn app(name: &str, id: &str) -> AppEntry {
    AppEntry {
        id: Some(id.into()),
        name: name.into(),
        pid: Some(1),
        frontmost: false,
    }
}

fn world() -> Fake {
    let mut w = World {
        frontmost: "Editor".into(),
        installed: vec![
            app("Calculator", "org.example.calc"),
            app("Editor", "org.example.editor"),
        ],
        ..Default::default()
    };
    let mut roots = vec![
        editor_window(),
        kids(
            node(Role::Window, "Vault"),
            vec![node(Role::Button, "Unlock")],
        ),
        kids(
            node(Role::Window, "Notes"),
            vec![with(node(Role::Button, "New"), &[K::Press], |_| {})],
        ),
    ];
    for r in &mut roots {
        w.number(r);
    }
    let [editor, vault, notes] = <[FNode; 3]>::try_from(roots).ok().unwrap();
    w.windows = vec![
        FWin {
            app: app("Notes", "org.example.notes"),
            native: "win-notes".into(),
            title: "Notes".into(),
            focused: false,
            root: notes,
        },
        FWin {
            app: app("Editor", "org.example.editor"),
            native: "win-editor".into(),
            title: "Untitled".into(),
            focused: true,
            root: editor,
        },
        FWin {
            app: app("1Password", "com.1password.1password"),
            native: "win-vault".into(),
            title: "Vault".into(),
            focused: false,
            root: vault,
        },
    ];
    // Save sets the status line; Close removes itself; Apply changes nothing visible but the
    // status of a sibling (window change).
    let save = w.uid_named("Save");
    w.press.insert(save, |w, _| {
        let s = w.uid_named("Status");
        w.find_mut(s).unwrap().node.value = Some("Saved".into());
    });
    let close = w.uid_named("Close");
    w.press.insert(close, |w, uid| w.remove(uid));
    let apply = w.uid_named("Apply");
    w.press.insert(apply, |w, _| {
        let f = w.uid_named("Folder");
        w.find_mut(f).unwrap().node.description = Some("applied".into());
        let s = w.uid_named("Status");
        w.find_mut(s).unwrap().node.value = Some("Applied".into());
    });
    Fake(Arc::new(Mutex::new(w)))
}

fn config() -> A11yConfig {
    A11yConfig {
        verify_timeout: Duration::from_millis(30),
        ..Default::default()
    }
}

fn backend() -> (Backend<Fake>, Fake) {
    let f = world();
    (Backend::new(f.clone(), config()), f)
}

fn editor(b: &Backend<Fake>) -> UiTree {
    ComputerUse::snapshot(b, &WindowSel::Focused, &TreeLimits::default()).unwrap()
}

fn id_of(t: &UiTree, name: &str) -> ElementId {
    t.root
        .iter()
        .find(|n| n.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no {name} in tree"))
        .id
        .clone()
}

fn ids_of(t: &UiTree, name: &str) -> Vec<ElementId> {
    t.root
        .iter()
        .filter(|n| n.name.as_deref() == Some(name))
        .map(|n| n.id.clone())
        .collect()
}

// --- Ids --------------------------------------------------------------------------------------

#[test]
fn element_ids_are_pinned_and_short() {
    let path = [IdKey {
        role: "button".into(),
        label: "Save".into(),
        ordinal: 0,
    }];
    let a = element_id("win-1", &path);
    assert_eq!(a.as_str().len(), 8);
    assert!(a.as_str().starts_with('e'));
    assert_eq!(a, element_id("win-1", &path));
    assert_ne!(a, element_id("win-2", &path));
    assert_ne!(a, element_id("win-1", &[]));
    // Pinned: ids must not change between builds or toolchains.
    assert_eq!(element_id("w", &[]).as_str(), "ekiy9ipw");
    assert_eq!(window_id("w").as_str(), "wfnrzxef");
}

#[test]
fn ids_are_stable_across_snapshots() {
    let (b, _) = backend();
    let t1 = editor(&b);
    let t2 = editor(&b);
    assert_eq!(t1, t2);
    let all: Vec<_> = t1.root.iter().map(|n| n.id.clone()).collect();
    let mut dedup = all.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(all.len(), dedup.len(), "ids are unique");
    assert_eq!(t1.app.name, "Editor");
    assert_eq!(t1.window.title.as_deref(), Some("Untitled"));
    assert!(t1.window.focused);
}

#[test]
fn ids_survive_unrelated_insertions() {
    let (b, f) = backend();
    let t1 = editor(&b);
    let title = id_of(&t1, "Title");
    let status = id_of(&t1, "Status");
    {
        let mut w = f.w();
        let win = w.uid_named("Editor");
        // A new sibling after Title, and a differently-labelled one before it.
        w.insert(win, 2, node(Role::Button, "Inserted"));
        w.insert(win, 0, node(Role::TextField, "Search"));
    }
    let t2 = editor(&b);
    assert_eq!(id_of(&t2, "Title"), title);
    assert_eq!(id_of(&t2, "Status"), status);
    b.act(&title, &UiAction::Focus).unwrap();
}

#[test]
fn duplicate_labels_get_ordinals() {
    let (b, f) = backend();
    let t = editor(&b);
    let saves = ids_of(&t, "Save");
    assert_eq!(saves.len(), 2);
    assert_ne!(saves[0], saves[1]);
    b.act(&saves[1], &UiAction::Press).unwrap();
    let w = f.w();
    let mut all = Vec::new();
    w.windows[1].root.children[0].walk(&mut all);
    let second = all
        .iter()
        .filter(|n| n.node.name.as_deref() == Some("Save"))
        .nth(1);
    assert_eq!(w.performed[0].0, second.unwrap().uid);
}

#[test]
fn identifier_beats_name() {
    let (b, f) = backend();
    {
        let mut w = f.w();
        let s = w.uid_named("Status");
        let n = w.find_mut(s).unwrap();
        n.node
            .attrs
            .insert("identifier".into(), "status-line".into());
    }
    let t1 = editor(&b);
    let id = id_of(&t1, "Status");
    {
        let mut w = f.w();
        let s = w.uid_named("Status");
        w.find_mut(s).unwrap().node.name = Some("Status bar".into());
    }
    let t2 = editor(&b);
    assert_eq!(id_of(&t2, "Status bar"), id);
}

#[test]
fn stale_and_unknown_ids_are_not_found() {
    let (b, f) = backend();
    let t = editor(&b);
    let noop = id_of(&t, "Noop");
    {
        let mut w = f.w();
        let u = w.uid_named("Noop");
        w.remove(u);
    }
    let e = b.act(&noop, &UiAction::Press).unwrap_err();
    assert!(
        matches!(&e, Error::NotFound(m) if m.contains("stale")),
        "{e}"
    );
    let e = b
        .act(&ElementId("e0000000".into()), &UiAction::Press)
        .unwrap_err();
    assert!(matches!(e, Error::NotFound(_)));
    assert!(f.w().performed.is_empty());
}

#[test]
fn renamed_element_is_stale_not_confused() {
    let (b, f) = backend();
    let t = editor(&b);
    let save1 = ids_of(&t, "Save")[1].clone();
    {
        let mut w = f.w();
        let first = w.uid_named("Save");
        w.remove(first);
    }
    // The second Save is now the first: its ordinal changed, so the old id must not resolve to
    // a different element.
    let saves = ids_of(&editor(&b), "Save");
    assert_eq!(saves.len(), 1);
    assert_ne!(saves[0], save1);
    assert!(matches!(
        b.act(&save1, &UiAction::Press),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn cache_is_bounded() {
    let (b, _) = backend();
    let notes =
        ComputerUse::snapshot(&b, &WindowSel::App("notes".into()), &TreeLimits::default()).unwrap();
    let new = id_of(&notes, "New");
    for _ in 0..CACHED {
        editor(&b);
    }
    assert!(matches!(
        b.act(&new, &UiAction::Press),
        Err(Error::NotFound(_))
    ));
    let notes = ComputerUse::snapshot(
        &b,
        &WindowSel::App("org.example.notes".into()),
        &TreeLimits::default(),
    )
    .unwrap();
    assert!(b.act(&id_of(&notes, "New"), &UiAction::Focus).is_ok());
}

const CACHED: usize = backend::CACHED_SNAPSHOTS;

// --- Enumeration and policy -------------------------------------------------------------------

#[test]
fn denied_apps_are_hidden_and_refused() {
    let (b, f) = backend();
    let apps = b.apps().unwrap();
    assert_eq!(
        apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["Editor", "Notes"]
    );
    assert!(apps[0].frontmost);
    let wins = b.windows().unwrap();
    assert_eq!(wins.len(), 2);
    assert_eq!(wins[0].app.name, "Editor", "focused window first");
    assert!(wins.iter().all(|w| w.id.as_str().starts_with('w')));

    let e = ComputerUse::snapshot(
        &b,
        &WindowSel::App("1password".into()),
        &TreeLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "{e}");
    let e = ComputerUse::snapshot(
        &b,
        &WindowSel::Id(window_id("win-vault")),
        &TreeLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(e, Error::Invalid(_)));
    assert!(matches!(
        b.screenshot(&WindowSel::Id(window_id("win-vault"))),
        Err(Error::Invalid(_))
    ));

    // An app that becomes denied after its snapshot cannot be acted on.
    let notes = ComputerUse::snapshot(
        &b,
        &WindowSel::Id(wins[1].id.clone()),
        &TreeLimits::default(),
    )
    .unwrap();
    f.w().windows[0].app.name = "1Password Notes".into();
    assert!(matches!(
        b.act(&id_of(&notes, "New"), &UiAction::Press),
        Err(Error::Invalid(_))
    ));
    assert!(f.w().performed.is_empty());
}

#[test]
fn user_deny_rules_apply() {
    let f = world();
    let mut cfg = config();
    cfg.deny
        .push(DenyRule::parse("id:org.example.notes").unwrap());
    let b = Backend::new(f, cfg);
    assert_eq!(b.apps().unwrap().len(), 1);
    assert!(matches!(
        ComputerUse::snapshot(&b, &WindowSel::App("Notes".into()), &TreeLimits::default()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn denied_web_host_refuses_snapshot() {
    let (b, f) = backend();
    {
        let mut w = f.w();
        let body = w.uid_named("Body");
        let n = w.find_mut(body).unwrap();
        n.node.attrs.insert(
            "url".into(),
            "https://user:pw@vault.bitwarden.com/#/vault".into(),
        );
    }
    assert!(matches!(
        ComputerUse::snapshot(&b, &WindowSel::Focused, &TreeLimits::default()),
        Err(Error::Invalid(_))
    ));
    {
        let mut w = f.w();
        let body = w.uid_named("Body");
        let n = w.find_mut(body).unwrap();
        n.node
            .attrs
            .insert("url".into(), "https://user:pw@example.com/a".into());
    }
    let t = editor(&b);
    assert_eq!(t.app.url.as_deref(), Some("https://example.com/a"));
}

#[test]
fn missing_window_is_not_found() {
    let (b, _) = backend();
    for sel in [
        WindowSel::App("Nope".into()),
        WindowSel::Id(window_id("win-gone")),
    ] {
        assert!(matches!(
            ComputerUse::snapshot(&b, &sel, &TreeLimits::default()),
            Err(Error::NotFound(_))
        ));
    }
}

#[test]
fn secure_values_never_leave_and_cannot_be_set() {
    let (b, f) = backend();
    let t = editor(&b);
    let json = serde_json::to_string(&t).unwrap();
    assert!(!json.contains("hunter2"), "{json}");
    let pw = t.find(&id_of(&t, "Password")).unwrap();
    assert!(pw.states.secure);
    assert_eq!(pw.value, None);
    assert!(pw.children.is_empty(), "secure elements are not descended");

    let e = b
        .act(
            &pw.id,
            &UiAction::SetValue {
                value: "guess".into(),
            },
        )
        .unwrap_err();
    assert!(matches!(e, Error::Invalid(_)));
    assert!(f.w().performed.is_empty());

    // Pressing or focusing it is fine, and the outcome carries no value either.
    let out = b.act(&pw.id, &UiAction::Focus).unwrap();
    assert!(out.verified);
    assert_eq!(out.after.unwrap().value, None);

    // Typing into it is refused; named keys are allowed.
    let e = b
        .key_input(None, &KeyInput::Text("guess".into()))
        .unwrap_err();
    assert!(matches!(e, Error::Invalid(_)));
    let e = b
        .key_input(Some(&pw.id), &KeyInput::Text("guess".into()))
        .unwrap_err();
    assert!(matches!(e, Error::Invalid(_)));
    assert!(f.w().typed.is_empty());
    b.key_input(None, &KeyInput::Keys(vec![KeyChord::parse("tab").unwrap()]))
        .unwrap();
}

#[test]
fn strings_are_sanitised() {
    let (b, f) = backend();
    {
        let mut w = f.w();
        let s = w.uid_named("Status");
        w.find_mut(s).unwrap().node.value = Some("Ready\u{202E}\u{0007} now".into());
        w.windows[1].title = "Untitled\u{202E}".into();
    }
    let t = editor(&b);
    let s = t.find(&id_of(&t, "Status")).unwrap();
    assert_eq!(s.value.as_deref(), Some("Ready now"));
    assert_eq!(t.window.title.as_deref(), Some("Untitled"));
}

// --- Bounds -----------------------------------------------------------------------------------

#[test]
fn default_walk_is_complete_and_records_focus() {
    let (b, _) = backend();
    let t = editor(&b);
    assert!(!t.truncated);
    assert_eq!(t.focused, Some(id_of(&t, "Title")));
    let save = t.find(&id_of(&t, "Bold")).unwrap();
    assert_eq!(save.actions, vec![UiActionKind::Press]);
    assert_eq!(save.states.checked, Some(false));
}

#[test]
fn node_budget_truncates() {
    let (b, _) = backend();
    let limits = TreeLimits {
        max_nodes: 5,
        ..Default::default()
    };
    let t = ComputerUse::snapshot(&b, &WindowSel::Focused, &limits).unwrap();
    assert!(t.truncated);
    assert_eq!(t.count(), 5);
}

#[test]
fn depth_and_children_limits_truncate() {
    let (b, _) = backend();
    let t = ComputerUse::snapshot(
        &b,
        &WindowSel::Focused,
        &TreeLimits {
            max_depth: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(t.truncated);
    assert!(t.root.children.iter().all(|c| c.children.is_empty()));
    assert_eq!(t.root.children.len(), 11);

    let t2 = ComputerUse::snapshot(
        &b,
        &WindowSel::Focused,
        &TreeLimits {
            max_children: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(t2.truncated);
    assert_eq!(t2.root.children.len(), 2);
    // Ids do not depend on the limits.
    assert_eq!(id_of(&t2, "Save"), ids_of(&editor(&b), "Save")[0]);

    let t3 = ComputerUse::snapshot(
        &b,
        &WindowSel::Focused,
        &TreeLimits {
            deadline: Duration::ZERO,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(t3.truncated);
    assert_eq!(t3.count(), 1);
}

#[test]
fn offscreen_subtrees_are_skipped() {
    let (b, _) = backend();
    let t = editor(&b);
    let item4 = t.find(&id_of(&t, "Item 4")).unwrap();
    assert!(item4.states.offscreen);
    assert!(item4.children.is_empty());
    assert_eq!(t.find(&id_of(&t, "Item 3")).unwrap().children.len(), 1);
    assert!(!t.truncated);

    let all = ComputerUse::snapshot(
        &b,
        &WindowSel::Focused,
        &TreeLimits {
            skip_offscreen: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.find(&item4.id).unwrap().children.len(), 1);
}

// --- Actions ----------------------------------------------------------------------------------

fn inert(f: &Fake) {
    f.w().inert = true;
}

#[test]
fn set_value_is_verified() {
    let (b, f) = backend();
    let t = editor(&b);
    let id = id_of(&t, "Title");
    let out = b
        .act(
            &id,
            &UiAction::SetValue {
                value: "New\r title".into(),
            },
        )
        .unwrap();
    assert!(out.verified, "{}", out.detail);
    assert_eq!(out.before.unwrap().value.as_deref(), Some("draft"));
    let after = out.after.unwrap();
    assert_eq!(after.value.as_deref(), Some("New title"));
    assert_eq!(after.id, id);
    assert!(after.children.is_empty());
    {
        let w = f.w();
        let expected = UiAction::SetValue {
            value: "New title".into(),
        };
        assert_eq!(w.performed, vec![(w.uid_named("Title"), expected)]);
    }

    inert(&f);
    let out = b
        .act(
            &id,
            &UiAction::SetValue {
                value: "other".into(),
            },
        )
        .unwrap();
    assert!(!out.verified);
    assert_eq!(f.w().performed.len(), 2, "never retried");
}

#[test]
fn select_focus_expand_are_verified() {
    let cases = [
        ("Item 1", UiAction::Select),
        ("Body", UiAction::Focus),
        ("Folder", UiAction::Expand { expand: true }),
    ];
    for (name, action) in cases.clone() {
        let (b, _) = backend();
        let t = editor(&b);
        let out = b.act(&id_of(&t, name), &action).unwrap();
        assert!(out.verified, "{name}: {}", out.detail);
    }
    for (name, action) in cases {
        let (b, f) = backend();
        let t = editor(&b);
        inert(&f);
        let out = b.act(&id_of(&t, name), &action).unwrap();
        assert!(!out.verified, "{name}");
        assert!(!out.detail.is_empty());
        assert_eq!(f.w().performed.len(), 1);
    }
}

#[test]
fn collapse_is_checked_against_the_requested_state() {
    let (b, _) = backend();
    let t = editor(&b);
    let out = b
        .act(&id_of(&t, "Folder"), &UiAction::Expand { expand: false })
        .unwrap();
    // Already collapsed: the requested state holds.
    assert!(out.verified);
}

#[test]
fn press_is_verified_by_visible_change() {
    let (b, f) = backend();
    let t = editor(&b);
    // The element's own state.
    let out = b.act(&id_of(&t, "Bold"), &UiAction::Press).unwrap();
    assert!(out.verified, "{}", out.detail);
    assert!(out.detail.contains("checked"));
    assert_eq!(out.after.unwrap().states.checked, Some(true));
    // Something else in the window.
    let out = b.act(&ids_of(&t, "Save")[0], &UiAction::Press).unwrap();
    assert!(out.verified, "{}", out.detail);
    assert!(out.detail.contains("window"), "{}", out.detail);
    // The element went away.
    let out = b.act(&id_of(&t, "Close"), &UiAction::Press).unwrap();
    assert!(out.verified, "{}", out.detail);
    // Nothing at all.
    let n = f.w().performed.len();
    let out = b.act(&id_of(&t, "Noop"), &UiAction::Press).unwrap();
    assert!(!out.verified);
    assert_eq!(out.detail, "nothing visibly changed");
    assert_eq!(f.w().performed.len(), n + 1, "never retried");
}

#[test]
fn press_in_inert_app_is_unverified() {
    let (b, f) = backend();
    let t = editor(&b);
    inert(&f);
    for name in ["Bold", "Close", "Apply"] {
        let out = b.act(&id_of(&t, name), &UiAction::Press).unwrap();
        assert!(!out.verified, "{name}");
    }
}

#[test]
fn disabled_elements_are_refused() {
    let (b, f) = backend();
    let t = editor(&b);
    let e = b.act(&id_of(&t, "Disabled"), &UiAction::Press).unwrap_err();
    assert!(matches!(e, Error::Invalid(_)));
    assert!(f.w().performed.is_empty());
}

#[test]
fn scroll_is_verified_by_movement() {
    let (b, f) = backend();
    let t = editor(&b);
    let list = id_of(&t, "Items");
    let down = UiAction::Scroll {
        direction: ScrollDirection::Down,
        amount: 3,
    };
    let out = b.act(&list, &down).unwrap();
    assert!(out.verified, "{}", out.detail);
    inert(&f);
    let out = b.act(&list, &down).unwrap();
    assert!(!out.verified);
}

#[test]
fn window_root_is_addressable() {
    let (b, f) = backend();
    let t = editor(&b);
    let out = b.act(&t.root.id, &UiAction::Focus).unwrap();
    assert!(out.verified);
    let w = f.w();
    assert_eq!(w.performed[0].0, w.uid_named("Editor"));
}

// --- Keys -------------------------------------------------------------------------------------

#[test]
fn key_chords_parse_and_round_trip() {
    let cases = [
        ("enter", "enter"),
        ("Return", "enter"),
        ("esc", "escape"),
        ("a", "a"),
        ("F5", "f5"),
        ("f12", "f12"),
        ("ctrl+shift+tab", "ctrl+shift+tab"),
        ("shift+ctrl+Tab", "ctrl+shift+tab"),
        ("cmd+z", "meta+z"),
        ("command+shift+z", "shift+meta+z"),
        ("super+a", "meta+a"),
        ("win+d", "meta+d"),
        ("option+left", "alt+left"),
        ("pgup", "page_up"),
        ("PageDown", "page_down"),
        ("arrowup", "up"),
        ("ctrl+A", "ctrl+a"),
        ("ctrl++", "ctrl+plus"),
        ("ctrl+plus", "ctrl+plus"),
        ("+", "plus"),
        ("ctrl+comma", "ctrl+comma"),
        ("del", "delete"),
        ("space", "space"),
    ];
    for (input, canon) in cases {
        let c = KeyChord::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
        assert_eq!(c.to_string(), canon, "{input}");
        assert_eq!(KeyChord::parse(canon).unwrap(), c, "{canon}");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<KeyChord>(&json).unwrap(), c);
    }
    let c: KeyChord = "ctrl+alt+shift+meta+f1".parse().unwrap();
    assert!(c.modifiers.ctrl && c.modifiers.alt && c.modifiers.shift && c.modifiers.meta);
    assert_eq!(c.key, Key::Named(NamedKey::F(1)));
}

#[test]
fn bad_key_names_are_refused() {
    for bad in [
        "",
        "  ",
        "ctrl",
        "ctrl+",
        "hyper+a",
        "ctrl+ctrl+a",
        "enterr",
        "f0",
        "f13",
        "f05",
        "ab",
        "ctrl+shift",
        "\u{7}",
    ] {
        assert!(
            matches!(KeyChord::parse(bad), Err(Error::Invalid(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn key_lists_parse() {
    let keys = KeyChord::parse_list("ctrl+a, backspace").unwrap();
    assert_eq!(
        keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["ctrl+a", "backspace"]
    );
    assert_eq!(KeyChord::parse_list("tab tab enter").unwrap().len(), 3);
    assert!(KeyChord::parse_list(" , ").is_err());
    assert!(KeyChord::parse_list("tab, nope").is_err());
}

#[test]
fn typed_text_is_verified() {
    let (b, f) = backend();
    let out = b
        .key_input(None, &KeyInput::Text(" more\r\ntext\u{1b}".into()))
        .unwrap();
    assert!(out.verified, "{}", out.detail);
    // Single-line field: the newline is dropped, as are \r and escape.
    assert_eq!(f.w().typed, vec![KeyInput::Text(" moretext".into())]);
    let after = out.after.unwrap();
    assert_eq!(after.value.as_deref(), Some("draft moretext"));
    // The focused element was located and given its snapshot id.
    let t = editor(&b);
    assert_eq!(after.id, id_of(&t, "Title"));

    inert(&f);
    let out = b.key_input(None, &KeyInput::Text("zzz".into())).unwrap();
    assert!(!out.verified);
}

#[test]
fn typing_into_a_target_focuses_it_first() {
    let (b, f) = backend();
    let t = editor(&b);
    let body = id_of(&t, "Body");
    let out = b
        .key_input(Some(&body), &KeyInput::Text("line 1\nline 2".into()))
        .unwrap();
    assert!(out.verified, "{}", out.detail);
    assert_eq!(out.after.unwrap().value.as_deref(), Some("line 1\nline 2"));
    let w = f.w();
    assert_eq!(w.performed, vec![(w.uid_named("Body"), UiAction::Focus)]);
    assert_eq!(w.typed, vec![KeyInput::Text("line 1\nline 2".into())]);
}

#[test]
fn target_that_cannot_be_focused_gets_nothing() {
    let (b, f) = backend();
    let t = editor(&b);
    inert(&f);
    let out = b
        .key_input(Some(&id_of(&t, "Body")), &KeyInput::Text("x".into()))
        .unwrap();
    assert!(!out.verified);
    assert!(out.detail.contains("nothing was typed"));
    assert!(f.w().typed.is_empty());
}

#[test]
fn named_keys_are_verified_by_change() {
    let (b, f) = backend();
    let keys = KeyInput::Keys(KeyChord::parse_list("tab").unwrap());
    let out = b.key_input(None, &keys).unwrap();
    assert!(out.verified, "{}", out.detail);
    let t = editor(&b);
    assert_eq!(t.focused, Some(id_of(&t, "Body")));

    inert(&f);
    let out = b.key_input(None, &keys).unwrap();
    assert!(!out.verified);
    assert!(matches!(
        b.key_input(None, &KeyInput::Keys(Vec::new())),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn key_input_policy() {
    let (b, f) = backend();
    // Nothing to type after cleaning.
    assert!(matches!(
        b.key_input(None, &KeyInput::Text("\r\u{7}".into())),
        Err(Error::Invalid(_))
    ));
    // Nothing focused: text has nowhere to go.
    {
        let mut w = f.w();
        w.set_focus(0);
    }
    assert!(matches!(
        b.key_input(None, &KeyInput::Text("x".into())),
        Err(Error::NotFound(_))
    ));
    // Denied frontmost app.
    {
        let mut w = f.w();
        for x in &mut w.windows {
            x.focused = x.native == "win-vault";
        }
        let u = w.uid_named("Unlock");
        w.set_focus(u);
        w.frontmost = "1Password".into();
    }
    assert!(matches!(
        b.key_input(
            None,
            &KeyInput::Keys(KeyChord::parse_list("enter").unwrap())
        ),
        Err(Error::Invalid(_))
    ));
    assert!(f.w().typed.is_empty());
}

// --- Apps -------------------------------------------------------------------------------------

#[test]
fn open_app_is_verified() {
    let (b, f) = backend();
    let out = b.open_app("calculator").unwrap();
    assert!(out.verified, "{}", out.detail);
    assert_eq!(b.apps().unwrap()[0].name, "Calculator");
    assert_eq!(b.windows().unwrap()[0].app.name, "Calculator");

    // Already running: activated.
    let out = b.open_app("org.example.editor").unwrap_err();
    assert!(
        matches!(out, Error::NotFound(_)),
        "the fake launches by name only"
    );
    let out = b.open_app("Editor").unwrap();
    assert!(out.verified, "{}", out.detail);
    assert_eq!(b.apps().unwrap()[0].name, "Editor");
    assert!(b.open_app("nope").is_err());
    assert_eq!(
        f.w().opened,
        ["calculator", "org.example.editor", "Editor", "nope"]
    );
}

#[test]
fn open_app_unverified_and_denied() {
    let (b, f) = backend();
    assert!(matches!(b.open_app("1Password"), Err(Error::Invalid(_))));
    assert!(matches!(
        b.open_app("com.agilebits.onepassword7"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(b.open_app("  "), Err(Error::Invalid(_))));
    assert!(f.w().opened.is_empty());
    inert(&f);
    let out = b.open_app("Calculator").unwrap();
    assert!(!out.verified);
    assert!(out.detail.contains("did not appear"), "{}", out.detail);
}

#[test]
fn screenshot_is_delegated() {
    let (b, f) = backend();
    let png = b.screenshot(&WindowSel::Focused).unwrap().unwrap();
    assert_eq!(png[..2], [0x89, b'P']);
    assert_eq!(u64::from(png[2]), f.w().uid_named("Editor"));
}

#[test]
fn computer_use_is_object_safe() {
    let (b, _) = backend();
    let dyn_cu: Arc<dyn ComputerUse> = Arc::new(b);
    assert_eq!(dyn_cu.permission(), Permission::Granted);
    assert!(
        dyn_cu
            .snapshot(&WindowSel::Focused, &TreeLimits::default())
            .is_ok()
    );
}
