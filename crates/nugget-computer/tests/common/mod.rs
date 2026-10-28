//! Test doubles: an in-memory `ComputerUse` with scripted effects, a scripted model, and helpers.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use nugget_a11y::computer::{Key, NamedKey};
use nugget_a11y::role::Role;
use nugget_a11y::tree::States;
use nugget_a11y::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, KeyInput, Permission, TreeLimits, UiAction,
    UiActionKind, UiNode, UiTree, WindowId, WindowInfo, WindowSel,
};
use nugget_agent::{Snapshot, Tool, ToolCtx, ToolResult, Verification};
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------------------------
// Tree building

pub fn n(id: &str, role: Role, name: &str) -> UiNode {
    let actions = match role {
        Role::Button | Role::Link | Role::MenuItem | Role::CheckBox => vec![UiActionKind::Press],
        Role::TextField | Role::TextArea | Role::SearchField => {
            vec![UiActionKind::SetValue, UiActionKind::Focus]
        }
        Role::ListItem | Role::Tab => vec![UiActionKind::Select],
        Role::ComboBox => vec![UiActionKind::Expand],
        _ => vec![],
    };
    let editable = matches!(role, Role::TextField | Role::TextArea | Role::SearchField);
    UiNode {
        id: ElementId(id.into()),
        role,
        name: (!name.is_empty()).then(|| name.to_string()),
        states: States {
            editable,
            multiline: match role {
                Role::TextArea => Some(true),
                Role::TextField => Some(false),
                _ => None,
            },
            ..States::default()
        },
        actions,
        // Editable fields read back as empty rather than absent, as on the real platforms.
        value: editable.then(String::new),
        ..UiNode::default()
    }
}

pub trait NodeExt {
    fn kids(self, children: Vec<UiNode>) -> UiNode;
    fn value(self, v: &str) -> UiNode;
    fn secure(self) -> UiNode;
    fn raw(self, raw: &str) -> UiNode;
    fn offscreen(self) -> UiNode;
}

impl NodeExt for UiNode {
    fn kids(mut self, children: Vec<UiNode>) -> UiNode {
        self.children = children;
        self
    }
    fn value(mut self, v: &str) -> UiNode {
        self.value = Some(v.into());
        self
    }
    fn secure(mut self) -> UiNode {
        self.states.secure = true;
        self
    }
    fn raw(mut self, raw: &str) -> UiNode {
        self.raw_role = raw.into();
        self
    }
    fn offscreen(mut self) -> UiNode {
        self.states.offscreen = true;
        self
    }
}

pub fn window(id: &str, app: &str, app_id: Option<&str>, title: &str, kids: Vec<UiNode>) -> UiTree {
    let entry = AppEntry {
        id: app_id.map(String::from),
        name: app.into(),
        pid: Some(42),
        frontmost: false,
    };
    UiTree {
        app: AppInfo {
            id: app_id.map(String::from),
            name: app.into(),
            window_title: Some(title.into()),
            url: None,
        },
        window: WindowInfo {
            id: WindowId(id.into()),
            app: entry,
            title: Some(title.into()),
            bounds: None,
            focused: false,
            minimized: false,
        },
        root: n(&format!("{id}root"), Role::Window, title).kids(kids),
        focused: None,
        truncated: false,
    }
}

/// A compose window with a bit of everything.
pub fn mail() -> UiTree {
    let mut t = window(
        "w1",
        "Mail",
        Some("com.apple.mail"),
        "New Message",
        vec![
            n("eto", Role::TextField, "To"),
            n("esubject", Role::TextField, "Subject"),
            n("ebody", Role::TextArea, "Body"),
            n("epw", Role::TextField, "Password").secure(),
            n("esend", Role::Button, "Send"),
            n("edelete", Role::Button, "Delete draft"),
            n("ebuy", Role::Button, "Place order"),
            n("eperm", Role::Button, "Grant permission"),
            n("", Role::Group, "").kids(vec![n("ebold", Role::Button, "Bold")]),
            n("eprio", Role::ComboBox, "Priority")
                .value("Normal")
                .kids(vec![
                    n("elow", Role::ListItem, "Low"),
                    n("enormal", Role::ListItem, "Normal"),
                    n("ehigh", Role::ListItem, "High"),
                ]),
            n("elist", Role::List, "Messages").kids(vec![
                n("em1", Role::ListItem, "Hello"),
                n("em2", Role::ListItem, "World"),
            ]),
            n("edlg", Role::Dialog, "Delete 3 files?").kids(vec![
                n("etxt", Role::StaticText, "This cannot be undone."),
                n("eok", Role::Button, "OK"),
            ]),
        ],
    );
    t.focused = Some(ElementId("eto".into()));
    t
}

pub fn terminal() -> UiTree {
    let mut t = window(
        "w2",
        "Terminal",
        Some("com.apple.Terminal"),
        "bash",
        vec![n("eterm", Role::TextArea, "shell").raw("AXTextArea")],
    );
    t.focused = Some(ElementId("eterm".into()));
    t
}

// ---------------------------------------------------------------------------------------------
// Fake backend

type Effect = Box<dyn Fn(&mut World) + Send>;
pub type SnapshotHook = Box<dyn Fn(&mut World, usize) + Send>;

pub struct World {
    pub windows: Vec<UiTree>,
    /// Index into `windows` of the focused one.
    pub front: usize,
    pub snapshots: usize,
    pub acts: Vec<String>,
    /// Overrides `verified` on every outcome.
    pub force_verified: Option<bool>,
    /// Typing does nothing (but reports success when `force_verified` says so).
    pub drop_typing: bool,
    /// Set values are written as this instead.
    pub wrong_value: Option<String>,
    pub screenshot: Option<Vec<u8>>,
    /// Runs on each snapshot, with the snapshot count.
    pub on_snapshot: Option<SnapshotHook>,
}

impl World {
    pub fn node_mut(&mut self, id: &str) -> Option<&mut UiNode> {
        fn find<'a>(n: &'a mut UiNode, id: &str) -> Option<&'a mut UiNode> {
            if n.id.0 == id {
                return Some(n);
            }
            n.children.iter_mut().find_map(|c| find(c, id))
        }
        self.windows.iter_mut().find_map(|t| find(&mut t.root, id))
    }

    pub fn node(&self, id: &str) -> Option<UiNode> {
        let id = ElementId(id.into());
        self.windows.iter().find_map(|t| t.find(&id).cloned())
    }

    pub fn remove(&mut self, id: &str) {
        fn rm(n: &mut UiNode, id: &str) {
            n.children.retain(|c| c.id.0 != id);
            n.children.iter_mut().for_each(|c| rm(c, id));
        }
        self.windows.iter_mut().for_each(|t| rm(&mut t.root, id));
    }

    pub fn close(&mut self, window: &str) {
        self.windows.retain(|t| t.window.id.0 != window);
        self.front = 0;
    }

    fn focused_id(&self) -> Option<ElementId> {
        self.windows.get(self.front)?.focused.clone()
    }
}

pub struct FakeComputer {
    pub world: Mutex<World>,
    effects: Mutex<HashMap<String, Effect>>,
}

impl FakeComputer {
    pub fn new(windows: Vec<UiTree>) -> Arc<Self> {
        Arc::new(Self {
            world: Mutex::new(World {
                windows,
                front: 0,
                snapshots: 0,
                acts: Vec::new(),
                force_verified: None,
                drop_typing: false,
                wrong_value: None,
                screenshot: None,
                on_snapshot: None,
            }),
            effects: Mutex::new(HashMap::new()),
        })
    }

    /// What pressing `key` (an element id, or `key:enter` for a key) does.
    pub fn on(&self, key: &str, f: impl Fn(&mut World) + Send + 'static) {
        self.effects.lock().unwrap().insert(key.into(), Box::new(f));
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        f(&mut self.world.lock().unwrap())
    }

    pub fn acts(&self) -> Vec<String> {
        self.with(|w| w.acts.clone())
    }

    fn run_effect(&self, key: &str, w: &mut World) -> bool {
        match self.effects.lock().unwrap().get(key) {
            Some(f) => {
                f(w);
                true
            }
            None => false,
        }
    }

    fn outcome(w: &World, verified: bool, detail: &str) -> ActionOutcome {
        ActionOutcome {
            verified: w.force_verified.unwrap_or(verified),
            detail: detail.into(),
            before: None,
            after: None,
        }
    }
}

impl ComputerUse for FakeComputer {
    fn permission(&self) -> Permission {
        Permission::Granted
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        Ok(self.with(|w| w.windows.iter().map(|t| t.window.app.clone()).collect()))
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(self.with(|w| {
            let mut out: Vec<WindowInfo> = Vec::new();
            for (i, t) in w.windows.iter().enumerate() {
                let mut info = t.window.clone();
                info.focused = i == w.front;
                if info.focused {
                    out.insert(0, info);
                } else {
                    out.push(info);
                }
            }
            out
        }))
    }

    fn snapshot(&self, sel: &WindowSel, _limits: &TreeLimits) -> Result<UiTree> {
        let mut w = self.world.lock().unwrap();
        w.snapshots += 1;
        if let Some(f) = w.on_snapshot.take() {
            let count = w.snapshots;
            f(&mut w, count);
            w.on_snapshot = Some(f);
        }
        let t = match sel {
            WindowSel::Focused => w.windows.get(w.front),
            WindowSel::Id(id) => w.windows.iter().find(|t| &t.window.id == id),
            WindowSel::App(a) => w
                .windows
                .iter()
                .find(|t| t.app.name.eq_ignore_ascii_case(a)),
        };
        t.cloned()
            .ok_or_else(|| Error::NotFound(format!("window {sel:?}")))
    }

    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        let mut w = self.world.lock().unwrap();
        w.acts.push(format!("{}:{}", action.kind().as_str(), id));
        if w.node(id.as_str()).is_none() {
            return Err(Error::NotFound(format!("element {id}")));
        }
        match action {
            UiAction::Press => {
                let changed = self.run_effect(id.as_str(), &mut w);
                Ok(Self::outcome(&w, changed, "pressed"))
            }
            UiAction::SetValue { value } => {
                let node = w.node(id.as_str()).unwrap();
                if node.states.secure {
                    return Err(Error::Invalid("secure field".into()));
                }
                let written = w.wrong_value.clone().unwrap_or_else(|| value.clone());
                w.node_mut(id.as_str()).unwrap().value = Some(written.clone());
                let ok = &written == value;
                Ok(Self::outcome(&w, ok, "value set"))
            }
            UiAction::Select => {
                let name = w.node(id.as_str()).unwrap().name;
                // Deselect siblings and set the parent's value.
                fn walk(n: &mut UiNode, id: &str, name: &Option<String>) {
                    if n.children.iter().any(|c| c.id.0 == id) {
                        for c in &mut n.children {
                            c.states.selected = c.id.0 == id;
                        }
                        if n.role == Role::ComboBox {
                            n.value = name.clone();
                        }
                    }
                    n.children.iter_mut().for_each(|c| walk(c, id, name));
                }
                for t in &mut w.windows {
                    walk(&mut t.root, id.as_str(), &name);
                }
                Ok(Self::outcome(&w, true, "selected"))
            }
            UiAction::Expand { expand } => {
                w.node_mut(id.as_str()).unwrap().states.expanded = Some(*expand);
                self.run_effect(&format!("expand:{id}"), &mut w);
                Ok(Self::outcome(&w, true, "expanded"))
            }
            UiAction::Focus | UiAction::Scroll { .. } => Ok(Self::outcome(&w, true, "ok")),
        }
    }

    fn key_input(&self, target: Option<&ElementId>, input: &KeyInput) -> Result<ActionOutcome> {
        let mut w = self.world.lock().unwrap();
        let id = target.cloned().or_else(|| w.focused_id());
        match input {
            KeyInput::Text(text) => {
                let id = id.ok_or_else(|| Error::NotFound("nothing focused".into()))?;
                w.acts.push(format!("type:{id}"));
                let node = w
                    .node(id.as_str())
                    .ok_or_else(|| Error::NotFound(format!("element {id}")))?;
                if node.states.secure {
                    return Err(Error::Invalid("secure field".into()));
                }
                if !w.drop_typing {
                    let n = w.node_mut(id.as_str()).unwrap();
                    let mut v = n.value.clone().unwrap_or_default();
                    v.push_str(text);
                    n.value = Some(v);
                }
                let ok = !w.drop_typing;
                Ok(Self::outcome(&w, ok, "typed"))
            }
            KeyInput::Keys(chords) => {
                let mut any = false;
                for c in chords {
                    w.acts.push(format!("key:{c}"));
                    let k = match c.key {
                        Key::Named(NamedKey::Enter) => "key:enter".to_string(),
                        _ => format!("key:{c}"),
                    };
                    any |= self.run_effect(&k, &mut w);
                }
                Ok(Self::outcome(&w, any, "keys sent"))
            }
        }
    }

    fn open_app(&self, name: &str) -> Result<ActionOutcome> {
        let mut w = self.world.lock().unwrap();
        w.acts.push(format!("open:{name}"));
        match w
            .windows
            .iter()
            .position(|t| t.app.name.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                w.front = i;
                Ok(Self::outcome(&w, true, "frontmost"))
            }
            None => Ok(Self::outcome(&w, false, "did not open")),
        }
    }

    fn screenshot(&self, _window: &WindowSel) -> Result<Option<Vec<u8>>> {
        Ok(self.with(|w| w.screenshot.clone()))
    }
}

// ---------------------------------------------------------------------------------------------
// Tools

pub struct Kit {
    pub cu: Arc<FakeComputer>,
    pub tools: Vec<Arc<dyn Tool>>,
}

impl Kit {
    pub fn new(windows: Vec<UiTree>) -> Self {
        Self::with_config(windows, nugget_computer::ComputerConfig::default())
    }

    pub fn with_config(windows: Vec<UiTree>, config: nugget_computer::ComputerConfig) -> Self {
        let cu = FakeComputer::new(windows);
        let tools = nugget_computer::tools_with(cu.clone(), config);
        Self { cu, tools }
    }

    pub fn tool(&self, name: &str) -> Arc<dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .clone()
    }

    pub async fn call(&self, name: &str, args: Value) -> ToolResult {
        self.tool(name).call(args, &ctx()).await
    }

    /// Observe the focused window so the cache knows its elements.
    pub async fn look(&self) -> ToolResult {
        self.call("observe", serde_json::json!({})).await
    }

    /// What the agent loop does for a mutating call: observe, call, observe, verify.
    pub async fn act(&self, name: &str, args: Value) -> (ToolResult, Verification) {
        let t = self.tool(name);
        let c = ctx();
        let before: Snapshot = t.observe(&args, &c).await;
        let r = t.call(args.clone(), &c).await;
        let after = t.observe(&args, &c).await;
        let v = t.verify(&args, &before, &after, &r);
        (r, v)
    }
}

pub fn ctx() -> ToolCtx {
    ctx_with(CancellationToken::new())
}

pub fn ctx_with(cancel: CancellationToken) -> ToolCtx {
    ToolCtx {
        task_id: "t".into(),
        action_id: "a".into(),
        step: 1,
        cancel,
    }
}

// ---------------------------------------------------------------------------------------------
// Scripted model (the minimal pattern of nugget-agent's own tests)

pub struct Turn {
    pub calls: Vec<(String, String, Value)>,
}

pub fn turn(calls: Vec<(&str, &str, Value)>) -> Turn {
    Turn {
        calls: calls
            .into_iter()
            .map(|(id, name, args)| (id.to_string(), name.to_string(), args))
            .collect(),
    }
}

pub struct Scripted {
    id: ProviderId,
    turns: Mutex<VecDeque<Turn>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl Scripted {
    pub fn new(turns: Vec<Turn>) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new("scripted"),
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "scripted".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.requests.lock().unwrap().push(req);
        let Some(t) = self.turns.lock().unwrap().pop_front() else {
            return Err(Error::Provider("script exhausted".into()));
        };
        let mut events = Vec::new();
        for (id, name, arguments) in t.calls {
            events.push(Ok(ChatEvent::ToolCall {
                id,
                name,
                arguments,
            }));
        }
        events.push(Ok(ChatEvent::Done {
            stop_reason: StopReason::ToolUse,
            input_tokens: Some(100),
            output_tokens: Some(20),
        }));
        Ok(Box::pin(stream::iter(events)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("embed".into()))
    }
}

pub const SHORT: Duration = Duration::from_millis(20);
