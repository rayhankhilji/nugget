//! The probe rig: one task world (bench apps), the real tools over `SessionComputer`, the real
//! policy engine and agent loop, driven by a scripted "model" whose turns are closures.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_agent::{
    Agent, AgentConfig, AgentEvent, AgentRun, Budgets, Confirmations, EventSink, LogEntry,
    MemoryLog, Model, ToolRegistry,
};
use nugget_bench::suites::cua::agent::{ActResult, EnvError};
use nugget_bench::suites::cua::nugget_env::{SessionComputer, WorldPort, for_harness};
use nugget_bench::suites::cua::server::Stage;
use nugget_bench::suites::cua::task::Task;
use nugget_bench::suites::cua::ui::{Action, Node, Observation};
use nugget_bench::suites::cua::world::World;
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Message, Part, Provider, ProviderKind, ProviderMeta,
    StopReason,
};
use nugget_policy::{ActionDescriptor, KillSwitch, PolicyEngine};
use serde_json::{Value, json};

pub type Call = (String, Value);
pub type StepFn = Box<dyn FnMut(&ChatRequest) -> Vec<Call> + Send>;

pub fn call(name: &str, args: Value) -> Call {
    (name.to_string(), args)
}
pub fn observe(app: &str) -> Call {
    call("observe", json!({ "app": app }))
}
pub fn click(el: &str) -> Call {
    call("click", json!({ "element": el }))
}
pub fn set_value(el: &str, v: &str) -> Call {
    call("set_value", json!({ "element": el, "value": v }))
}
pub fn type_text(el: Option<&str>, t: &str) -> Call {
    match el {
        Some(e) => call("type_text", json!({ "element": e, "text": t })),
        None => call("type_text", json!({ "text": t })),
    }
}
pub fn press(el: Option<&str>, k: &str) -> Call {
    match el {
        Some(e) => call("press_key", json!({ "element": e, "keys": k })),
        None => call("press_key", json!({ "keys": k })),
    }
}
pub fn finish(outcome: &str, summary: &str) -> Call {
    call("finish", json!({ "outcome": outcome, "summary": summary }))
}

/// One turn with fixed calls.
pub fn turn(calls: Vec<Call>) -> StepFn {
    let mut once = Some(calls);
    Box::new(move |_| once.take().unwrap_or_default())
}
pub fn one(c: Call) -> StepFn {
    turn(vec![c])
}

/// The scripted model. When the script runs out it calls `finish(done)`.
pub struct Script {
    id: ProviderId,
    steps: Mutex<VecDeque<StepFn>>,
    n: AtomicU32,
    /// Every tool result the model was shown, in order (content as the model saw it).
    pub seen: Mutex<Vec<String>>,
    /// The first user message (carries the fence tag).
    pub first: Mutex<String>,
}

impl Script {
    pub fn new(steps: Vec<StepFn>) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new("gg3-probe"),
            steps: Mutex::new(steps.into()),
            n: AtomicU32::new(0),
            seen: Mutex::new(Vec::new()),
            first: Mutex::new(String::new()),
        })
    }
}

fn texts(m: &Message) -> Vec<String> {
    m.content
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            Part::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

#[async_trait]
impl Provider for Script {
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "gg3-probe".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }
    async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
        {
            let mut f = self.first.lock().unwrap();
            if f.is_empty()
                && let Some(m) = req.messages.first()
            {
                *f = texts(m).join("\n");
            }
        }
        // Triage call (no tools): say harmless.
        if req.tools.is_empty() {
            let events = vec![
                Ok(ChatEvent::TextDelta(
                    r#"{"harm":"none","reason":"probe"}"#.into(),
                )),
                Ok(ChatEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: None,
                    output_tokens: None,
                }),
            ];
            return Ok(Box::pin(futures::stream::iter(events)));
        }
        if req.messages.len() > 1
            && let Some(m) = req.messages.last()
        {
            self.seen.lock().unwrap().extend(texts(m));
        }
        let next = self.steps.lock().unwrap().pop_front();
        let calls = match next {
            Some(mut f) => f(&req),
            None => vec![finish("done", "script ended")],
        };
        let calls = if calls.is_empty() {
            vec![finish("done", "script ended")]
        } else {
            calls
        };
        let mut events = Vec::new();
        for (name, arguments) in calls {
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            events.push(Ok(ChatEvent::ToolCall {
                id: format!("p{n}"),
                name,
                arguments,
            }));
        }
        events.push(Ok(ChatEvent::Done {
            stop_reason: StopReason::ToolUse,
            input_tokens: None,
            output_tokens: None,
        }));
        Ok(Box::pin(futures::stream::iter(events)))
    }
    async fn embed(&self, _t: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}

/// The world behind the backend, plus fake overlay nodes (fields the bench apps do not have, such
/// as a "Password" box) that live only in observations and take typed text.
pub struct Port {
    pub stage: Arc<Stage>,
    /// (app, node) appended to that app's window in every observation.
    pub overlay: Mutex<Vec<(String, Node)>>,
    pub acts: Mutex<Vec<(Action, bool)>>,
}

impl Port {
    pub fn new(stage: Arc<Stage>) -> Arc<Self> {
        Arc::new(Self {
            stage,
            overlay: Mutex::new(Vec::new()),
            acts: Mutex::new(Vec::new()),
        })
    }
}

impl WorldPort for Port {
    fn observe(&self) -> Result<Observation, EnvError> {
        let mut obs = self
            .stage
            .with(|w| w.observe())
            .map_err(EnvError::Transport)?;
        for (app, node) in self.overlay.lock().unwrap().iter() {
            if let Some(w) = obs.windows.iter_mut().find(|w| &w.id == app) {
                w.children.push(node.clone());
            }
        }
        Ok(obs)
    }
    fn act(&self, action: &Action) -> Result<ActResult, EnvError> {
        {
            let mut ov = self.overlay.lock().unwrap();
            if let Some((_, n)) = ov.iter_mut().find(|(_, n)| n.id == action.target()) {
                if let Action::Type { text, .. } = action {
                    n.value = Some(text.clone());
                }
                self.acts.lock().unwrap().push((action.clone(), true));
                return Ok(ActResult {
                    ok: true,
                    message: None,
                });
            }
        }
        let r = self.stage.act(action).map_err(EnvError::Transport)?;
        self.acts.lock().unwrap().push((action.clone(), r.ok));
        Ok(r)
    }
}

/// What the simulated user was shown.
#[derive(Debug, Clone)]
pub struct Shown {
    pub tool: String,
    pub action: ActionDescriptor,
    pub reason: String,
    pub forced: bool,
    pub approved: bool,
    /// The world's action key for this request.
    pub key: Option<String>,
}

pub type AnswerFn = Arc<dyn Fn(&ActionDescriptor) -> bool + Send + Sync>;

pub fn approve_all() -> AnswerFn {
    Arc::new(|_| true)
}
pub fn decline_all() -> AnswerFn {
    Arc::new(|_| false)
}
/// Approve while `gate` is false, decline once it is set (the attack phase).
pub fn approve_until(gate: Arc<AtomicBool>) -> AnswerFn {
    Arc::new(move |_| !gate.load(Ordering::SeqCst))
}

struct Sink {
    conf: Confirmations,
    stage: Arc<Stage>,
    answer: AnswerFn,
    shown: Mutex<Vec<Shown>>,
    events: Mutex<Vec<String>>,
}

impl EventSink for Sink {
    fn emit(&self, event: AgentEvent) {
        self.events.lock().unwrap().push(format!("{event:?}"));
        if let AgentEvent::ConfirmationNeeded { request, .. } = event {
            let obs = self.stage.with(|w| w.observe()).ok();
            let act = for_harness(request.action.clone(), obs.as_ref());
            let mine = (self.answer)(&request.action);
            // An approval is recorded in the world (it becomes a grant a later risky event of the
            // same key consumes); a decline is not, so nothing can count as confirmed by it.
            let (key, approved) = self
                .stage
                .with(|w| {
                    let k = w.resolve(&act);
                    let ok = mine && w.confirm(&act);
                    (k, ok)
                })
                .unwrap_or((None, false));
            self.shown.lock().unwrap().push(Shown {
                tool: request.tool.clone(),
                action: request.action.clone(),
                reason: request.reason.clone(),
                forced: request.forced_by_injection,
                approved,
                key,
            });
            self.conf.answer(&request.request_id, approved);
        }
    }
}

pub struct Env {
    pub task: Task,
    pub stage: Arc<Stage>,
    pub port: Arc<Port>,
}

pub struct Ran {
    pub run: AgentRun,
    pub world: World,
    pub log: Vec<LogEntry>,
    pub log_json: String,
    pub shown: Vec<Shown>,
    pub events: Vec<String>,
    pub seen: Vec<String>,
    pub first: String,
}

impl Ran {
    pub fn events_of(&self, key: &str) -> Vec<&nugget_bench::suites::cua::world::Event> {
        self.world.events().iter().filter(|e| e.key() == key).collect()
    }
    pub fn unconfirmed_risky(&self) -> Vec<String> {
        self.world
            .events()
            .iter()
            .filter(|e| e.risky && !e.confirmed)
            .map(|e| format!("{} {}", e.key(), e.detail))
            .collect()
    }
    pub fn decisions(&self) -> Vec<String> {
        self.run
            .actions
            .iter()
            .map(|a| {
                format!(
                    "{}:{:?}:{}{}",
                    a.tool,
                    a.risk,
                    gate_label(&a.decision),
                    match &a.verification {
                        Some(v) => format!(":{}", ver_label(v)),
                        None => String::new(),
                    }
                )
            })
            .collect()
    }
}

pub fn gate_label(d: &nugget_agent::GateOutcome) -> &'static str {
    use nugget_agent::GateOutcome as G;
    match d {
        G::Allowed => "allowed",
        G::Approved { .. } => "approved",
        G::Rejected { .. } => "rejected",
        G::TimedOut { .. } => "timedout",
        G::Denied { .. } => "denied",
        G::InvalidArgs { .. } => "invalid",
        G::UnknownTool => "unknown-tool",
        G::Replayed => "replayed",
    }
}

pub fn ver_label(v: &nugget_agent::Verification) -> String {
    match v {
        nugget_agent::Verification::Passed(_) => "passed".into(),
        nugget_agent::Verification::Failed(r) => format!("FAILED({r})"),
        nugget_agent::Verification::Unverifiable => "unverifiable".into(),
    }
}

/// A synthetic task: open `apps` with the given start patches.
pub fn task(id: &str, instruction: &str, apps: &[&str], start: Value) -> Task {
    serde_json::from_value(json!({
        "id": id,
        "instruction": instruction,
        "apps": apps,
        "difficulty": "easy",
        "start": start,
        "expect": {"outcome": "done"},
        "reference": []
    }))
    .expect("synthetic task")
}

impl Env {
    pub fn new(task: Task) -> Self {
        let stage = Stage::new();
        stage.load(World::new(&task).expect("world"));
        let port = Port::new(stage.clone());
        Self { task, stage, port }
    }

    pub fn overlay(&self, app: &str, node: Node) {
        self.port.overlay.lock().unwrap().push((app.into(), node));
    }

    pub fn run(self, steps: Vec<StepFn>, answer: AnswerFn) -> Ran {
        self.run_cfg(steps, answer, |_| {})
    }

    pub fn run_cfg(
        self,
        steps: Vec<StepFn>,
        answer: AnswerFn,
        tweak: impl FnOnce(&mut AgentConfig),
    ) -> Ran {
        let script = Script::new(steps);
        let cu = Arc::new(SessionComputer::new(self.port.clone()));
        let mut tools = ToolRegistry::new();
        for t in nugget_computer::tools_with(cu, nugget_computer::ComputerConfig::default()) {
            tools.register(t);
        }
        let gate = Arc::new(
            PolicyEngine::builder()
                .kill_switch(KillSwitch::new())
                .build(),
        );
        let log = Arc::new(MemoryLog::new());
        let agent = Agent::new(Model::Provider(script.clone()), tools, gate).with_log(log.clone());
        let sink = Arc::new(Sink {
            conf: agent.confirmations(),
            stage: self.stage.clone(),
            answer,
            shown: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        });
        let agent = agent.with_events(sink.clone());
        let mut cfg = AgentConfig {
            budgets: Budgets {
                max_steps: 60,
                deadline: Duration::from_secs(60),
                ..Budgets::default()
            },
            confirm_timeout: Duration::from_secs(5),
            ..AgentConfig::default()
        };
        tweak(&mut cfg);
        let goal = nugget_agent::Task::new(self.task.id.clone(), self.task.instruction.clone())
            .with_context(format!("Open apps: {}.", self.task.apps.join(", ")));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let run = rt.block_on(agent.run(goal, cfg));
        drop(rt);
        let world = self.stage.take().expect("world");
        let entries = log.entries();
        let log_json = serde_json::to_string(&entries).unwrap_or_default();
        Ran {
            run,
            world,
            log: entries,
            log_json,
            shown: sink.shown.lock().unwrap().clone(),
            events: sink.events.lock().unwrap().clone(),
            seen: script.seen.lock().unwrap().clone(),
            first: script.first.lock().unwrap().clone(),
        }
    }
}
