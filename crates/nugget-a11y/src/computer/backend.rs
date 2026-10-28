//! [`ComputerUse`](super::ComputerUse) for [`Backend`](crate::Backend): policy, id resolution and
//! read-back verification over any [`Platform`](crate::Platform).
//!
//! - Snapshots walk the window element depth-first within [`TreeLimits`] and remember, for each
//!   id they hand out, the window's native id and the [`IdKey`] path to the element ([`Cache`],
//!   the last [`CACHED_SNAPSHOTS`] snapshots).
//! - An id is resolved by finding its window again and matching the path child by child, so an
//!   element that moved, was renamed or vanished is reported stale instead of being confused
//!   with a neighbour.
//! - Every action is checked against policy first (deny list, secure, disabled), performed once
//!   (never retried), then the element and its window are re-read until the intended effect
//!   shows or `verify_timeout` passes.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};

use super::ids::{Fnv, IdKey, Ordinals, element_id, window_id};
use super::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, KeyInput, TreeLimits, UiAction, UiNode,
    UiTree, WindowInfo, WindowSel,
};
use crate::engine::{Backend, Platform, PlatformWindow};
use crate::role::Role;
use crate::sanitize;
use crate::tree::{Element, Node};
use crate::{Permission, Rect};

/// How many snapshots' ids stay resolvable.
pub const CACHED_SNAPSHOTS: usize = 8;
/// Children read per element while resolving an id.
const RESOLVE_CHILDREN: usize = 4096;
/// Nodes hashed for a window fingerprint (press and key verification).
const FINGERPRINT_NODES: usize = 400;
const FINGERPRINT_DEPTH: usize = 30;
const POLL: Duration = Duration::from_millis(25);
/// Ancestors climbed when giving the focused element an id.
const MAX_LOCATE_DEPTH: usize = 64;

#[derive(Debug, Clone)]
struct Entry {
    window: String,
    path: Vec<IdKey>,
}

/// Where the ids of recent snapshots point. Held by [`Backend`]; bounded.
#[derive(Debug, Default)]
pub struct Cache {
    snapshots: Mutex<VecDeque<HashMap<ElementId, Entry>>>,
}

impl Cache {
    fn push(&self, map: HashMap<ElementId, Entry>) {
        let mut s = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        s.push_front(map);
        s.truncate(CACHED_SNAPSHOTS);
    }

    /// Remember one more id in the newest snapshot (the focused element found by key input).
    fn add(&self, id: ElementId, entry: Entry) {
        let mut s = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        if s.is_empty() {
            s.push_front(HashMap::new());
        }
        if let Some(m) = s.front_mut() {
            m.insert(id, entry);
        }
    }

    fn get(&self, id: &ElementId) -> Option<Entry> {
        let s = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        s.iter().find_map(|m| m.get(id).cloned())
    }
}

/// A shallow, sanitised read of one element.
fn read<E: Element>(el: &E) -> Node {
    let mut n = el.node();
    n.children.clear();
    n.sanitize();
    n
}

fn ui_node<E: Element>(id: ElementId, node: &Node, el: &E) -> UiNode {
    let mut actions = el.actions();
    actions.sort();
    actions.dedup();
    UiNode {
        id,
        role: node.role,
        raw_role: node.raw_role.clone(),
        name: node.name.clone(),
        value: if node.states.secure {
            None
        } else {
            node.value.clone()
        },
        description: node.description.clone(),
        placeholder: node.placeholder.clone(),
        bounds: node.bounds,
        states: node.states,
        actions,
        children: Vec::new(),
    }
}

fn clean_app(mut a: AppEntry) -> AppEntry {
    a.id = sanitize::clean_opt(a.id.as_deref(), sanitize::MAX_SHORT);
    a.name = sanitize::clean_line(&a.name, sanitize::MAX_SHORT);
    a
}

fn app_info(app: &AppEntry, title: Option<&str>) -> AppInfo {
    AppInfo {
        id: app.id.clone(),
        name: app.name.clone(),
        window_title: title.map(str::to_string),
        url: None,
    }
}

fn matches_app(app: &AppEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return false;
    }
    let name = app.name.to_lowercase();
    let id = app.id.as_deref().unwrap_or("").to_lowercase();
    name == q
        || (!id.is_empty()
            && (id == q
                || id.trim_end_matches(".desktop") == q
                || id.ends_with(&format!(".{q}"))
                || id.trim_end_matches(".exe") == q))
}

/// Only web URLs, without credentials (used for host deny rules, never shown).
fn web_url(raw: &str) -> Option<String> {
    let u = sanitize::clean_line(raw, 2048);
    let (scheme, rest) = u.split_once("://")?;
    if !matches!(scheme.to_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    Some(format!("{scheme}://{host}{tail}"))
}

/// Whitespace-insensitive comparison form.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text we type or set: no `\r`, no control characters except `\n` and `\t`.
fn clean_input(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

fn single_line(n: &Node) -> bool {
    match n.states.multiline {
        Some(m) => !m,
        None => matches!(n.role, Role::TextField | Role::SearchField | Role::ComboBox),
    }
}

fn hash_node(h: &mut Fnv, n: &Node, with_bounds: bool) {
    h.str(n.role.as_str());
    h.str(n.name.as_deref().unwrap_or(""));
    h.str(n.value.as_deref().unwrap_or(""));
    let s = n.states;
    let tri = |b: Option<bool>| b.map_or(0, |b| 1 + u64::from(b));
    h.u64(
        u64::from(s.focused)
            | u64::from(s.selected) << 1
            | u64::from(s.disabled) << 2
            | u64::from(s.offscreen) << 3
            | tri(s.checked) << 4
            | tri(s.expanded) << 6,
    );
    if with_bounds {
        let r = n.bounds.unwrap_or(Rect {
            x: -1.0,
            y: -1.0,
            width: -1.0,
            height: -1.0,
        });
        for v in [r.x, r.y, r.width, r.height] {
            h.u64(v.round() as i64 as u64);
        }
    }
}

/// A cheap hash of the first `max_nodes` elements of a subtree (depth-first).
fn fingerprint<E: Element>(root: &E, max_nodes: usize, with_bounds: bool) -> u64 {
    let mut h = Fnv::default();
    let mut left = max_nodes.saturating_sub(1);
    let mut frontier: Vec<(E, usize)> = Vec::new();
    let n = read(root);
    hash_node(&mut h, &n, with_bounds);
    if !n.states.secure {
        let kids = root.children(64);
        h.u64(kids.len() as u64);
        frontier.extend(kids.into_iter().rev().map(|k| (k, 1)));
    }
    while let Some((el, depth)) = frontier.pop() {
        if left == 0 {
            break;
        }
        left -= 1;
        let n = read(&el);
        hash_node(&mut h, &n, with_bounds);
        if depth < FINGERPRINT_DEPTH && !n.states.secure && !n.states.offscreen {
            let kids = el.children(64);
            h.u64(kids.len() as u64);
            frontier.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
        }
    }
    h.finish()
}

struct Walk<'a> {
    window: &'a str,
    limits: &'a TreeLimits,
    deadline: Instant,
    nodes_left: usize,
    truncated: bool,
    ids: HashMap<ElementId, Entry>,
    /// The deepest focused element: (depth, id).
    focused: Option<(usize, ElementId)>,
    url: Option<String>,
}

impl Walk<'_> {
    fn visit<E: Element>(&mut self, el: &E, node: Node, path: &mut Vec<IdKey>) -> UiNode {
        let id = element_id(self.window, path);
        self.ids.insert(
            id.clone(),
            Entry {
                window: self.window.to_string(),
                path: path.clone(),
            },
        );
        let depth = path.len();
        if node.states.focused && self.focused.as_ref().is_none_or(|(d, _)| depth > *d) {
            self.focused = Some((depth, id.clone()));
        }
        if self.url.is_none() {
            self.url = node.attr("url").and_then(web_url);
        }
        let mut out = ui_node(id, &node, el);
        // Secure elements are opaque; off-screen subtrees are summarised by their root.
        if node.states.secure || (depth > 0 && self.limits.skip_offscreen && node.states.offscreen)
        {
            return out;
        }
        if depth >= self.limits.max_depth {
            if !el.children(1).is_empty() {
                self.truncated = true;
            }
            return out;
        }
        let mut kids = el.children(self.limits.max_children.saturating_add(1));
        if kids.len() > self.limits.max_children {
            kids.truncate(self.limits.max_children);
            self.truncated = true;
        }
        let mut ordinals = Ordinals::default();
        for kid in kids {
            if self.nodes_left == 0 || Instant::now() >= self.deadline {
                self.truncated = true;
                break;
            }
            self.nodes_left -= 1;
            let n = read(&kid);
            path.push(ordinals.next(&n));
            let child = self.visit(&kid, n, path);
            path.pop();
            out.children.push(child);
        }
        out
    }
}

/// An element found again from its id.
struct Resolved<P: Platform> {
    window: PlatformWindow<P::Element>,
    path: Vec<IdKey>,
    element: P::Element,
}

/// What [`Backend::observe_screen`] saw: the window list and the focused window's contents.
#[derive(PartialEq, Eq, Clone, Copy)]
struct Screen(u64);

impl<P: Platform> Backend<P> {
    fn denied(&self, app: &AppEntry, title: Option<&str>) -> bool {
        self.config().deny.is_denied(&app_info(app, title))
    }

    fn refuse_denied(&self, app: &AppEntry) -> Result<()> {
        if self.denied(app, None) {
            return Err(Error::Invalid(format!(
                "{} is excluded from computer use",
                app.name
            )));
        }
        Ok(())
    }

    fn platform_windows(&self) -> Result<Vec<PlatformWindow<P::Element>>> {
        let mut ws = self.platform().windows()?;
        for w in &mut ws {
            w.app = clean_app(w.app.clone());
            w.title = sanitize::clean_opt(w.title.as_deref(), sanitize::MAX_SHORT * 2);
        }
        ws.sort_by_key(|w| !w.focused);
        Ok(ws)
    }

    fn window_info(w: &PlatformWindow<P::Element>) -> WindowInfo {
        WindowInfo {
            id: window_id(&w.native_id),
            app: w.app.clone(),
            title: w.title.clone(),
            bounds: w.bounds,
            focused: w.focused,
            minimized: w.minimized,
        }
    }

    /// The selected window; `Invalid` when its app is denied.
    fn select_window(&self, sel: &WindowSel) -> Result<PlatformWindow<P::Element>> {
        let ws = self.platform_windows()?;
        let found = match sel {
            WindowSel::Focused => ws.into_iter().next(),
            WindowSel::Id(id) => ws.into_iter().find(|w| window_id(&w.native_id) == *id),
            WindowSel::App(name) => ws.into_iter().find(|w| matches_app(&w.app, name)),
        };
        let w = found.ok_or_else(|| match sel {
            WindowSel::Focused => Error::NotFound("no window is open".into()),
            WindowSel::Id(id) => {
                Error::NotFound(format!("window {id} is gone; list windows again"))
            }
            WindowSel::App(name) => Error::NotFound(format!("no window of an app named {name:?}")),
        })?;
        if self.denied(&w.app, w.title.as_deref()) {
            return Err(Error::Invalid(format!(
                "{} is excluded from computer use",
                w.app.name
            )));
        }
        Ok(w)
    }

    fn stale(id: &ElementId) -> Error {
        Error::NotFound(format!("element {id} is stale; observe again"))
    }

    /// Find the element at a non-empty `path` under `root`, matching each step by role, label
    /// and ordinal. `None` for an empty path (the caller holds the root).
    fn walk_path(root: &P::Element, path: &[IdKey]) -> Option<P::Element> {
        let mut cur: Option<P::Element> = None;
        for key in path {
            let parent = cur.as_ref().unwrap_or(root);
            let mut ordinals = Ordinals::default();
            let next = parent
                .children(RESOLVE_CHILDREN)
                .into_iter()
                .find(|k| ordinals.next(&read(k)) == *key)?;
            cur = Some(next);
        }
        cur
    }

    fn resolve_entry(&self, id: &ElementId, entry: &Entry) -> Result<Resolved<P>> {
        let window = self
            .platform_windows()?
            .into_iter()
            .find(|w| w.native_id == entry.window)
            .ok_or_else(|| Self::stale(id))?;
        if self.denied(&window.app, window.title.as_deref()) {
            return Err(Error::Invalid(format!(
                "{} is excluded from computer use",
                window.app.name
            )));
        }
        let element = if entry.path.is_empty() {
            None
        } else {
            Some(Self::walk_path(&window.element, &entry.path).ok_or_else(|| Self::stale(id))?)
        };
        let element = match element {
            Some(e) => e,
            // The window itself: re-fetch it so we own a handle.
            None => self
                .platform()
                .windows()?
                .into_iter()
                .find(|w| w.native_id == entry.window)
                .map(|w| w.element)
                .ok_or_else(|| Self::stale(id))?,
        };
        Ok(Resolved {
            window,
            path: entry.path.clone(),
            element,
        })
    }

    fn resolve(&self, id: &ElementId) -> Result<Resolved<P>> {
        let entry = self.computer.get(id).ok_or_else(|| {
            Error::NotFound(format!("unknown element {id}; observe the window first"))
        })?;
        self.resolve_entry(id, &entry)
    }

    /// Whether the element at `path` still exists in its window.
    fn still_there(&self, window: &str, path: &[IdKey]) -> bool {
        let Ok(ws) = self.platform().windows() else {
            return true;
        };
        let Some(w) = ws.into_iter().find(|w| w.native_id == window) else {
            return false;
        };
        path.is_empty() || Self::walk_path(&w.element, path).is_some()
    }

    /// The window list plus the focused window's contents, hashed.
    fn observe_screen(&self, window: Option<&str>, with_bounds: bool) -> Screen {
        let mut h = Fnv::default();
        let Ok(ws) = self.platform().windows() else {
            return Screen(0);
        };
        for w in &ws {
            h.str(&w.native_id);
            h.str(w.title.as_deref().unwrap_or(""));
            h.u64(u64::from(w.focused) | u64::from(w.minimized) << 1);
        }
        let target = match window {
            Some(n) => ws.iter().find(|w| w.native_id == n),
            None => ws.iter().find(|w| w.focused).or(ws.first()),
        };
        if let Some(w) = target {
            h.u64(fingerprint(&w.element, FINGERPRINT_NODES, with_bounds));
        }
        Screen(h.finish())
    }

    /// Poll until `check` holds or `verify_timeout` passes. Returns whether it held.
    fn poll(&self, mut check: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + self.config().verify_timeout;
        loop {
            if check() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// The id path of `el` within one of `windows`, climbing parents.
    fn locate(
        &self,
        el: &P::Element,
        windows: &[PlatformWindow<P::Element>],
    ) -> Option<(usize, Vec<IdKey>)> {
        let mut rev = Vec::new();
        let mut owned: Option<P::Element> = None;
        for _ in 0..MAX_LOCATE_DEPTH {
            let cur = owned.as_ref().unwrap_or(el);
            if let Some(i) = windows.iter().position(|w| w.element.same(cur)) {
                rev.reverse();
                return Some((i, rev));
            }
            let parent = cur.parent()?;
            let mut ordinals = Ordinals::default();
            let mut key = None;
            for k in parent.children(RESOLVE_CHILDREN) {
                let kk = ordinals.next(&read(&k));
                if k.same(cur) {
                    key = Some(kk);
                    break;
                }
            }
            rev.push(key?);
            owned = Some(parent);
        }
        None
    }

    fn shallow(id: &ElementId, el: &P::Element) -> (Node, UiNode) {
        let n = read(el);
        let u = ui_node(id.clone(), &n, el);
        (n, u)
    }

    fn act_inner(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        let r = self.resolve(id)?;
        let el = &r.element;
        let (before, before_ui) = Self::shallow(id, el);
        if before.states.disabled {
            return Err(Error::Invalid(format!("element {id} is disabled")));
        }
        let action = match action {
            UiAction::SetValue { value } => {
                if before.states.secure {
                    return Err(Error::Invalid(
                        "refusing to set the value of a secure field".into(),
                    ));
                }
                UiAction::SetValue {
                    value: clean_input(value),
                }
            }
            a => a.clone(),
        };
        let with_bounds = matches!(action, UiAction::Scroll { .. });
        let screen_before = match action {
            UiAction::Press | UiAction::Scroll { .. } => {
                Some(self.observe_screen(Some(&r.window.native_id), with_bounds))
            }
            _ => None,
        };
        let local_before = with_bounds.then(|| fingerprint(el, 200, true));

        el.perform(&action)?;

        let mut after = before.clone();
        let mut detail = String::new();
        let verified = self.poll(|| {
            after = read(el);
            let s = after.states;
            match &action {
                UiAction::SetValue { value } => {
                    let ok = norm(after.value.as_deref().unwrap_or("")) == norm(value);
                    detail = if ok {
                        "value set".into()
                    } else {
                        "the value read back differs".into()
                    };
                    ok
                }
                UiAction::Select => {
                    let ok = s.selected || s.checked == Some(true);
                    detail = if ok {
                        "selected".into()
                    } else {
                        "not selected after the action".into()
                    };
                    ok
                }
                UiAction::Focus => {
                    detail = if s.focused {
                        "focused".into()
                    } else {
                        "focus did not move to the element".into()
                    };
                    s.focused
                }
                UiAction::Expand { expand } => {
                    let ok = s.expanded == Some(*expand);
                    let word = if *expand { "expanded" } else { "collapsed" };
                    detail = if ok {
                        word.into()
                    } else {
                        format!("not {word} after the action")
                    };
                    ok
                }
                UiAction::Press => {
                    if let Some(what) = press_change(&before, &after) {
                        detail = format!("the element's {what} changed");
                        return true;
                    }
                    if !self.still_there(&r.window.native_id, &r.path) {
                        detail = "the element went away".into();
                        return true;
                    }
                    if screen_before != Some(self.observe_screen(Some(&r.window.native_id), false))
                    {
                        detail = "the window changed".into();
                        return true;
                    }
                    detail = "nothing visibly changed".into();
                    false
                }
                UiAction::Scroll { .. } => {
                    if local_before != Some(fingerprint(el, 200, true))
                        || screen_before
                            != Some(self.observe_screen(Some(&r.window.native_id), true))
                    {
                        detail = "content moved".into();
                        return true;
                    }
                    detail = "nothing visibly moved (the view may already be at its end)".into();
                    false
                }
            }
        });
        let after_ui = ui_node(id.clone(), &after, el);
        Ok(ActionOutcome {
            verified,
            detail,
            before: Some(before_ui),
            after: Some(after_ui),
        })
    }

    fn key_input_inner(
        &self,
        target: Option<&ElementId>,
        input: &KeyInput,
    ) -> Result<ActionOutcome> {
        if let KeyInput::Keys(k) = input
            && k.is_empty()
        {
            return Err(Error::Invalid("no keys given".into()));
        }
        // The element that will receive the input, with its id when we can name it.
        let (id, el, window) = match target {
            Some(id) => {
                let r = self.resolve(id)?;
                let n = read(&r.element);
                if n.states.disabled {
                    return Err(Error::Invalid(format!("element {id} is disabled")));
                }
                if !n.states.focused {
                    r.element.perform(&UiAction::Focus)?;
                    if !self.poll(|| read(&r.element).states.focused) {
                        return Ok(ActionOutcome {
                            verified: false,
                            detail: "could not focus the target; nothing was typed".into(),
                            before: Some(ui_node(id.clone(), &n, &r.element)),
                            after: Some(ui_node(id.clone(), &read(&r.element), &r.element)),
                        });
                    }
                }
                (Some(id.clone()), Some(r.element), Some(r.window.native_id))
            }
            None => self.focused_element()?,
        };

        let text = match input {
            KeyInput::Text(t) => {
                let Some(el) = &el else {
                    return Err(Error::NotFound(
                        "nothing has keyboard focus to type into".into(),
                    ));
                };
                let n = read(el);
                if n.states.secure {
                    return Err(Error::Invalid(
                        "refusing to type into a secure field".into(),
                    ));
                }
                let mut t = clean_input(t);
                if single_line(&n) {
                    t.retain(|c| c != '\n');
                }
                if t.is_empty() {
                    return Err(Error::Invalid("nothing to type".into()));
                }
                Some(t)
            }
            KeyInput::Keys(_) => None,
        };

        let eid = id.clone().unwrap_or_default();
        let before = el.as_ref().map(|e| Self::shallow(&eid, e));
        let screen_before = self.observe_screen(window.as_deref(), false);

        let sent = match &text {
            Some(t) => KeyInput::Text(t.clone()),
            None => input.clone(),
        };
        self.platform().key_input(&sent)?;

        let mut after: Option<Node> = None;
        let mut detail = String::new();
        let verified = self.poll(|| {
            after = el.as_ref().map(read);
            match &text {
                Some(t) => {
                    let v = after
                        .as_ref()
                        .and_then(|n| n.value.as_deref())
                        .unwrap_or("");
                    let ok = norm(v).contains(&norm(t));
                    detail = if ok {
                        "the field contains the typed text".into()
                    } else {
                        "the typed text was not found in the field".into()
                    };
                    ok
                }
                None => {
                    if let (Some((b, _)), Some(a)) = (&before, &after)
                        && let Some(what) = press_change(b, a)
                    {
                        detail = format!("the focused element's {what} changed");
                        return true;
                    }
                    if self.observe_screen(window.as_deref(), false) != screen_before {
                        detail = "the window changed".into();
                        return true;
                    }
                    detail = "nothing visibly changed".into();
                    false
                }
            }
        });
        let after_ui = match (&el, &after) {
            (Some(e), Some(n)) => Some(ui_node(eid, n, e)),
            _ => None,
        };
        Ok(ActionOutcome {
            verified,
            detail,
            before: before.map(|(_, u)| u),
            after: after_ui,
        })
    }

    /// The focused element of the frontmost app, with an id when it can be located in a window.
    /// Refuses when the frontmost app is denied.
    #[allow(clippy::type_complexity)]
    fn focused_element(&self) -> Result<(Option<ElementId>, Option<P::Element>, Option<String>)> {
        let Some(t) = self.platform().focus()? else {
            if let Some(front) = self
                .platform()
                .apps()
                .ok()
                .and_then(|a| a.into_iter().map(clean_app).find(|a| a.frontmost))
            {
                self.refuse_denied(&front)?;
            }
            return Ok((None, None, None));
        };
        let mut app = t.app;
        app.id = sanitize::clean_opt(app.id.as_deref(), sanitize::MAX_SHORT);
        app.name = sanitize::clean_line(&app.name, sanitize::MAX_SHORT);
        if self.config().deny.is_denied(&app) {
            return Err(Error::Invalid(format!(
                "{} is excluded from computer use",
                app.name
            )));
        }
        let windows = self.platform().windows().unwrap_or_default();
        match self.locate(&t.element, &windows) {
            Some((i, path)) => {
                let native = windows[i].native_id.clone();
                let id = element_id(&native, &path);
                self.computer.add(
                    id.clone(),
                    Entry {
                        window: native.clone(),
                        path,
                    },
                );
                Ok((Some(id), Some(t.element), Some(native)))
            }
            None => Ok((None, Some(t.element), None)),
        }
    }
}

/// What a press visibly changed on the element itself.
fn press_change(before: &Node, after: &Node) -> Option<&'static str> {
    let (b, a) = (before.states, after.states);
    if b.checked != a.checked {
        Some("checked state")
    } else if b.expanded != a.expanded {
        Some("expanded state")
    } else if b.selected != a.selected {
        Some("selection")
    } else if before.value != after.value {
        Some("value")
    } else if before.name != after.name {
        Some("name")
    } else if b.focused != a.focused {
        Some("focus")
    } else if b.disabled != a.disabled {
        Some("enabled state")
    } else {
        None
    }
}

impl<P: Platform> ComputerUse for Backend<P> {
    fn permission(&self) -> Permission {
        self.platform().permission()
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        let mut apps: Vec<AppEntry> = self
            .platform()
            .apps()?
            .into_iter()
            .map(clean_app)
            .filter(|a| !self.denied(a, None))
            .collect();
        apps.sort_by_key(|a| !a.frontmost);
        Ok(apps)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(self
            .platform_windows()?
            .iter()
            .filter(|w| !self.denied(&w.app, w.title.as_deref()))
            .map(Self::window_info)
            .collect())
    }

    fn snapshot(&self, window: &WindowSel, limits: &TreeLimits) -> Result<UiTree> {
        let w = self.select_window(window)?;
        let mut walk = Walk {
            window: &w.native_id,
            limits,
            deadline: Instant::now() + limits.deadline,
            nodes_left: limits.max_nodes.saturating_sub(1),
            truncated: false,
            ids: HashMap::new(),
            focused: None,
            url: None,
        };
        let root_node = read(&w.element);
        let root = walk.visit(&w.element, root_node, &mut Vec::new());
        let mut app = app_info(&w.app, w.title.as_deref());
        app.url = walk.url.take();
        if self.config().deny.is_denied(&app) {
            return Err(Error::Invalid(format!(
                "{} is excluded from computer use",
                app.name
            )));
        }
        let focused = walk.focused.take().map(|(_, id)| id);
        let truncated = walk.truncated;
        self.computer.push(std::mem::take(&mut walk.ids));
        Ok(UiTree {
            app,
            window: Self::window_info(&w),
            root,
            focused,
            truncated,
        })
    }

    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        self.act_inner(id, action)
    }

    fn key_input(&self, target: Option<&ElementId>, input: &KeyInput) -> Result<ActionOutcome> {
        self.key_input_inner(target, input)
    }

    fn open_app(&self, name: &str) -> Result<ActionOutcome> {
        let name = sanitize::clean_line(name, sanitize::MAX_SHORT);
        if name.is_empty() {
            return Err(Error::Invalid("no app name given".into()));
        }
        let probe = AppEntry {
            id: Some(name.clone()),
            name: name.clone(),
            pid: None,
            frontmost: false,
        };
        self.refuse_denied(&probe)?;
        self.platform().open_app(&name)?;
        // Launching takes longer than an element action.
        let deadline = Instant::now() + self.config().verify_timeout.saturating_mul(8);
        loop {
            let apps: Vec<AppEntry> = self
                .platform()
                .apps()
                .unwrap_or_default()
                .into_iter()
                .map(clean_app)
                .collect();
            if let Some(a) = apps.iter().find(|a| a.frontmost && matches_app(a, &name)) {
                if self.denied(a, None) {
                    return Ok(ActionOutcome {
                        verified: false,
                        detail: "the app that opened is excluded from computer use".into(),
                        before: None,
                        after: None,
                    });
                }
                return Ok(ActionOutcome {
                    verified: true,
                    detail: format!("{} is frontmost", a.name),
                    before: None,
                    after: None,
                });
            }
            if Instant::now() >= deadline {
                let running = apps.iter().any(|a| matches_app(a, &name));
                return Ok(ActionOutcome {
                    verified: false,
                    detail: if running {
                        format!("{name} is running but not frontmost")
                    } else {
                        format!("{name} did not appear")
                    },
                    before: None,
                    after: None,
                });
            }
            std::thread::sleep(POLL);
        }
    }

    fn screenshot(&self, window: &WindowSel) -> Result<Option<Vec<u8>>> {
        let w = self.select_window(window)?;
        self.platform().screenshot(&w.element)
    }
}
