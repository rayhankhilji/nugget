//! The id mapping and the native session over a fake browser: a `ComputerUse` that renders each
//! app the way Chromium exposes the HTML front end (title bar, `<h1>`, labels before fields,
//! text inside buttons, ` (on)` after switches, a select's options as list items, the status
//! line) under its own ids, and posts actions to the world as the page's script would.

use std::sync::atomic::{AtomicU32, Ordering};

use nugget_a11y::computer::UiActionKind as K;

use super::super::agent::{CuaAgent, Session, TaskBrief};
use super::super::env_native::{NativeSession, register};
use super::super::nugget::NuggetAgent;
use super::super::server::Stage;
use super::super::session::{EpisodeSession, Transport};
use super::super::task::{self, Task};
use super::super::world::World;
use super::*;

fn rn(role: A11yRole, name: &str) -> UiNode {
    UiNode {
        role,
        name: (!name.is_empty()).then(|| name.to_owned()),
        ..UiNode::default()
    }
}

/// What a fake real element stands for.
#[derive(Clone)]
enum Real {
    Control(String),
    Option(String, String),
}

struct FakeEdge {
    stage: Arc<Stage>,
    next: AtomicU32,
    ids: Mutex<HashMap<ElementId, Real>>,
    status: Mutex<HashMap<String, String>>,
    /// Pattern calls that change nothing (a page that ignores them), to exercise the keyboard.
    ignore_set_value: bool,
    typed: Mutex<HashMap<ElementId, String>>,
}

impl FakeEdge {
    fn new(stage: Arc<Stage>) -> Self {
        Self {
            stage,
            next: AtomicU32::new(0),
            ids: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
            ignore_set_value: false,
            typed: Mutex::new(HashMap::new()),
        }
    }

    fn id(&self, what: Option<Real>) -> ElementId {
        let id = ElementId(format!("r{}", self.next.fetch_add(1, Ordering::Relaxed)));
        if let Some(w) = what {
            self.ids.lock().unwrap().insert(id.clone(), w);
        }
        id
    }

    fn node(&self, mut n: UiNode, what: Option<Real>) -> UiNode {
        n.id = self.id(what);
        n
    }

    fn render(&self, b: &Node, out: &mut Vec<UiNode>) {
        let text = |s: &str| self.node(rn(A11yRole::StaticText, s), None);
        let ctl = Some(Real::Control(b.id.clone()));
        let disabled = !b.enabled;
        match b.role {
            Role::Window => unreachable!(),
            Role::Heading => {
                let mut h = self.node(rn(A11yRole::Heading, &b.name), None);
                h.children.push(text(&b.name));
                out.push(h);
            }
            Role::Text | Role::Alert => {
                let mut p = self.node(rn(A11yRole::Paragraph, ""), None);
                p.children.push(text(&b.name));
                out.push(p);
            }
            Role::Button | Role::Switch => {
                let name = match b.checked {
                    Some(on) => format!("{} ({})", b.name, if on { "on" } else { "off" }),
                    None => b.name.clone(),
                };
                let mut n = self.node(rn(A11yRole::Button, &name), ctl);
                n.states.checked = b.checked;
                n.states.disabled = disabled;
                n.actions = vec![K::Press, K::Focus];
                n.children.push(text(&name));
                out.push(n);
            }
            Role::CheckBox => {
                let mut n = self.node(rn(A11yRole::CheckBox, &b.name), ctl);
                n.states.checked = b.checked;
                n.states.disabled = disabled;
                n.actions = vec![K::Press];
                out.push(n);
                out.push(text(&b.name));
            }
            Role::TextBox | Role::TextArea => {
                out.push(text(&b.name));
                let role = if b.role == Role::TextBox {
                    A11yRole::TextField
                } else {
                    A11yRole::TextArea
                };
                let mut n = self.node(rn(role, &b.name), ctl);
                n.value = b.value.clone().filter(|v| !v.is_empty());
                n.states.disabled = disabled;
                n.actions = vec![K::SetValue, K::Focus];
                out.push(n);
            }
            Role::ComboBox => {
                out.push(text(&b.name));
                let mut n = self.node(rn(A11yRole::ComboBox, &b.name), ctl);
                n.value = b.value.clone();
                n.actions = vec![K::Expand, K::Focus];
                let mut list = self.node(rn(A11yRole::List, ""), None);
                for o in &b.options {
                    let mut item = self.node(
                        rn(A11yRole::ListItem, o),
                        Some(Real::Option(b.id.clone(), o.clone())),
                    );
                    item.actions = vec![K::Select];
                    item.states.offscreen = true;
                    list.children.push(item);
                }
                n.children.push(list);
                out.push(n);
            }
            Role::List | Role::ListItem | Role::Group => {
                let role = match b.role {
                    Role::List => A11yRole::List,
                    Role::ListItem => A11yRole::ListItem,
                    _ => A11yRole::Group,
                };
                let mut n = self.node(rn(role, &b.name), None);
                if b.role == Role::Group {
                    let mut h = self.node(rn(A11yRole::Heading, &b.name), None);
                    h.children.push(text(&b.name));
                    n.children.push(h);
                }
                for c in &b.children {
                    self.render(c, &mut n.children);
                }
                out.push(n);
            }
        }
    }

    fn page(&self, app: &str) -> (String, UiNode) {
        let w = self.stage.with(|w| w.window(app)).unwrap();
        let title = w.as_ref().map_or("Closed".to_owned(), |w| w.name.clone());
        let mut root = self.node(rn(A11yRole::Window, &title), None);
        for b in ["Minimize", "Close"] {
            let mut n = self.node(rn(A11yRole::Button, b), None);
            n.actions = vec![K::Press];
            root.children.push(n);
        }
        let mut doc = self.node(rn(A11yRole::WebArea, &title), None);
        if let Some(w) = &w {
            let mut main = self.node(rn(A11yRole::Group, &w.name), None);
            let mut h1 = self.node(rn(A11yRole::Heading, &w.name), None);
            h1.children
                .push(self.node(rn(A11yRole::StaticText, &w.name), None));
            main.children.push(h1);
            for c in &w.children {
                self.render(c, &mut main.children);
            }
            doc.children.push(main);
        }
        let mut status = self.node(rn(A11yRole::Paragraph, ""), None);
        if let Some(s) = self.status.lock().unwrap().get(app) {
            status
                .children
                .push(self.node(rn(A11yRole::StaticText, s), None));
        }
        doc.children.push(status);
        root.children.push(doc);
        (title, root)
    }

    fn post(&self, action: Action) -> Result<ActionOutcome> {
        let app = action.app().to_owned();
        let r = self.stage.act(&action).map_err(Error::Other)?;
        let mut status = self.status.lock().unwrap();
        match r.message {
            Some(m) if !r.ok => {
                status.insert(app, m);
            }
            _ => {
                status.remove(&app);
            }
        }
        Ok(ActionOutcome {
            verified: r.ok,
            detail: String::new(),
            before: None,
            after: None,
        })
    }

    fn win(&self, i: usize, app: &str) -> WindowInfo {
        let (title, _) = self.page(app);
        WindowInfo {
            id: WindowId(format!("hwnd-{app}")),
            app: AppEntry {
                id: Some("msedge.exe".into()),
                name: "msedge".into(),
                pid: Some(100 + i as u32),
                frontmost: false,
            },
            title: Some(title),
            bounds: None,
            focused: false,
            minimized: false,
        }
    }
}

impl ComputerUse for FakeEdge {
    fn permission(&self) -> Permission {
        Permission::NotRequired
    }
    fn apps(&self) -> Result<Vec<AppEntry>> {
        Ok(Vec::new())
    }
    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(super::super::apps::APP_IDS
            .iter()
            .enumerate()
            .map(|(i, a)| self.win(i, a))
            .collect())
    }
    fn snapshot(&self, window: &WindowSel, _limits: &TreeLimits) -> Result<UiTree> {
        let WindowSel::Id(id) = window else {
            return Err(Error::Invalid("by id only".into()));
        };
        let app = id.as_str().trim_start_matches("hwnd-").to_owned();
        let i = super::super::apps::APP_IDS
            .iter()
            .position(|a| *a == app)
            .ok_or_else(|| Error::NotFound(app.clone()))?;
        let (_, root) = self.page(&app);
        Ok(UiTree {
            app: AppInfo {
                id: None,
                name: "msedge".into(),
                window_title: None,
                url: None,
            },
            window: self.win(i, &app),
            root,
            focused: None,
            truncated: false,
        })
    }
    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome> {
        let what = self
            .ids
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        let done = || {
            Ok(ActionOutcome {
                verified: true,
                detail: String::new(),
                before: None,
                after: None,
            })
        };
        match (what, action) {
            (Real::Control(t), UiAction::Press) => self.post(Action::Click { target: t }),
            (Real::Control(_), UiAction::SetValue { .. }) if self.ignore_set_value => done(),
            (Real::Control(t), UiAction::SetValue { value }) => self.post(Action::Type {
                target: t,
                text: value.clone(),
            }),
            (Real::Option(t, o), UiAction::Select) => self.post(Action::Select {
                target: t,
                option: o,
            }),
            (_, UiAction::Focus | UiAction::Scroll { .. }) => done(),
            _ => Err(Error::Unsupported("fake".into())),
        }
    }
    fn key_input(&self, target: Option<&ElementId>, input: &KeyInput) -> Result<ActionOutcome> {
        // A text field: select-all, type, Tab commits (the `change` event).
        let id = target
            .cloned()
            .or_else(|| self.typed.lock().unwrap().keys().next().cloned())
            .ok_or_else(|| Error::Invalid("no focus".into()))?;
        match input {
            KeyInput::Text(t) => {
                self.typed.lock().unwrap().insert(id, t.clone());
                Ok(ActionOutcome {
                    verified: true,
                    detail: String::new(),
                    before: None,
                    after: None,
                })
            }
            KeyInput::Keys(k) if k.iter().any(|c| c.key == Key::Named(NamedKey::Tab)) => {
                let text = self.typed.lock().unwrap().remove(&id).unwrap_or_default();
                let Some(Real::Control(t)) = self.ids.lock().unwrap().get(&id).cloned() else {
                    return Err(Error::Invalid("not a field".into()));
                };
                self.post(Action::Type { target: t, text })
            }
            KeyInput::Keys(_) => Ok(ActionOutcome {
                verified: true,
                detail: String::new(),
                before: None,
                after: None,
            }),
        }
    }
    fn open_app(&self, _name: &str) -> Result<ActionOutcome> {
        Err(Error::Unsupported("fake".into()))
    }
}

fn load(id: &str) -> Task {
    task::load_all()
        .unwrap()
        .into_iter()
        .find(|t| t.id == id)
        .unwrap()
}

fn pids() -> Arc<dyn WindowMap> {
    Arc::new(
        super::super::apps::APP_IDS
            .iter()
            .enumerate()
            .map(|(i, a)| ((*a).to_owned(), 100 + i as u32))
            .collect::<HashMap<String, u32>>(),
    )
}

fn setup(
    id: &str,
    fake: impl FnOnce(Arc<Stage>) -> FakeEdge,
) -> (Task, Arc<Stage>, Arc<UiaDriver>) {
    let t = load(id);
    let stage = Stage::new();
    stage.load(World::new(&t).unwrap());
    let sys: Arc<dyn ComputerUse> = Arc::new(fake(stage.clone()));
    let driver = Arc::new(UiaDriver::new(sys, pids()).with_settle(Duration::from_millis(300)));
    (t, stage, driver)
}

#[test]
fn names_match_exactly_or_with_suffixes_and_cuts() {
    assert!(name_matches("Pay  $3", Some("Pay $3")));
    assert!(name_matches("Firewall", Some("Firewall (on)")));
    assert!(!name_matches("Fire", Some("Firewall (on)")));
    assert!(!name_matches("Pay", Some("Pay $3")));
    assert!(name_matches(
        "A very long message body that goes on",
        Some("A very long message…")
    ));
    assert!(!name_matches("Short", Some("Sh…")));
    assert!(!name_matches("x", None));
}

#[test]
fn matching_skips_decoys_and_checks_state() {
    let bench = Node::window("forms", "Change of address")
        .child(Node::text("forms.intro", "Email"))
        .child(Node::textbox("forms.email", "Email", "a@b.c"))
        .child(Node::switch("forms.news", "Newsletter", true))
        .child(Node::combo(
            "forms.country",
            "Country",
            "Germany",
            vec!["France".into(), "Germany".into()],
        ))
        .child(Node::button("forms.next", "Next"));
    let stage = Stage::new();
    let fake = FakeEdge::new(stage);
    let mut real = rn(A11yRole::Window, "Change of address");
    for b in &bench.children {
        fake.render(b, &mut real.children);
    }
    let m = match_tree(&bench, &real);
    assert!(m.consistent(), "{m:?}");
    // The label before the field is skipped, the field itself matched.
    assert_eq!(m.nodes["forms.email"].role, A11yRole::TextField);
    assert_eq!(m.nodes["forms.intro"].role, A11yRole::StaticText);
    assert_eq!(
        m.nodes["forms.news"].name.as_deref(),
        Some("Newsletter (on)")
    );
    assert_eq!(m.options.len(), 2);
    assert_eq!(
        m.options["forms.country.opt1"].name.as_deref(),
        Some("Germany")
    );
    // The button's own text is not taken for a later node.
    assert_eq!(m.nodes["forms.next"].role, A11yRole::Button);

    // An old page: the field does not show the bench value yet.
    let newer = Node::window("forms", "Change of address").child(Node::textbox(
        "forms.email",
        "Email",
        "new@b.c",
    ));
    let m = match_tree(&newer, &real);
    assert_eq!(m.stale, vec!["forms.email".to_owned()]);
    assert!(!m.consistent());
    // A control that is not there at all.
    let other =
        Node::window("forms", "Change of address").child(Node::button("forms.back", "Back"));
    let m = match_tree(&other, &real);
    assert_eq!(m.missing, vec!["forms.back".to_owned()]);
}

#[test]
fn presented_tree_uses_bench_ids_and_real_content() {
    let bench = Node::window("settings", "Settings — laptop")
        .child(Node::switch("settings.firewall", "Firewall", true))
        .child(Node::combo(
            "settings.theme",
            "Theme",
            "Dark",
            vec!["Light".into(), "Dark".into()],
        ))
        .child(Node::button("settings.gone", "Not rendered"));
    let stage = Stage::new();
    let fake = FakeEdge::new(stage);
    let mut real = rn(A11yRole::Window, "Settings — laptop");
    for b in &bench.children[..2] {
        fake.render(b, &mut real.children);
    }
    let m = match_tree(&bench, &real);
    let tree = present(&bench, &m, Some("Settings — laptop"));
    assert_eq!(tree.id.as_str(), "settings");
    let fw = tree.find(&ElementId("settings-firewall".into())).unwrap();
    assert_eq!(fw.name.as_deref(), Some("Firewall (on)"));
    assert_eq!(fw.states.checked, Some(true));
    assert!(fw.actions.contains(&K::Press));
    let theme = tree.find(&ElementId("settings-theme".into())).unwrap();
    assert_eq!(theme.children.len(), 2);
    assert_eq!(theme.children[1].id.as_str(), "settings-theme-opt1");
    assert!(theme.children[1].states.selected);
    assert!(theme.actions.contains(&K::SetValue));
    // What has no real counterpart is not shown.
    assert!(tree.find(&ElementId("settings-gone".into())).is_none());
    // Real ids never leak.
    assert!(tree.iter().all(|n| !n.id.as_str().starts_with('r')));
}

#[test]
fn driver_acts_through_the_real_window() {
    let (t, stage, driver) = setup("forms-fill-basic", FakeEdge::new);
    let mut session = EpisodeSession::new(Transport::Local(stage.clone()), t.max_steps, None);
    let mut s = NativeSession::new(&mut session, stage.clone(), driver.clone(), 3);
    let obs = s.observe().unwrap();
    let field = {
        let mut all = Vec::new();
        obs.windows[0].walk(&mut all);
        all.into_iter()
            .find(|n| n.role == Role::TextBox && n.enabled)
            .unwrap()
            .id
            .clone()
    };
    let r = s
        .act(&Action::Type {
            target: field.clone(),
            text: "Ada".into(),
        })
        .unwrap();
    assert!(r.ok, "{r:?}");
    let now = stage.with(|w| w.observe()).unwrap();
    assert_eq!(now.find(&field).unwrap().value.as_deref(), Some("Ada"));
    // An element that is not on screen fails without touching anything.
    let r = s
        .act(&Action::Click {
            target: "forms.nope".into(),
        })
        .unwrap();
    assert!(!r.ok);
    assert_eq!(s.steps_taken(), 2);
    // The step budget holds.
    s.act(&Action::Click {
        target: "forms.nope".into(),
    })
    .unwrap();
    assert!(
        s.act(&Action::Click {
            target: "forms.nope".into()
        })
        .is_err()
    );
}

#[test]
fn keyboard_fallback_when_the_page_ignores_set_value() {
    let (t, stage, driver) = setup("forms-fill-basic", |s| FakeEdge {
        ignore_set_value: true,
        ..FakeEdge::new(s)
    });
    let mut session = EpisodeSession::new(Transport::Local(stage.clone()), t.max_steps, None);
    let mut s = NativeSession::new(&mut session, stage.clone(), driver.clone(), 5);
    let obs = s.observe().unwrap();
    let mut all = Vec::new();
    obs.windows[0].walk(&mut all);
    let field = all
        .into_iter()
        .find(|n| n.role == Role::TextBox && n.enabled)
        .unwrap()
        .id
        .clone();
    let r = s
        .act(&Action::Type {
            target: field.clone(),
            text: "Grace".into(),
        })
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(driver.fallbacks.load(Ordering::Relaxed), 1);
    let now = stage.with(|w| w.observe()).unwrap();
    assert_eq!(now.find(&field).unwrap().value.as_deref(), Some("Grace"));
}

/// The whole nugget arm, its tools reading the fake real windows: every action goes through the
/// fake's accessibility actions, and the scripted model finds its targets by bench id.
fn nugget_arm(id: &str) {
    let (t, stage, driver) = setup(id, FakeEdge::new);
    let _reg = register(driver.clone());
    let mut agent = NuggetAgent::new(std::slice::from_ref(&t));
    let mut session = EpisodeSession::new(Transport::Local(stage.clone()), t.max_steps, None);
    let mut s = NativeSession::new(&mut session, stage.clone(), driver, t.max_steps);
    let outcome = agent.run(&TaskBrief::from(&t), &mut s);
    let steps = s.steps_taken();
    let world = stage.take().unwrap();
    let score = task::score(&t, &world, &outcome);
    assert!(score.success, "{id}: {outcome:?} {:?}", score.reasons);
    assert!(steps > 0);
    assert_eq!(score.unconfirmed_risky_actions, 0);
}

#[test]
fn nugget_arm_fills_a_form_over_the_real_tree() {
    nugget_arm("forms-fill-basic");
}

#[test]
fn nugget_arm_organises_files_over_the_real_tree() {
    let id = task::load_all()
        .unwrap()
        .into_iter()
        .find(|t| t.app() == "files" && t.injection.is_none() && t.user.deny.is_empty())
        .unwrap()
        .id;
    nugget_arm(&id);
}
