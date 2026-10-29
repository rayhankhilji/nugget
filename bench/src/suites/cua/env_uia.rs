//! Real accessibility for the `native` environment: the app windows (Edge on Windows, Chrome on
//! macOS, see [`super::env_native`]) read and driven through the OS accessibility API
//! (`nugget_a11y::system_computer()`: UI Automation on Windows, AX on macOS). Nothing here is
//! specific to one OS: it works on any [`ComputerUse`]. Each snapshot is first put in the
//! matcher's conventions by [`super::env_ax::normalize`] (a no-op for UIA trees).
//!
//! - **Matching.** A web page's accessibility tree has no bench ids (`UiNode` carries no
//!   automation id), so the real tree of an app window is matched to the app's bench tree
//!   ([`super::ui::Node`], from the session's observation) by **role class, accessible name and
//!   document order** ([`match_tree`]): bench nodes are visited depth first and each takes the
//!   next real node, after the previous match, whose role class (press, check, edit, combo or
//!   other) and name agree. Extra real nodes (the `<h1>`, labels, the browser frame) are
//!   skipped; the subtree of a matched control is not searched again (a button's own text). A
//!   name matches exactly after collapsing whitespace, or with a CSS suffix such as ` (on)`, or
//!   as a prefix the accessibility layer cut with `…`. A real window is **consistent** with the
//!   bench tree when every named bench node matched and every control shows the bench state
//!   (text value, checked, selected option): an old page that has not reloaded yet is not.
//! - **Presentation.** [`present`] builds the tree the model sees: the bench structure, ids
//!   [`encode_id`]d from the bench ids (so tool results show `[shop-pay]`), and everything else
//!   (role, name, value, states, actions, bounds) read from the real element. A combo box lists
//!   its options as `<combo>.opt<n>` children, as in the socket environments.
//! - **Actions** ([`UiaDriver::perform`]): the bench action's target is located in the real
//!   window and acted on through UI Automation: Invoke or Toggle for a click, ValuePattern for
//!   text, SelectionItem for an option (falling back to keyboard input when a page ignores the
//!   pattern). The page itself posts the change to the apps server, so the world changes exactly
//!   as it would for a user; the action succeeded when the world version moves, and failed
//!   (with the page's status line as the message) when it does not.
//! - **The agent's backend** ([`UiaComputer`]): a [`ComputerUse`] whose snapshots are real
//!   snapshots presented with bench ids, whose element actions are real UIA actions (performed
//!   by the session, which counts the step), and whose outcomes are verified by reading the
//!   real window again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nugget_a11y::Permission;
use nugget_a11y::computer::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, Key, KeyChord, KeyInput, Modifiers, NamedKey,
    TreeLimits, UiAction, UiActionKind, UiNode, UiTree, WindowId, WindowInfo, WindowSel,
};
use nugget_a11y::role::Role as A11yRole;
use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};

use super::agent::{ActResult, EnvError};
use super::env_ax::normalize;
use super::nugget_env::{WorldPort, decode_id, encode_id};
use super::ui::{Action, Node, Observation, Role, app_of, check_action};

/// Suffix of a combo-box option's bench id (`forms.country.opt2`), as in the socket envs.
const OPTION: &str = ".opt";
/// How long a window may take to show the bench state (a page reload after an action).
pub const SETTLE: Duration = Duration::from_secs(12);
/// How long an action may take to reach the apps server.
const EFFECT: Duration = Duration::from_secs(4);
/// When to try the keyboard instead, if a pattern call changed nothing.
const FALLBACK_AFTER: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_millis(60);

/// Limits for reading a whole app window: the page is small, but the browser frame adds depth,
/// and off-screen controls (below the fold, a closed list's options) must be read too.
pub fn real_limits() -> TreeLimits {
    TreeLimits {
        max_nodes: 4000,
        max_depth: 60,
        max_children: 500,
        deadline: Duration::from_secs(6),
        skip_offscreen: false,
    }
}

fn option_id(combo: &str, i: usize) -> String {
    format!("{combo}{OPTION}{i}")
}

// ---------------------------------------------------------------------------------------------
// Matching

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Press,
    Check,
    Edit,
    Combo,
    Other,
}

fn bench_class(role: Role) -> Class {
    match role {
        Role::Button | Role::Switch => Class::Press,
        Role::CheckBox => Class::Check,
        Role::TextBox | Role::TextArea => Class::Edit,
        Role::ComboBox => Class::Combo,
        _ => Class::Other,
    }
}

fn real_class(n: &UiNode) -> Class {
    match n.role {
        A11yRole::Button | A11yRole::Link | A11yRole::MenuItem => Class::Press,
        A11yRole::CheckBox => Class::Check,
        A11yRole::TextField | A11yRole::TextArea | A11yRole::SearchField => Class::Edit,
        A11yRole::ComboBox => Class::Combo,
        _ => Class::Other,
    }
}

fn compatible(b: Class, r: Class) -> bool {
    match b {
        // A switch is a toggle button: UIA may call it a button or a check box.
        Class::Press | Class::Check => matches!(r, Class::Press | Class::Check),
        other => other == r,
    }
}

/// Whitespace-collapsed comparison form.
pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text values compare without `\r` and trailing whitespace.
fn norm_value(s: Option<&str>) -> String {
    s.unwrap_or("")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_owned()
}

/// Whether a real accessible name is the bench name (see the module docs).
pub fn name_matches(bench: &str, real: Option<&str>) -> bool {
    let b = norm(bench);
    let r = norm(real.unwrap_or(""));
    if r == b {
        return true;
    }
    if b.is_empty() {
        return false;
    }
    // CSS generated content (`::after { content: ' (on)' }`) is part of the name.
    if let Some(rest) = r.strip_prefix(b.as_str())
        && rest.starts_with(" (")
    {
        return true;
    }
    // A long name the accessibility layer cut.
    if let Some(cut) = r.strip_suffix('…') {
        let cut = cut.trim_end();
        return cut.chars().count() >= 12 && b.starts_with(cut);
    }
    false
}

/// A real node in document order, with the end of its subtree.
struct Flat<'a> {
    node: &'a UiNode,
    end: usize,
}

fn flatten<'a>(n: &'a UiNode, out: &mut Vec<Flat<'a>>) {
    let i = out.len();
    out.push(Flat {
        node: n,
        end: i + 1,
    });
    for c in &n.children {
        flatten(c, out);
    }
    let end = out.len();
    out[i].end = end;
}

/// Shallow copy, no children.
fn shallow(n: &UiNode) -> UiNode {
    UiNode {
        children: Vec::new(),
        ..n.clone()
    }
}

/// How one real window lines up with one bench window.
#[derive(Debug, Clone, Default)]
pub struct Match {
    /// Bench id → the real element (shallow).
    pub nodes: HashMap<String, UiNode>,
    /// Combo-box option bench id (`forms.country.opt2`) → the real option element.
    pub options: HashMap<String, UiNode>,
    /// Named bench nodes with no real counterpart.
    pub missing: Vec<String>,
    /// Bench controls whose real state differs (value, checked): the page is not current.
    pub stale: Vec<String>,
    /// Text after the last matched element (the page's status line), non-empty names only.
    pub trailing: Vec<String>,
}

impl Match {
    /// Every named bench node is on screen in the bench state.
    pub fn consistent(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty()
    }
}

fn state_agrees(b: &Node, r: &UiNode) -> bool {
    match b.role {
        Role::TextBox | Role::TextArea => {
            r.states.secure || norm_value(r.value.as_deref()) == norm_value(b.value.as_deref())
        }
        Role::CheckBox | Role::Switch => match (b.checked, r.states.checked) {
            (Some(want), Some(got)) => want == got,
            _ => true,
        },
        Role::ComboBox => match (&b.value, &r.value) {
            (Some(want), Some(got)) => norm(want) == norm(got),
            _ => true,
        },
        _ => true,
    }
}

/// Match a real window tree to a bench window tree (see the module docs).
pub fn match_tree(bench: &Node, real: &UiNode) -> Match {
    let mut flat = Vec::new();
    flatten(real, &mut flat);
    let mut bench_nodes = Vec::new();
    bench.walk(&mut bench_nodes);
    let mut m = Match::default();
    let mut pos = 0usize;
    let mut last_end = 0usize;
    for b in bench_nodes {
        let class = bench_class(b.role);
        // An unnamed decoration (an empty text) need not be found.
        if b.name.trim().is_empty() && class == Class::Other {
            continue;
        }
        let hit = (pos..flat.len()).find(|&i| {
            let r = flat[i].node;
            compatible(class, real_class(r))
                && (name_matches(&b.name, r.name.as_deref())
                    || (b.name.trim().is_empty() && class != Class::Other))
        });
        let Some(i) = hit else {
            m.missing.push(b.id.clone());
            continue;
        };
        let r = flat[i].node;
        if !state_agrees(b, r) {
            m.stale.push(b.id.clone());
        }
        m.nodes.insert(b.id.clone(), shallow(r));
        if class == Class::Other {
            pos = i + 1;
            last_end = last_end.max(i + 1);
        } else {
            // Options of a combo box: its descendants named like the bench options, in order.
            if class == Class::Combo {
                let mut from = i + 1;
                for (k, o) in b.options.iter().enumerate() {
                    let found = (from..flat[i].end).find(|&j| {
                        let n = flat[j].node;
                        (matches!(n.role, A11yRole::ListItem | A11yRole::MenuItem)
                            || n.actions.contains(&UiActionKind::Select))
                            && name_matches(o, n.name.as_deref())
                    });
                    if let Some(j) = found {
                        m.options.insert(option_id(&b.id, k), shallow(flat[j].node));
                        from = j + 1;
                    }
                }
            }
            pos = flat[i].end;
            last_end = last_end.max(flat[i].end);
        }
    }
    // The window's own title (AX shows it as text after the page) is not a status line.
    let title = norm(real.name.as_deref().unwrap_or(""));
    m.trailing = flat[last_end.min(flat.len())..]
        .iter()
        .filter(|f| f.node.children.is_empty() && real_class(f.node) == Class::Other)
        .filter_map(|f| f.node.name.as_deref().map(norm))
        .filter(|s| !s.is_empty() && *s != title)
        .collect();
    m
}

fn bench_role_actions(b: &Node) -> Vec<UiActionKind> {
    if !b.enabled {
        return Vec::new();
    }
    match b.role {
        Role::Button | Role::CheckBox | Role::Switch => vec![UiActionKind::Press],
        Role::TextBox | Role::TextArea => vec![UiActionKind::SetValue, UiActionKind::Focus],
        Role::ComboBox => vec![
            UiActionKind::SetValue,
            UiActionKind::Expand,
            UiActionKind::Focus,
        ],
        _ => Vec::new(),
    }
}

/// The tree the model sees: bench structure and ids, real content (see the module docs).
/// Bench nodes with no real counterpart are left out (their children are kept).
pub fn present(bench: &Node, m: &Match, title: Option<&str>) -> UiNode {
    let mut out = present_node(bench, m);
    let root = out.pop().unwrap_or_default();
    let mut root = if root.id.as_str() == encode_id(&bench.id) {
        root
    } else {
        // The window itself was not matched: a bare window around what was.
        let mut w = UiNode {
            id: ElementId(encode_id(&bench.id)),
            role: A11yRole::Window,
            name: Some(bench.name.clone()),
            ..UiNode::default()
        };
        w.children.push(root);
        w.children.extend(out);
        w
    };
    root.role = A11yRole::Window;
    if let Some(t) = title {
        root.name = Some(t.to_owned());
    }
    root
}

/// Presented nodes for `b`: one node, or (unmatched) its presented children.
fn present_node(b: &Node, m: &Match) -> Vec<UiNode> {
    let kids: Vec<UiNode> = b.children.iter().flat_map(|c| present_node(c, m)).collect();
    let Some(r) = m.nodes.get(&b.id) else {
        return kids;
    };
    let mut n = UiNode {
        id: ElementId(encode_id(&b.id)),
        children: kids,
        ..r.clone()
    };
    // Bench controls keep their capabilities even when the browser reports fewer patterns
    // (a page's script, not a pattern, does the work).
    for a in bench_role_actions(b) {
        if !n.actions.contains(&a) {
            n.actions.push(a);
        }
    }
    if b.role == Role::ComboBox {
        if !b.options.is_empty() {
            n.description = Some(format!("options: {}", b.options.join(" | ")));
        }
        let current = r.value.clone().or_else(|| b.value.clone());
        for (i, o) in b.options.iter().enumerate() {
            let oid = option_id(&b.id, i);
            let real = m.options.get(&oid);
            n.children.push(UiNode {
                id: ElementId(encode_id(&oid)),
                role: A11yRole::ListItem,
                raw_role: real
                    .map(|x| x.raw_role.clone())
                    .unwrap_or_else(|| "option".into()),
                name: Some(o.clone()),
                bounds: real.and_then(|x| x.bounds),
                states: nugget_a11y::tree::States {
                    selected: current.as_deref().map(norm) == Some(norm(o)),
                    disabled: !b.enabled,
                    ..Default::default()
                },
                actions: if b.enabled {
                    vec![UiActionKind::Select]
                } else {
                    Vec::new()
                },
                ..UiNode::default()
            });
        }
    }
    vec![n]
}

// ---------------------------------------------------------------------------------------------
// The driver: real windows of the apps

/// Which real window shows which app.
pub trait WindowMap: Send + Sync {
    /// The process that owns `app`'s window, when known.
    fn pid(&self, app: &str) -> Option<u32>;
}

impl WindowMap for HashMap<String, u32> {
    fn pid(&self, app: &str) -> Option<u32> {
        self.get(app).copied()
    }
}

/// A read of one app window, matched to its bench tree.
pub struct Located {
    pub window: WindowInfo,
    pub tree: UiTree,
    pub bench: Node,
    pub matched: Match,
}

impl Located {
    pub fn real(&self, bench_id: &str) -> Option<&UiNode> {
        self.matched
            .nodes
            .get(bench_id)
            .or_else(|| self.matched.options.get(bench_id))
    }
}

/// Real UIA reads and actions on the app windows, shared by the session and the agent's backend.
pub struct UiaDriver {
    sys: Arc<dyn ComputerUse>,
    windows: Arc<dyn WindowMap>,
    settle: Duration,
    /// Locates that timed out before the window showed the bench state.
    pub unsettled: std::sync::atomic::AtomicU32,
    /// Real window reads (snapshots over the accessibility API) by `locate`.
    pub reads: std::sync::atomic::AtomicU32,
    /// Actions that needed the keyboard fallback.
    pub fallbacks: std::sync::atomic::AtomicU32,
}

fn fail(message: impl Into<String>) -> ActResult {
    ActResult {
        ok: false,
        message: Some(message.into()),
    }
}

fn ok() -> ActResult {
    ActResult {
        ok: true,
        message: None,
    }
}

fn chord(key: Key) -> KeyChord {
    KeyChord {
        key,
        modifiers: Modifiers::default(),
    }
}

/// Typing an option's text into a closed list. On macOS each character goes as its own key
/// press: a list's type-ahead reads key presses, and the text input path sends a string per
/// event.
fn type_ahead(option: &str) -> KeyInput {
    if cfg!(target_os = "macos") {
        KeyInput::Keys(option.chars().map(|c| chord(Key::Char(c))).collect())
    } else {
        KeyInput::Text(option.to_owned())
    }
}

/// Select all: Command+A on macOS, Ctrl+A elsewhere.
fn select_all() -> KeyChord {
    let mac = cfg!(target_os = "macos");
    KeyChord {
        key: Key::Char('a'),
        modifiers: Modifiers {
            ctrl: !mac,
            meta: mac,
            ..Modifiers::default()
        },
    }
}

impl UiaDriver {
    pub fn new(sys: Arc<dyn ComputerUse>, windows: Arc<dyn WindowMap>) -> Self {
        Self {
            sys,
            windows,
            settle: SETTLE,
            unsettled: Default::default(),
            reads: Default::default(),
            fallbacks: Default::default(),
        }
    }

    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    pub fn sys(&self) -> &Arc<dyn ComputerUse> {
        &self.sys
    }

    /// The real window of `app`: by owning process, else by title (`title` is the bench
    /// window's name, which the page uses as its title).
    pub fn real_window(&self, app: &str, title: &str) -> Result<WindowInfo> {
        let ws = self.sys.windows()?;
        if let Some(pid) = self.windows.pid(app)
            && let Some(w) = ws.iter().find(|w| w.app.pid == Some(pid))
        {
            return Ok(w.clone());
        }
        let want = norm(title).to_lowercase();
        ws.into_iter()
            .find(|w| {
                let t = norm(w.title.as_deref().unwrap_or("")).to_lowercase();
                !want.is_empty() && (t == want || t.starts_with(&format!("{want} ")))
            })
            .ok_or_else(|| Error::NotFound(format!("no window for {app} ({title:?})")))
    }

    /// Read `app`'s window until it shows the bench state (or the settle time passes; the last
    /// read is returned either way). `bench` gives the current bench window.
    pub fn locate(&self, app: &str, bench: &dyn Fn() -> Result<Node>) -> Result<Located> {
        let t0 = Instant::now();
        let mut last: Option<Located> = None;
        let mut last_err: Option<Error> = None;
        loop {
            match bench().and_then(|b| {
                let window = self.real_window(app, &b.name)?;
                self.reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut tree = self
                    .sys
                    .snapshot(&WindowSel::Id(window.id.clone()), &real_limits())?;
                normalize(&mut tree.root);
                let matched = match_tree(&b, &tree.root);
                Ok(Located {
                    window,
                    tree,
                    bench: b,
                    matched,
                })
            }) {
                Ok(l) if l.matched.consistent() => return Ok(l),
                Ok(l) => last = Some(l),
                Err(e) => last_err = Some(e),
            }
            if t0.elapsed() >= self.settle {
                break;
            }
            std::thread::sleep(POLL * 3);
        }
        self.unsettled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match last {
            Some(l) => {
                eprintln!("{}", diagnose(app, &l));
                Ok(l)
            }
            None => Err(last_err.unwrap_or_else(|| Error::NotFound(format!("{app}: no window")))),
        }
    }

    /// Perform a bench action through UI Automation (see the module docs). `obs` is the bench
    /// observation before the action; `version` reads the world version.
    pub fn perform(
        &self,
        action: &Action,
        obs: &Observation,
        version: &dyn Fn() -> String,
        bench: &dyn Fn() -> Result<Node>,
    ) -> ActResult {
        let app = action.app().to_owned();
        let Some(win) = obs.windows.iter().find(|w| w.id == app) else {
            return fail(format!("app `{app}` is not open"));
        };
        let Some(node) = win.find(action.target()) else {
            return fail(format!("no element `{}` on screen", action.target()));
        };
        if let Err(e) = check_action(node, action) {
            return fail(e);
        }
        // Nothing for the page to report: the control already shows this.
        match action {
            Action::Type { text, .. }
                if norm_value(node.value.as_deref()) == norm_value(Some(text)) =>
            {
                return ok();
            }
            Action::Select { option, .. } if node.value.as_deref() == Some(option.as_str()) => {
                return ok();
            }
            _ => {}
        }
        // The page can be replaced between the read and the action (a second reload): the
        // element is then gone, so read the window again and retry once.
        let mut attempt = 0;
        let v0 = version();
        let (real, first) = loop {
            let located = match self.locate(&app, bench) {
                Ok(l) => l,
                Err(e) => return fail(format!("cannot read the {app} window: {e}")),
            };
            let Some(real) = located.real(action.target()).cloned() else {
                return fail(format!(
                    "`{}` is not in the real {app} window (accessibility match failed)",
                    node.name
                ));
            };
            let first: Result<ActionOutcome> = match action {
                Action::Click { .. } => self.sys.act(&real.id, &UiAction::Press),
                Action::Type { text, .. } => self.sys.act(
                    &real.id,
                    &UiAction::SetValue {
                        value: text.clone(),
                    },
                ),
                Action::Select { target, option } => {
                    let opt = node
                        .options
                        .iter()
                        .position(|o| o == option)
                        .and_then(|i| located.matched.options.get(&option_id(target, i)));
                    let has = |n: &UiNode, k: UiActionKind| n.actions.contains(&k);
                    match opt {
                        Some(o) if has(o, UiActionKind::Select) => {
                            self.sys.act(&o.id, &UiAction::Select)
                        }
                        // AX may expose a closed `<select>`'s options as menu items to press.
                        Some(o) if has(o, UiActionKind::Press) => {
                            self.sys.act(&o.id, &UiAction::Press)
                        }
                        Some(o) if !has(&real, UiActionKind::SetValue) => {
                            self.sys.act(&o.id, &UiAction::Select)
                        }
                        _ => self.sys.act(
                            &real.id,
                            &UiAction::SetValue {
                                value: option.clone(),
                            },
                        ),
                    }
                }
            };
            let stale = matches!(&first, Err(Error::NotFound(_)));
            if stale && attempt == 0 && version() == v0 {
                attempt += 1;
                std::thread::sleep(POLL * 5);
                continue;
            }
            break (real, first);
        };
        let first_err = first.err();
        if self.await_effect(&v0, version, FALLBACK_AFTER) {
            return ok();
        }
        // The pattern call changed nothing: type it, as a user would.
        let fallback = match action {
            Action::Click { .. } => None,
            Action::Type { text, .. } => Some(vec![
                KeyInput::Keys(vec![select_all()]),
                KeyInput::Text(text.clone()),
                KeyInput::Keys(vec![chord(Key::Named(NamedKey::Tab))]),
            ]),
            Action::Select { option, .. } => Some(vec![
                type_ahead(option),
                KeyInput::Keys(vec![chord(Key::Named(NamedKey::Tab))]),
            ]),
        };
        if let Some(inputs) = fallback {
            self.fallbacks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Each input focuses the field first (a no-op when it has focus); Tab then leaves
            // it, which commits the change.
            let mut target = real.id.clone();
            let mut retried = false;
            let mut i = 0;
            while i < inputs.len() {
                // A list's type-ahead commits on the first letter that picks the option; the
                // page then reloads, and the rest of the input has nothing left to do.
                if version() != v0 {
                    return ok();
                }
                match self.sys.key_input(Some(&target), &inputs[i]) {
                    Ok(_) => i += 1,
                    // The page was replaced under the field: read it again and start over.
                    Err(Error::NotFound(_)) if !retried => {
                        retried = true;
                        if self.await_effect(&v0, version, POLL * 5) {
                            return ok();
                        }
                        let fresh = self
                            .locate(&app, bench)
                            .ok()
                            .and_then(|l| l.real(action.target()).map(|n| n.id.clone()));
                        match fresh {
                            Some(id) => {
                                target = id;
                                i = 0;
                            }
                            None => {
                                return fail(format!("`{}` is gone from the window", node.name));
                            }
                        }
                    }
                    Err(e) => {
                        if self.await_effect(&v0, version, EFFECT) {
                            return ok();
                        }
                        return fail(format!("keyboard input failed: {e}"));
                    }
                }
            }
            if self.await_effect(&v0, version, EFFECT) {
                return ok();
            }
        } else if self.await_effect(&v0, version, EFFECT - FALLBACK_AFTER) {
            return ok();
        }
        // Refused: the page shows the app's message in its status line.
        if let Some(msg) = self.status(&app, bench) {
            return fail(msg);
        }
        fail(match first_err {
            Some(e) => format!("no visible effect ({e})"),
            None => "no visible effect".into(),
        })
    }

    /// Wait up to `wait` for the world to change.
    fn await_effect(&self, v0: &str, version: &dyn Fn() -> String, wait: Duration) -> bool {
        let t0 = Instant::now();
        loop {
            if version() != v0 {
                return true;
            }
            if t0.elapsed() >= wait {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// The page's status line (`#status`, set when the app refuses an action), if any.
    fn status(&self, app: &str, bench: &dyn Fn() -> Result<Node>) -> Option<String> {
        let b = bench().ok()?;
        let w = self.real_window(app, &b.name).ok()?;
        let mut tree = self
            .sys
            .snapshot(&WindowSel::Id(w.id), &real_limits())
            .ok()?;
        normalize(&mut tree.root);
        let m = match_tree(&b, &tree.root);
        m.trailing.into_iter().next()
    }
}

/// A short account of a failed match, for the run log.
pub fn diagnose(app: &str, l: &Located) -> String {
    let mut s = format!(
        "cua native: the {app} window ({:?}) did not settle: {} missing, {} stale",
        l.window.title,
        l.matched.missing.len(),
        l.matched.stale.len()
    );
    for id in l.matched.missing.iter().take(6) {
        if let Some(b) = l.bench.find(id) {
            s.push_str(&format!("\n  missing {id} ({:?} {:?})", b.role, b.name));
        }
    }
    for id in l.matched.stale.iter().take(6) {
        let b = l.bench.find(id);
        let r = l.matched.nodes.get(id);
        s.push_str(&format!(
            "\n  stale {id}: bench {:?}/{:?}, real {:?}/{:?}",
            b.and_then(|b| b.value.clone()),
            b.and_then(|b| b.checked),
            r.and_then(|r| r.value.clone()),
            r.and_then(|r| r.states.checked)
        ));
    }
    s.push_str("\n  real tree (first 60 named nodes):");
    for n in l
        .tree
        .root
        .iter()
        .filter(|n| n.name.as_deref().is_some_and(|x| !x.trim().is_empty()))
        .take(60)
    {
        let name: String = n.name.as_deref().unwrap_or("").chars().take(50).collect();
        s.push_str(&format!(
            "\n    {} {:?} {name:?} value={:?} checked={:?} actions={:?}",
            n.role.as_str(),
            n.raw_role,
            n.value
                .as_deref()
                .map(|v| v.chars().take(30).collect::<String>()),
            n.states.checked,
            n.actions
        ));
    }
    s
}

// ---------------------------------------------------------------------------------------------
// The agent's backend

#[derive(Default)]
struct Focus {
    app: Option<String>,
    element: Option<String>,
}

/// What a resolved bench id points at.
enum Target<'a> {
    Node(&'a Node),
    /// An option of a combo box.
    Option(&'a Node, String),
}

fn resolve<'a>(obs: &'a Observation, id: &str) -> Option<Target<'a>> {
    if let Some(n) = obs.find(id) {
        return Some(Target::Node(n));
    }
    let (combo, i) = id.rsplit_once(OPTION)?;
    let n = obs.find(combo)?;
    let i: usize = i.parse().ok()?;
    (n.role == Role::ComboBox)
        .then(|| n.options.get(i).map(|o| Target::Option(n, o.clone())))
        .flatten()
}

fn env_err(e: EnvError) -> Error {
    match e {
        EnvError::StepBudget(n) => Error::Invalid(format!("step budget of {n} actions exhausted")),
        EnvError::Transport(m) => Error::Other(m),
    }
}

fn app_title(w: &Node) -> &str {
    w.name.split(" — ").next().unwrap_or(&w.name)
}

/// The nugget arm's [`ComputerUse`] in the native environment (see the module docs).
pub struct UiaComputer {
    port: Arc<dyn WorldPort>,
    driver: Arc<UiaDriver>,
    focus: Mutex<Focus>,
    /// Bench id → real element id, from the latest snapshot of each window.
    real_ids: Mutex<HashMap<String, ElementId>>,
}

impl UiaComputer {
    pub fn new(port: Arc<dyn WorldPort>, driver: Arc<UiaDriver>) -> Self {
        Self {
            port,
            driver,
            focus: Mutex::new(Focus::default()),
            real_ids: Mutex::new(HashMap::new()),
        }
    }

    fn observe(&self) -> Result<Observation> {
        self.port.observe().map_err(env_err)
    }

    fn focus(&self) -> std::sync::MutexGuard<'_, Focus> {
        self.focus.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn frontmost(&self, obs: &Observation) -> Option<String> {
        let f = self.focus().app.clone();
        f.filter(|a| obs.windows.iter().any(|w| &w.id == a))
            .or_else(|| obs.windows.first().map(|w| w.id.clone()))
    }

    fn bench_window(&self, app: &str) -> Result<Node> {
        self.observe()?
            .windows
            .into_iter()
            .find(|w| w.id == app)
            .ok_or_else(|| Error::NotFound(format!("`{app}` is not open")))
    }

    fn app_for(&self, obs: &Observation, sel: &WindowSel) -> Result<String> {
        let front = self.frontmost(obs);
        let w = match sel {
            WindowSel::Focused => front
                .as_deref()
                .and_then(|f| obs.windows.iter().find(|w| w.id == f)),
            WindowSel::Id(id) => obs.windows.iter().find(|w| w.id == id.as_str()),
            WindowSel::App(name) => {
                let name = name.trim().to_lowercase();
                obs.windows.iter().find(|w| {
                    w.id == name
                        || w.name.to_lowercase() == name
                        || app_title(w).to_lowercase() == name
                })
            }
        };
        w.map(|w| w.id.clone())
            .ok_or_else(|| Error::NotFound(format!("no such window: {sel:?}")))
    }

    fn entry(&self, app: &str, pid: Option<u32>, front: Option<&str>) -> AppEntry {
        AppEntry {
            id: Some(app.to_owned()),
            name: app.to_owned(),
            pid,
            frontmost: front == Some(app),
        }
    }

    /// Read `app`'s real window, presented with bench ids.
    fn read(&self, app: &str) -> Result<(UiTree, Located)> {
        let l = self.driver.locate(app, &|| self.bench_window(app))?;
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let root = present(&l.bench, &l.matched, l.window.title.as_deref());
        {
            let mut ids = self.real_ids.lock().unwrap_or_else(|e| e.into_inner());
            ids.retain(|k, _| app_of(k) != app);
            for (k, v) in l.matched.nodes.iter().chain(l.matched.options.iter()) {
                ids.insert(k.clone(), v.id.clone());
            }
        }
        // Focus: the real focused element, when it is one of the bench controls.
        let real_focus = l.tree.focused.clone();
        let focused = real_focus.and_then(|f| {
            l.matched
                .nodes
                .iter()
                .find(|(_, n)| n.id == f)
                .map(|(k, _)| ElementId(encode_id(k)))
        });
        let window = WindowInfo {
            id: WindowId(app.to_owned()),
            app: self.entry(app, l.window.app.pid, front.as_deref()),
            title: l.window.title.clone(),
            bounds: l.window.bounds,
            focused: front.as_deref() == Some(app),
            minimized: l.window.minimized,
        };
        let tree = UiTree {
            app: AppInfo {
                id: Some(app.to_owned()),
                name: app.to_owned(),
                window_title: l.window.title.clone(),
                url: None,
            },
            window,
            root,
            focused,
            truncated: !l.matched.consistent(),
        };
        Ok((tree, l))
    }

    fn real_id(&self, bench: &str) -> Option<ElementId> {
        self.real_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(bench)
            .cloned()
    }

    fn not_found(id: &str) -> Error {
        Error::NotFound(format!(
            "element `{id}` is not on screen (stale or unknown id); observe again"
        ))
    }

    /// One real action through the session (which performs it over UIA and counts the step),
    /// verified by reading the real window again.
    fn perform(
        &self,
        id: &str,
        action: Action,
        check: impl Fn(Option<&UiNode>, &UiTree, &UiTree) -> (bool, String),
    ) -> Result<ActionOutcome> {
        let app = app_of(action.target()).to_owned();
        let (before_tree, _) = self.read(&app)?;
        let eid = ElementId(encode_id(id));
        let before = before_tree.find(&eid).map(shallow);
        let r = self.port.act(&action).map_err(env_err)?;
        {
            let mut f = self.focus();
            f.app = Some(app.clone());
            f.element = Some(id.to_owned());
        }
        let (after_tree, _) = self.read(&app)?;
        let after_node = after_tree.find(&eid);
        let after = after_node.map(shallow);
        if !r.ok {
            return Ok(ActionOutcome {
                verified: false,
                detail: format!(
                    "the app refused: {}",
                    r.message.unwrap_or_else(|| "no effect".into())
                ),
                before,
                after,
            });
        }
        let (verified, detail) = check(after_node, &before_tree, &after_tree);
        Ok(ActionOutcome {
            verified,
            detail,
            before,
            after,
        })
    }

    fn set_text(&self, n: &Node, value: &str) -> Result<ActionOutcome> {
        let want = value.to_owned();
        let action = Action::Type {
            target: n.id.clone(),
            text: want.clone(),
        };
        let name = n.name.clone();
        self.perform(&n.id, action, |after, _, _| match after {
            Some(x) if norm_value(x.value.as_deref()) == norm_value(Some(&want)) => {
                (true, format!("`{name}` now reads {want:?}"))
            }
            Some(x) => (
                false,
                format!(
                    "`{name}` reads {:?}, not {want:?}",
                    x.value.as_deref().unwrap_or("")
                ),
            ),
            None => (false, "the field is gone from the screen".into()),
        })
    }

    fn select(&self, n: &Node, option: &str) -> Result<ActionOutcome> {
        let want = n
            .options
            .iter()
            .find(|o| o.as_str() == option)
            .or_else(|| {
                n.options
                    .iter()
                    .find(|o| o.trim().eq_ignore_ascii_case(option.trim()))
            })
            .cloned()
            .unwrap_or_else(|| option.to_owned());
        let action = Action::Select {
            target: n.id.clone(),
            option: want.clone(),
        };
        let name = n.name.clone();
        self.perform(&n.id, action, |after, _, _| match after {
            Some(x) if x.value.as_deref().map(norm) == Some(norm(&want)) => {
                (true, format!("`{name}` now shows {want:?}"))
            }
            Some(x) => (
                false,
                format!(
                    "`{name}` shows {:?}, not {want:?}",
                    x.value.as_deref().unwrap_or("")
                ),
            ),
            None => (false, "the list is gone from the screen".into()),
        })
    }

    fn press(&self, n: &Node) -> Result<ActionOutcome> {
        let name = n.name.clone();
        let eid = ElementId(encode_id(&n.id));
        let action = Action::Click {
            target: n.id.clone(),
        };
        self.perform(&n.id, action, |after, before_tree, after_tree| {
            let was = before_tree.find(&eid).and_then(|x| x.states.checked);
            let now = after.and_then(|x| x.states.checked);
            if was.is_some() && now.is_some() && now != was {
                (
                    true,
                    format!(
                        "`{name}` is now {}",
                        if now == Some(true) { "on" } else { "off" }
                    ),
                )
            } else if strip_bounds(&before_tree.root) != strip_bounds(&after_tree.root) {
                (true, format!("pressed `{name}`; the screen changed"))
            } else {
                (false, format!("pressed `{name}`; nothing visibly changed"))
            }
        })
    }

    fn set_value(&self, n: &Node, value: &str) -> Result<ActionOutcome> {
        match n.role {
            Role::TextBox | Role::TextArea => self.set_text(n, value),
            Role::ComboBox => self.select(n, value),
            _ => Err(Error::Invalid(format!("`{}` has no value to set", n.name))),
        }
    }
}

/// A tree without bounds or focus, for "did the screen change".
fn strip_bounds(n: &UiNode) -> UiNode {
    UiNode {
        bounds: None,
        states: nugget_a11y::tree::States {
            focused: false,
            ..n.states
        },
        children: n.children.iter().map(strip_bounds).collect(),
        ..n.clone()
    }
}

impl ComputerUse for UiaComputer {
    fn permission(&self) -> Permission {
        self.driver.sys().permission()
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let mut out = Vec::new();
        for w in &obs.windows {
            let pid = self
                .driver
                .real_window(&w.id, &w.name)
                .ok()
                .and_then(|r| r.app.pid);
            out.push(self.entry(&w.id, pid, front.as_deref()));
        }
        out.sort_by_key(|a| !a.frontmost);
        Ok(out)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let mut out = Vec::new();
        for w in &obs.windows {
            let real = self.driver.real_window(&w.id, &w.name).ok();
            out.push(WindowInfo {
                id: WindowId(w.id.clone()),
                app: self.entry(
                    &w.id,
                    real.as_ref().and_then(|r| r.app.pid),
                    front.as_deref(),
                ),
                title: real
                    .as_ref()
                    .and_then(|r| r.title.clone())
                    .or_else(|| Some(w.name.clone())),
                bounds: real.as_ref().and_then(|r| r.bounds),
                focused: front.as_deref() == Some(w.id.as_str()),
                minimized: false,
            });
        }
        out.sort_by_key(|w| !w.focused);
        Ok(out)
    }

    fn snapshot(&self, window: &WindowSel, _limits: &TreeLimits) -> Result<UiTree> {
        let obs = self.observe()?;
        let app = self.app_for(&obs, window)?;
        Ok(self.read(&app)?.0)
    }

    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        let obs = self.observe()?;
        let bench = decode_id(id.as_str());
        let target = resolve(&obs, &bench).ok_or_else(|| Self::not_found(id.as_str()))?;
        match (target, action) {
            (Target::Option(n, o), UiAction::Select | UiAction::Press) => self.select(n, &o),
            (Target::Option(..), _) => Err(Error::Invalid(format!(
                "`{id}` is a list option; it can only be selected"
            ))),
            (Target::Node(n), UiAction::Press) => match n.role {
                Role::Button | Role::CheckBox | Role::Switch => self.press(n),
                _ => Err(Error::Invalid(format!("`{}` cannot be pressed", n.name))),
            },
            (Target::Node(n), UiAction::SetValue { value }) => self.set_value(n, value),
            (Target::Node(n), UiAction::Select) => Err(Error::Invalid(format!(
                "`{}` cannot be selected; select one of a list's options instead",
                n.name
            ))),
            // Focus and scroll change nothing in the apps: real, and no step.
            (Target::Node(n), a @ (UiAction::Focus | UiAction::Scroll { .. })) => {
                if self.real_id(&n.id).is_none() {
                    self.read(app_of(&n.id))?;
                }
                let real = self
                    .real_id(&n.id)
                    .ok_or_else(|| Self::not_found(id.as_str()))?;
                let out = self.driver.sys().act(&real, a)?;
                if matches!(a, UiAction::Focus) {
                    let mut f = self.focus();
                    f.app = Some(app_of(&n.id).to_owned());
                    f.element = Some(n.id.clone());
                }
                Ok(ActionOutcome {
                    before: None,
                    after: None,
                    ..out
                })
            }
            (Target::Node(n), UiAction::Expand { .. }) => Ok(ActionOutcome {
                verified: n.role == Role::ComboBox,
                detail: if n.role == Role::ComboBox {
                    format!("`{}` options: {}", n.name, n.options.join(" | "))
                } else {
                    format!("`{}` does not expand", n.name)
                },
                before: None,
                after: None,
            }),
        }
    }

    fn key_input(&self, target: Option<&ElementId>, input: &KeyInput) -> Result<ActionOutcome> {
        let obs = self.observe()?;
        let id = match target {
            Some(t) => decode_id(t.as_str()),
            None => self
                .focus()
                .element
                .clone()
                .ok_or_else(|| Error::Invalid("nothing has keyboard focus".into()))?,
        };
        let Some(Target::Node(n)) = resolve(&obs, &id) else {
            return Err(Self::not_found(&encode_id(&id)));
        };
        match input {
            KeyInput::Text(text) => match n.role {
                Role::TextBox | Role::TextArea => {
                    let typed: String = text
                        .chars()
                        .filter(|c| *c != '\r' && (*c != '\n' || n.role == Role::TextArea))
                        .collect();
                    let value = format!("{}{typed}", n.value.as_deref().unwrap_or(""));
                    self.set_text(n, &value)
                }
                _ => Err(Error::Invalid(format!("`{}` does not take text", n.name))),
            },
            KeyInput::Keys(chords) => {
                let plain = |k: NamedKey| {
                    chords.len() == 1
                        && chords[0].key == Key::Named(k)
                        && chords[0].modifiers == Modifiers::default()
                };
                let pressable = matches!(n.role, Role::Button | Role::CheckBox | Role::Switch);
                if pressable && (plain(NamedKey::Enter) || plain(NamedKey::Space)) {
                    self.press(n)
                } else {
                    Err(Error::Unsupported(
                        "the test apps have no keyboard shortcuts; only enter or space on a \
                         focused button, check box or switch"
                            .into(),
                    ))
                }
            }
        }
    }

    fn open_app(&self, name: &str) -> Result<ActionOutcome> {
        let obs = self.observe()?;
        let want = name.trim().to_lowercase();
        let w = obs
            .windows
            .iter()
            .find(|w| {
                w.id == want || w.name.to_lowercase() == want || app_title(w).to_lowercase() == want
            })
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "`{name}` is not open, and only the task's apps can be used"
                ))
            })?;
        let mut f = self.focus();
        f.app = Some(w.id.clone());
        f.element = None;
        Ok(ActionOutcome {
            verified: true,
            detail: format!("`{}` is frontmost", w.name),
            before: None,
            after: None,
        })
    }
}

#[cfg(test)]
mod tests;
