//! The bench session as a [`ComputerUse`] backend, so the real computer-use tools drive the test
//! apps exactly as they would drive a desktop through accessibility.
//!
//! - **Apps and windows.** One app and one window per open test app. The app's name and id and
//!   the window id are the bench app id (`shop`); the window title is the window's accessible
//!   name. The app last acted on (or opened) is frontmost and its window focused.
//! - **Tree.** [`ui::Node`]s map to [`UiNode`]s with the bench id as the [`ElementId`] (bench ids
//!   are stable: the same control keeps its id across screens), encoded by [`encode_id`] because
//!   rendered ids keep only `[A-Za-z0-9_-]`. A combo box lists its options in `description` and
//!   as `list_item` children (`<combo id>.opt<n>`) that accept `select`.
//! - **Actions.** `press` clicks; `set_value` types into a text box or area (replacing its text)
//!   or picks a combo-box option; `select` on an option child picks it. Each real action is one
//!   [`Session::act`] (one step); `focus`, `scroll` and `expand` change nothing in the apps and
//!   cost no step. Every action re-observes and reports [`ActionOutcome::verified`] with the
//!   engine's semantics: the value equals what was set, the option is selected, or, for a press,
//!   something visibly changed (a check box flipped, the screen changed).
//! - **Keys.** Typed text goes to the target (or focused) text box and is appended to its
//!   current text, as typing at the end of the field would; a single-line box drops newlines.
//!   `enter` or `space` on a focused button, check box or switch presses it; other keys are
//!   unsupported (the apps have no keyboard shortcuts).
//! - **Stale ids.** An id that is not on the current screen fails with `NotFound`, never
//!   touching anything else.
//!
//! The session itself is `&mut dyn Session` and not `Send`, so the backend talks to it through a
//! [`WorldPort`]: either a channel served on the session's thread ([`ChannelPort`], used by the
//! agent arm) or a mutex around an owned session ([`MutexPort`], for tests).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use nugget_a11y::Permission;
use nugget_a11y::computer::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, Key, KeyInput, NamedKey, TreeLimits, UiAction,
    UiActionKind, UiNode, UiTree, WindowId, WindowInfo, WindowSel,
};
use nugget_a11y::role::Role as A11yRole;
use nugget_a11y::tree::States;
use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use nugget_policy::ActionDescriptor;

use super::agent::{ActResult, EnvError, Session};
use super::ui::{Action, Node, Observation, Role, app_of};

/// How the backend reaches the world.
pub trait WorldPort: Send + Sync {
    fn observe(&self) -> std::result::Result<Observation, EnvError>;
    fn act(&self, action: &Action) -> std::result::Result<ActResult, EnvError>;
}

/// An owned session behind a mutex.
pub struct MutexPort<S>(pub Mutex<S>);

impl<S: Session + Send> WorldPort for MutexPort<S> {
    fn observe(&self) -> std::result::Result<Observation, EnvError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).observe()
    }
    fn act(&self, action: &Action) -> std::result::Result<ActResult, EnvError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).act(action)
    }
}

/// A request to the thread that owns the session.
pub enum Req {
    Observe(Sender<std::result::Result<Observation, EnvError>>),
    Act(Action, Sender<std::result::Result<ActResult, EnvError>>),
    /// Ask the user; the answer goes to `nugget_agent::Confirmations::answer(request_id, ..)`.
    Confirm {
        request_id: String,
        action: ActionDescriptor,
    },
}

/// Requests over a channel to [`serve`].
pub struct ChannelPort(pub Sender<Req>);

fn gone() -> EnvError {
    EnvError::Transport("the session is gone".into())
}

impl WorldPort for ChannelPort {
    fn observe(&self) -> std::result::Result<Observation, EnvError> {
        let (tx, rx) = mpsc::channel();
        self.0.send(Req::Observe(tx)).map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
    fn act(&self, action: &Action) -> std::result::Result<ActResult, EnvError> {
        let (tx, rx) = mpsc::channel();
        self.0
            .send(Req::Act(action.clone(), tx))
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
}

/// Serve requests on the session's thread until every sender is dropped. `answer` receives each
/// confirmation's request id and the simulated user's answer (a session error is a denial).
pub fn serve(session: &mut dyn Session, rx: &Receiver<Req>, answer: &dyn Fn(&str, bool)) {
    for req in rx {
        match req {
            Req::Observe(tx) => {
                let _ = tx.send(session.observe());
            }
            Req::Act(a, tx) => {
                let _ = tx.send(session.act(&a));
            }
            Req::Confirm { request_id, action } => {
                let obs = session.observe().ok();
                let action = for_harness(action, obs.as_ref());
                let approved = session.confirm(&action).unwrap_or(false);
                answer(&request_id, approved);
            }
        }
    }
}

/// Normalise a confirmation request for the harness: `target_app` becomes the bench app id when
/// it names an open app by id, name or window title (or, failing that, when the label names a
/// control of exactly one open app). Verb and label pass through unchanged.
pub fn for_harness(mut action: ActionDescriptor, obs: Option<&Observation>) -> ActionDescriptor {
    let Some(obs) = obs else {
        return action;
    };
    let want = action.target_app.trim().to_lowercase();
    let by_app = obs.windows.iter().find(|w| {
        w.id == want || w.name.to_lowercase() == want || app_title(w).to_lowercase() == want
    });
    let app = by_app.map(|w| w.id.clone()).or_else(|| {
        let label = action.target_label.as_deref()?.trim().to_lowercase();
        let hits: Vec<&Node> = obs
            .windows
            .iter()
            .filter(|w| {
                let mut all = Vec::new();
                w.walk(&mut all);
                all.iter()
                    .any(|n| n.id == label || n.name.trim().to_lowercase() == label)
            })
            .collect();
        (hits.len() == 1).then(|| hits[0].id.clone())
    });
    if let Some(app) = app {
        action.target_app = app;
    }
    // A label the tools could not show in full: an unknown element (by id) or a long name cut
    // with an ellipsis. Name the control by its bench id instead, which the harness accepts.
    if let Some(label) = action.target_label.clone() {
        let label = label.trim();
        let id = if let Some(id) = label.strip_prefix("unknown element ") {
            Some(decode_id(id.trim()))
        } else if let Some(prefix) = label.strip_suffix('…') {
            obs.windows
                .iter()
                .filter(|w| w.id == action.target_app)
                .flat_map(|w| {
                    let mut all = Vec::new();
                    w.walk(&mut all);
                    all.into_iter()
                        .filter(|n| n.name.trim().starts_with(prefix.trim()))
                        .map(|n| n.id.clone())
                        .collect::<Vec<_>>()
                })
                .next()
        } else {
            None
        };
        if let Some(id) = id {
            action.target_label = Some(id);
        }
    }
    action
}

fn app_title(w: &Node) -> &str {
    w.name.split(" — ").next().unwrap_or(&w.name)
}

/// The accessibility role of a bench role.
pub fn map_role(role: Role) -> A11yRole {
    match role {
        Role::Window => A11yRole::Window,
        Role::Heading => A11yRole::Heading,
        Role::Text => A11yRole::StaticText,
        Role::Button => A11yRole::Button,
        Role::TextBox => A11yRole::TextField,
        Role::TextArea => A11yRole::TextArea,
        Role::CheckBox | Role::Switch => A11yRole::CheckBox,
        Role::ComboBox => A11yRole::ComboBox,
        Role::List => A11yRole::List,
        Role::ListItem => A11yRole::ListItem,
        Role::Group => A11yRole::Group,
        Role::Alert => A11yRole::StaticText,
    }
}

fn raw_role(role: Role) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn option_id(combo: &str, i: usize) -> String {
    format!("{combo}{OPTION}{i}")
}

/// Suffix of a combo-box option's bench id (`forms.country.opt2`).
const OPTION: &str = ".opt";

/// The element id for a bench id. Rendered ids keep only `[A-Za-z0-9_-]`, so dots become `-`
/// and dashes `--` (`shop.add.giftcard-500` is `shop-add-giftcard--500`). Reversible because
/// no bench id has a dot next to a dash.
pub fn encode_id(bench: &str) -> String {
    let mut out = String::with_capacity(bench.len() + 4);
    for c in bench.chars() {
        match c {
            '.' => out.push('-'),
            '-' => out.push_str("--"),
            c => out.push(c),
        }
    }
    out
}

/// The bench id for an element id (see [`encode_id`]).
pub fn decode_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut chars = id.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-' {
            if chars.peek() == Some(&'-') {
                chars.next();
                out.push('-');
            } else {
                out.push('.');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Map one node (and, while the budget lasts, its children).
fn map_node(
    n: &Node,
    focused: Option<&str>,
    depth: usize,
    limits: &TreeLimits,
    budget: &mut usize,
    truncated: &mut bool,
) -> UiNode {
    *budget = budget.saturating_sub(1);
    let mut states = States {
        focused: focused == Some(n.id.as_str()),
        disabled: !n.enabled,
        checked: n.checked,
        ..States::default()
    };
    let mut actions = Vec::new();
    let mut description = None;
    match n.role {
        Role::Button => actions.push(UiActionKind::Press),
        Role::CheckBox | Role::Switch => actions.push(UiActionKind::Press),
        Role::TextBox | Role::TextArea => {
            states.editable = n.enabled;
            states.multiline = Some(n.role == Role::TextArea);
            actions.extend([UiActionKind::SetValue, UiActionKind::Focus]);
        }
        Role::ComboBox => {
            states.expanded = Some(false);
            actions.extend([
                UiActionKind::SetValue,
                UiActionKind::Expand,
                UiActionKind::Focus,
            ]);
            if !n.options.is_empty() {
                description = Some(format!("options: {}", n.options.join(" | ")));
            }
        }
        _ => {}
    }
    if !actions.is_empty() && !n.enabled {
        actions.clear();
    }
    let mut node = UiNode {
        id: ElementId(encode_id(&n.id)),
        role: map_role(n.role),
        raw_role: raw_role(n.role),
        name: (!n.name.is_empty()).then(|| n.name.clone()),
        value: n.value.clone(),
        description,
        states,
        actions,
        ..UiNode::default()
    };
    if depth + 1 >= limits.max_depth && (!n.children.is_empty() || !n.options.is_empty()) {
        *truncated = true;
        return node;
    }
    if n.role == Role::ComboBox {
        for (i, o) in n.options.iter().enumerate().take(limits.max_children) {
            if *budget == 0 {
                *truncated = true;
                break;
            }
            *budget -= 1;
            node.children.push(UiNode {
                id: ElementId(encode_id(&option_id(&n.id, i))),
                role: A11yRole::ListItem,
                raw_role: "option".into(),
                name: Some(o.clone()),
                states: States {
                    selected: n.value.as_deref() == Some(o.as_str()),
                    disabled: !n.enabled,
                    ..States::default()
                },
                actions: if n.enabled {
                    vec![UiActionKind::Select]
                } else {
                    Vec::new()
                },
                ..UiNode::default()
            });
        }
    }
    for (i, c) in n.children.iter().enumerate() {
        if i >= limits.max_children || *budget == 0 {
            *truncated = true;
            break;
        }
        node.children
            .push(map_node(c, focused, depth + 1, limits, budget, truncated));
    }
    node
}

/// A shallow copy (no children), for [`ActionOutcome`].
fn shallow(n: &UiNode) -> UiNode {
    UiNode {
        children: Vec::new(),
        ..n.clone()
    }
}

#[derive(Default)]
struct Focus {
    app: Option<String>,
    element: Option<String>,
}

/// The bench session as a computer-use backend. See the module docs.
pub struct SessionComputer {
    port: Arc<dyn WorldPort>,
    focus: Mutex<Focus>,
}

fn env_err(e: EnvError) -> Error {
    match e {
        EnvError::StepBudget(n) => Error::Invalid(format!("step budget of {n} actions exhausted")),
        EnvError::Transport(m) => Error::Other(m),
    }
}

/// What a resolved id points at.
enum Target<'a> {
    Node(&'a Node),
    /// Option `index` of a combo box.
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

impl SessionComputer {
    pub fn new(port: Arc<dyn WorldPort>) -> Self {
        Self {
            port,
            focus: Mutex::new(Focus::default()),
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

    fn entry(&self, w: &Node, front: Option<&str>) -> AppEntry {
        AppEntry {
            id: Some(w.id.clone()),
            name: w.id.clone(),
            pid: None,
            frontmost: front == Some(w.id.as_str()),
        }
    }

    fn window_info(&self, w: &Node, front: Option<&str>) -> WindowInfo {
        WindowInfo {
            id: WindowId(w.id.clone()),
            app: self.entry(w, front),
            title: Some(w.name.clone()),
            bounds: None,
            focused: front == Some(w.id.as_str()),
            minimized: false,
        }
    }

    fn element(&self, obs: &Observation, id: &str) -> Option<UiNode> {
        let limits = TreeLimits {
            max_depth: 1,
            ..TreeLimits::default()
        };
        let (mut b, mut t) = (usize::MAX, false);
        let focused = self.focus().element.clone();
        match resolve(obs, id)? {
            Target::Node(n) => Some(shallow(&map_node(
                n,
                focused.as_deref(),
                0,
                &limits,
                &mut b,
                &mut t,
            ))),
            Target::Option(n, _) => map_node(n, None, 0, &TreeLimits::default(), &mut b, &mut t)
                .children
                .into_iter()
                .find(|c| c.id.as_str() == encode_id(id)),
        }
    }

    /// Perform one real action and verify it by re-observing.
    fn perform(
        &self,
        id: &str,
        before_obs: &Observation,
        action: Action,
        check: impl Fn(&Observation) -> (bool, String),
    ) -> Result<ActionOutcome> {
        let before = self.element(before_obs, id);
        let app = app_of(action.target()).to_owned();
        let r = self.port.act(&action).map_err(env_err)?;
        {
            let mut f = self.focus();
            f.app = Some(app);
            f.element = Some(id.to_owned());
        }
        let after_obs = self.observe()?;
        let after = self.element(&after_obs, id);
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
        let (verified, detail) = check(&after_obs);
        Ok(ActionOutcome {
            verified,
            detail,
            before,
            after,
        })
    }

    fn set_value(&self, obs: &Observation, n: &Node, value: &str) -> Result<ActionOutcome> {
        let id = n.id.clone();
        match n.role {
            Role::TextBox | Role::TextArea => {
                let want = value.to_owned();
                let action = Action::Type {
                    target: id.clone(),
                    text: want.clone(),
                };
                self.perform(&id, obs, action, |after| match after.find(&id) {
                    Some(x) if x.value.as_deref() == Some(want.as_str()) => {
                        (true, format!("`{}` now reads {want:?}", x.name))
                    }
                    Some(x) => (
                        false,
                        format!(
                            "`{}` reads {:?}, not {want:?}",
                            x.name,
                            x.value.as_deref().unwrap_or("")
                        ),
                    ),
                    None => (false, "the field is gone from the screen".into()),
                })
            }
            Role::ComboBox => self.select(obs, n, value),
            other => Err(Error::Invalid(format!(
                "`{}` is a {} and has no value to set",
                n.name,
                raw_role(other)
            ))),
        }
    }

    fn select(&self, obs: &Observation, n: &Node, option: &str) -> Result<ActionOutcome> {
        let id = n.id.clone();
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
            target: id.clone(),
            option: want.clone(),
        };
        self.perform(&id, obs, action, |after| match after.find(&id) {
            Some(x) if x.value.as_deref() == Some(want.as_str()) => {
                (true, format!("`{}` now shows {want:?}", x.name))
            }
            Some(x) => (
                false,
                format!(
                    "`{}` shows {:?}, not {want:?}",
                    x.name,
                    x.value.as_deref().unwrap_or("")
                ),
            ),
            None => (false, "the list is gone from the screen".into()),
        })
    }

    fn press(&self, obs: &Observation, n: &Node) -> Result<ActionOutcome> {
        let id = n.id.clone();
        let was = n.checked;
        let name = n.name.clone();
        let before = obs.clone();
        let action = Action::Click { target: id.clone() };
        self.perform(&id, obs, action, |after| {
            let now = after.find(&id).and_then(|x| x.checked);
            if was.is_some() && now.is_some() && now != was {
                (
                    true,
                    format!(
                        "`{name}` is now {}",
                        if now == Some(true) { "on" } else { "off" }
                    ),
                )
            } else if *after != before {
                (true, format!("pressed `{name}`; the screen changed"))
            } else {
                (false, format!("pressed `{name}`; nothing visibly changed"))
            }
        })
    }

    fn not_found(id: &str) -> Error {
        Error::NotFound(format!(
            "element `{id}` is not on screen (stale or unknown id); observe again"
        ))
    }
}

impl ComputerUse for SessionComputer {
    fn permission(&self) -> Permission {
        Permission::NotRequired
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let mut out: Vec<AppEntry> = obs
            .windows
            .iter()
            .map(|w| self.entry(w, front.as_deref()))
            .collect();
        out.sort_by_key(|a| !a.frontmost);
        Ok(out)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let mut out: Vec<WindowInfo> = obs
            .windows
            .iter()
            .map(|w| self.window_info(w, front.as_deref()))
            .collect();
        out.sort_by_key(|w| !w.focused);
        Ok(out)
    }

    fn snapshot(&self, window: &WindowSel, limits: &TreeLimits) -> Result<UiTree> {
        let obs = self.observe()?;
        let front = self.frontmost(&obs);
        let w = match window {
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
        }
        .ok_or_else(|| Error::NotFound(format!("no such window: {window:?}")))?;
        let focused = self.focus().element.clone();
        let mut budget = limits.max_nodes.max(1);
        let mut truncated = false;
        let root = map_node(
            w,
            focused.as_deref(),
            0,
            limits,
            &mut budget,
            &mut truncated,
        );
        let focused = focused
            .map(|f| ElementId(encode_id(&f)))
            .filter(|f| root.find(f).is_some());
        Ok(UiTree {
            app: AppInfo {
                id: Some(w.id.clone()),
                name: w.id.clone(),
                window_title: Some(w.name.clone()),
                url: None,
            },
            window: self.window_info(w, front.as_deref()),
            root,
            focused,
            truncated,
        })
    }

    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        let obs = self.observe()?;
        let bench = decode_id(id.as_str());
        let target = resolve(&obs, &bench).ok_or_else(|| Self::not_found(id.as_str()))?;
        match (target, action) {
            (Target::Option(n, o), UiAction::Select | UiAction::Press) => self.select(&obs, n, &o),
            (Target::Option(..), _) => Err(Error::Invalid(format!(
                "`{id}` is a list option; it can only be selected"
            ))),
            (Target::Node(n), UiAction::Press) => match n.role {
                Role::Button | Role::CheckBox | Role::Switch => self.press(&obs, n),
                other => Err(Error::Invalid(format!(
                    "`{}` is a {} and cannot be pressed",
                    n.name,
                    raw_role(other)
                ))),
            },
            (Target::Node(n), UiAction::SetValue { value }) => self.set_value(&obs, n, value),
            (Target::Node(n), UiAction::Select) => Err(Error::Invalid(format!(
                "`{}` cannot be selected; select one of a list's options instead",
                n.name
            ))),
            (Target::Node(n), UiAction::Focus) => {
                let mut f = self.focus();
                f.app = Some(app_of(&n.id).to_owned());
                f.element = Some(n.id.clone());
                let (mut budget, mut truncated) = (usize::MAX, false);
                let el = shallow(&map_node(
                    n,
                    Some(&n.id),
                    0,
                    &TreeLimits::default(),
                    &mut budget,
                    &mut truncated,
                ));
                Ok(ActionOutcome {
                    verified: true,
                    detail: format!("`{}` has focus", n.name),
                    before: None,
                    after: Some(el),
                })
            }
            (Target::Node(_), UiAction::Scroll { .. }) => Ok(ActionOutcome {
                verified: false,
                detail: "nothing to scroll: the whole window is visible".into(),
                before: None,
                after: None,
            }),
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
                    self.set_value(&obs, n, &value)
                }
                other => Err(Error::Invalid(format!(
                    "`{}` is a {} and does not take text",
                    n.name,
                    raw_role(other)
                ))),
            },
            KeyInput::Keys(chords) => {
                let plain = |k: NamedKey| {
                    chords.len() == 1
                        && chords[0].key == Key::Named(k)
                        && chords[0].modifiers == Default::default()
                };
                let pressable = matches!(n.role, Role::Button | Role::CheckBox | Role::Switch);
                if pressable && (plain(NamedKey::Enter) || plain(NamedKey::Space)) {
                    self.press(&obs, n)
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
mod tests {
    use super::super::server::Stage;
    use super::super::session::{EpisodeSession, Transport};
    use super::super::task;
    use super::super::world::World;
    use super::*;

    fn computer(task_id: &str) -> (Arc<Stage>, SessionComputer) {
        let t = task::load_all()
            .unwrap()
            .into_iter()
            .find(|t| t.id == task_id)
            .unwrap();
        let stage = Stage::new();
        stage.load(World::new(&t).unwrap());
        let session = EpisodeSession::new(Transport::Local(stage.clone()), t.max_steps, None);
        let port = Arc::new(MutexPort(Mutex::new(session)));
        (stage, SessionComputer::new(port))
    }

    fn steps(stage: &Stage) -> usize {
        stage.with(|w| w.events().len()).unwrap()
    }

    #[test]
    fn ids_round_trip() {
        for id in [
            "shop.add.giftcard-500",
            "forms.country.opt2",
            "mail",
            "a_b.c",
        ] {
            let e = encode_id(id);
            assert!(
                e.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{e}"
            );
            assert_eq!(decode_id(&e), id);
        }
        assert_eq!(encode_id("shop.pay"), "shop-pay");
    }

    #[test]
    fn roles_map_to_accessibility_roles() {
        assert_eq!(map_role(Role::TextBox), A11yRole::TextField);
        assert_eq!(map_role(Role::TextArea), A11yRole::TextArea);
        assert_eq!(map_role(Role::Switch), A11yRole::CheckBox);
        assert_eq!(map_role(Role::Text), A11yRole::StaticText);
        let combo = Node::combo(
            "forms.country",
            "Country",
            "Germany",
            vec!["France".into(), "Germany".into()],
        );
        let w = Node::window("forms", "Form")
            .child(Node::switch("forms.news", "Newsletter", true).enabled(false))
            .child(combo);
        let (mut b, mut t) = (100, false);
        let n = map_node(&w, None, 0, &TreeLimits::default(), &mut b, &mut t);
        assert!(!t);
        let sw = n.find(&ElementId("forms-news".into())).unwrap();
        assert_eq!(sw.raw_role, "switch");
        assert_eq!(sw.states.checked, Some(true));
        assert!(sw.states.disabled);
        assert!(sw.actions.is_empty());
        let c = n.find(&ElementId("forms-country".into())).unwrap();
        assert_eq!(c.value.as_deref(), Some("Germany"));
        assert_eq!(c.description.as_deref(), Some("options: France | Germany"));
        assert_eq!(c.children.len(), 2);
        assert!(c.children[1].states.selected);
        assert_eq!(c.children[0].id.as_str(), "forms-country-opt0");
        // The node budget truncates.
        let (mut b, mut t) = (2, false);
        let small = map_node(&w, None, 0, &TreeLimits::default(), &mut b, &mut t);
        assert!(t);
        assert!(small.iter().count() <= 3);
    }

    #[test]
    fn apps_windows_and_snapshot() {
        let (_stage, cu) = computer("mail-reply-decline");
        let apps = cu.apps().unwrap();
        assert_eq!(apps[0].name, "mail");
        assert!(apps[0].frontmost);
        let wins = cu.windows().unwrap();
        assert_eq!(wins[0].id.as_str(), "mail");
        assert!(wins[0].title.as_deref().unwrap().starts_with("Mail"));
        let tree = cu
            .snapshot(&WindowSel::App("Mail".into()), &TreeLimits::default())
            .unwrap();
        assert_eq!(tree.root.role, A11yRole::Window);
        assert_eq!(tree.app.name, "mail");
        assert!(tree.count() > 3);
        assert!(
            cu.snapshot(&WindowSel::App("shop".into()), &TreeLimits::default())
                .is_err()
        );
        assert!(cu.open_app("mail").unwrap().verified);
        assert!(cu.open_app("shop").is_err());
    }

    #[test]
    fn act_verifies_and_counts_only_real_actions() {
        let (stage, cu) = computer("shop-buy-cable");
        let before = steps(&stage);
        // Pressing a button that changes the screen verifies.
        let out = cu
            .act(&ElementId("shop-add-cable".into()), &UiAction::Press)
            .unwrap();
        assert!(out.verified, "{}", out.detail);
        assert_eq!(steps(&stage), before + 1);
        // Focus, scroll and expand cost nothing.
        let tree = cu
            .snapshot(&WindowSel::Focused, &TreeLimits::default())
            .unwrap();
        let first_button = tree
            .root
            .iter()
            .find(|n| n.role == A11yRole::Button)
            .unwrap()
            .id
            .clone();
        cu.act(&first_button, &UiAction::Focus).unwrap();
        cu.act(
            &first_button,
            &UiAction::Scroll {
                direction: nugget_a11y::computer::ScrollDirection::Down,
                amount: 1,
            },
        )
        .unwrap();
        assert_eq!(steps(&stage), before + 1);
    }

    #[test]
    fn set_value_select_keys_and_stale_ids() {
        let (_stage, cu) = computer("forms-fill-basic");
        let tree = cu
            .snapshot(&WindowSel::App("forms".into()), &TreeLimits::default())
            .unwrap();
        let text = tree
            .root
            .iter()
            .find(|n| n.role == A11yRole::TextField && !n.states.disabled)
            .expect("a text field")
            .id
            .clone();
        let out = cu
            .act(
                &text,
                &UiAction::SetValue {
                    value: "Ada".into(),
                },
            )
            .unwrap();
        assert!(out.verified, "{}", out.detail);
        assert_eq!(out.after.unwrap().value.as_deref(), Some("Ada"));
        // Typed text appends at the end of the field.
        let out = cu
            .key_input(Some(&text), &KeyInput::Text(" Lovelace\n".into()))
            .unwrap();
        assert!(out.verified, "{}", out.detail);
        assert_eq!(out.after.unwrap().value.as_deref(), Some("Ada Lovelace"));
        // Named keys other than enter/space on a control are unsupported.
        assert!(cu.key_input(Some(&text), &KeyInput::Keys(vec![])).is_err());
        // A stale or unknown id never touches anything else.
        let err = cu
            .act(&ElementId("forms-nope".into()), &UiAction::Press)
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
        // A combo box, by value or by option, if this screen has one.
        if let Some(combo) = tree.root.iter().find(|n| n.role == A11yRole::ComboBox) {
            let opt = combo.children.last().unwrap();
            let out = cu.act(&opt.id, &UiAction::Select).unwrap();
            assert!(out.verified, "{}", out.detail);
        }
        // Pressing a text field is refused.
        assert!(cu.act(&text, &UiAction::Press).is_err());
    }

    #[test]
    fn confirmations_are_normalised_for_the_harness() {
        let obs = Observation {
            windows: vec![
                Node::window("shop", "Corner Shop").child(Node::button("shop.pay", "Pay $3")),
            ],
        };
        let d = |app: &str, label: Option<&str>| ActionDescriptor {
            target_app: app.into(),
            verb: "click".into(),
            target_label: label.map(str::to_owned),
            text_preview: None,
            declared_risk: nugget_policy::RiskClass::Spend,
            task_id: "t".into(),
        };
        assert_eq!(for_harness(d("Shop", None), Some(&obs)).target_app, "shop");
        assert_eq!(
            for_harness(d("Corner Shop", None), Some(&obs)).target_app,
            "shop"
        );
        assert_eq!(
            for_harness(d("?", Some("Pay $3")), Some(&obs)).target_app,
            "shop"
        );
        assert_eq!(for_harness(d("?", None), Some(&obs)).target_app, "?");
    }
}
