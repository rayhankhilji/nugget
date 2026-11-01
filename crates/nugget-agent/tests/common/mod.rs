//! Test doubles: a scripted model, a configurable policy gate, and tools with observable state.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use nugget_agent::{
    ActionTarget, Agent, AgentConfig, AgentEvent, AgentRun, Model, Snapshot, Task, Tool, ToolCtx,
    ToolRegistry, ToolResult, Verification,
};
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderKind, ProviderMeta, StopReason,
};
use nugget_policy::{ActionDescriptor, Decision, PolicyGate, RiskClass};
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub fn cfg() -> AgentConfig {
    AgentConfig {
        confirm_timeout: Duration::from_secs(5),
        halt_poll: Duration::from_millis(5),
        ..AgentConfig::default()
    }
}

// --- Scripted model ---

pub struct Turn {
    pub delay: Duration,
    pub text: String,
    pub calls: Vec<(String, String, Value)>,
    pub stop: StopReason,
}

pub fn turn(calls: Vec<(&str, &str, Value)>) -> Turn {
    Turn {
        delay: Duration::ZERO,
        text: String::new(),
        calls: calls
            .into_iter()
            .map(|(id, name, args)| (id.to_string(), name.to_string(), args))
            .collect(),
        stop: StopReason::ToolUse,
    }
}

/// A turn with one call.
pub fn one(id: &str, name: &str, args: Value) -> Turn {
    turn(vec![(id, name, args)])
}

pub fn text_turn(text: &str) -> Turn {
    Turn {
        text: text.into(),
        stop: StopReason::EndTurn,
        ..turn(vec![])
    }
}

pub fn send(id: &str, to: &str, body: &str) -> Turn {
    one(id, "send_email", json!({"to": to, "body": body}))
}

pub fn fin(outcome: &str) -> Turn {
    turn(vec![finish(outcome)])
}

impl Turn {
    pub fn delayed(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }

    pub fn saying(mut self, text: &str) -> Self {
        self.text = text.into();
        self
    }
}

pub fn finish(outcome: &str) -> (&'static str, &'static str, Value) {
    (
        "fin",
        "finish",
        json!({"summary": "done", "outcome": outcome}),
    )
}

/// Replays turns in order and records every request it was sent.
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

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Tool results (call id, text, is_error) in the model's latest request.
    pub fn last_results(&self) -> Vec<(String, String, bool)> {
        let reqs = self.requests();
        self.last_results_of(reqs.last().expect("no requests"))
    }

    /// Tool results in the last message of `req`.
    pub fn last_results_of(&self, req: &ChatRequest) -> Vec<(String, String, bool)> {
        req.messages
            .last()
            .unwrap()
            .content
            .iter()
            .filter_map(|p| match p {
                Part::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => Some((call_id.clone(), content.clone(), *is_error)),
                _ => None,
            })
            .collect()
    }

    /// The text of the first tool result in request `n`.
    pub fn result_text(&self, n: usize) -> String {
        self.last_results_of(&self.requests()[n])[0].1.clone()
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
        if !t.delay.is_zero() {
            tokio::time::sleep(t.delay).await;
        }
        let mut events = Vec::new();
        if !t.text.is_empty() {
            events.push(Ok(ChatEvent::TextDelta(t.text.clone())));
        }
        for (id, name, arguments) in t.calls {
            events.push(Ok(ChatEvent::ToolCall {
                id,
                name,
                arguments,
            }));
        }
        events.push(Ok(ChatEvent::Done {
            stop_reason: t.stop,
            input_tokens: Some(100),
            output_tokens: Some(20),
        }));
        Ok(Box::pin(stream::iter(events)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("embed".into()))
    }
}

// --- Policy gate ---

#[derive(Default)]
pub struct FakeGate {
    rules: Mutex<HashMap<String, Decision>>,
    pub halted: AtomicBool,
    pub seen: Mutex<Vec<ActionDescriptor>>,
    /// Approvals `record_approved` still accepts (`u32::MAX` by default).
    pub approvals_left: AtomicU32,
    /// Actions passed to `record_approved`, with their risk.
    pub approved: Mutex<Vec<(ActionDescriptor, RiskClass)>>,
}

impl FakeGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            approvals_left: AtomicU32::new(u32::MAX),
            ..Self::default()
        })
    }

    /// Decision for actions whose verb is `verb`.
    pub fn rule(self: &Arc<Self>, verb: &str, d: Decision) -> Arc<Self> {
        self.rules.lock().unwrap().insert(verb.into(), d);
        self.clone()
    }

    pub fn halt(&self) {
        self.halted.store(true, Ordering::SeqCst);
    }

    pub fn seen(&self) -> Vec<ActionDescriptor> {
        self.seen.lock().unwrap().clone()
    }
}

impl PolicyGate for FakeGate {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision {
        self.seen.lock().unwrap().push(action.clone());
        self.rules
            .lock()
            .unwrap()
            .get(&action.verb)
            .cloned()
            .unwrap_or(Decision::Allow)
    }

    fn halted(&self) -> bool {
        self.halted.load(Ordering::SeqCst)
    }

    fn record_approved(&self, action: &ActionDescriptor, risk: RiskClass) -> bool {
        self.approved.lock().unwrap().push((action.clone(), risk));
        // `fetch_update` is `try_update` from Rust 1.99, newer than the MSRV.
        #[allow(deprecated)]
        self.approvals_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

// --- Tools ---

type CallFn = Box<dyn Fn(&Value) -> ToolResult + Send + Sync>;
type ObserveFn = Box<dyn Fn() -> Value + Send + Sync>;
type VerifyFn =
    Box<dyn Fn(&Value, &Snapshot, &Snapshot, &ToolResult) -> Verification + Send + Sync>;
type DescribeFn = Box<dyn Fn(&Value) -> ActionTarget + Send + Sync>;

/// A tool built from closures.
pub struct Fake {
    name: &'static str,
    risk: RiskClass,
    schema: Value,
    call: CallFn,
    observe: Option<ObserveFn>,
    verify: Option<VerifyFn>,
    describe: Option<DescribeFn>,
}

impl Fake {
    pub fn new(
        name: &'static str,
        risk: RiskClass,
        call: impl Fn(&Value) -> ToolResult + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            risk,
            schema: json!({"type": "object"}),
            call: Box::new(call),
            observe: None,
            verify: None,
            describe: None,
        }
    }

    /// An object schema requiring these properties, each of type `ty`.
    pub fn args(mut self, ty: &str, required: &[&str]) -> Self {
        let props: serde_json::Map<String, Value> = required
            .iter()
            .map(|k| (k.to_string(), json!({"type": ty})))
            .collect();
        self.schema = json!({"type": "object", "properties": props, "required": required});
        self
    }

    pub fn observe(mut self, f: impl Fn() -> Value + Send + Sync + 'static) -> Self {
        self.observe = Some(Box::new(f));
        self
    }

    pub fn verify(
        mut self,
        f: impl Fn(&Value, &Snapshot, &Snapshot, &ToolResult) -> Verification + Send + Sync + 'static,
    ) -> Self {
        self.verify = Some(Box::new(f));
        self
    }

    pub fn describe(mut self, f: impl Fn(&Value) -> ActionTarget + Send + Sync + 'static) -> Self {
        self.describe = Some(Box::new(f));
        self
    }

    pub fn arc(self) -> Arc<dyn Tool> {
        Arc::new(self)
    }
}

#[async_trait]
impl Tool for Fake {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "A test tool."
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    fn declared_risk(&self) -> RiskClass {
        self.risk
    }
    fn describe(&self, args: &Value) -> ActionTarget {
        match &self.describe {
            Some(f) => f(args),
            None => ActionTarget {
                target_app: "nugget".into(),
                verb: self.name.into(),
                ..ActionTarget::default()
            },
        }
    }
    async fn observe(&self, _args: &Value, _ctx: &ToolCtx) -> Snapshot {
        self.observe.as_ref().map(|f| f())
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        (self.call)(&args)
    }
    fn verify(&self, a: &Value, b: &Snapshot, after: &Snapshot, r: &ToolResult) -> Verification {
        self.verify
            .as_ref()
            .map_or(Verification::Unverifiable, |f| f(a, b, after, r))
    }
}

/// A counter the agent can set or increment, sent mail and typed text. `broken` makes
/// `set_value` silently do nothing (`broken_once`: only the next time).
#[derive(Default)]
pub struct World {
    pub value: Mutex<i64>,
    pub broken: AtomicBool,
    pub broken_once: AtomicBool,
    pub set_calls: AtomicU32,
    pub increments: AtomicU32,
    pub sent: Mutex<Vec<(String, String)>>,
    pub typed: Mutex<Vec<String>>,
}

impl World {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn value(&self) -> i64 {
        *self.value.lock().unwrap()
    }

    pub fn sent(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    pub fn n(&self, counter: &AtomicU32) -> u32 {
        counter.load(Ordering::SeqCst)
    }
}

fn counter(w: &Arc<World>) -> impl Fn() -> Value + use<> {
    let w = w.clone();
    move || json!(w.value())
}

/// `set_value`, `increment`, `send_email`, `type_text` (unverifiable), `delete_all` (panics if
/// run) and `login` (takes a password), plus the built-ins.
pub fn registry(w: &Arc<World>) -> ToolRegistry {
    let (w1, w2, w3, w4, w5) = (w.clone(), w.clone(), w.clone(), w.clone(), w.clone());
    let set = Fake::new("set_value", RiskClass::Edit, move |a| {
        w1.set_calls.fetch_add(1, Ordering::SeqCst);
        let once = w1.broken_once.swap(false, Ordering::SeqCst);
        if !w1.broken.load(Ordering::SeqCst) && !once {
            *w1.value.lock().unwrap() = a["value"].as_i64().unwrap();
        }
        ToolResult::ok("set")
    })
    .args("integer", &["value"])
    .observe(counter(w))
    .verify(|a, _, after, _| match after.as_ref() == Some(&a["value"]) {
        true => Verification::Passed(format!("counter reads {}", a["value"])),
        false => Verification::Failed(format!(
            "counter reads {}, expected {}",
            after.clone().unwrap_or_default(),
            a["value"]
        )),
    });
    let inc = Fake::new("increment", RiskClass::Edit, move |_| {
        w2.increments.fetch_add(1, Ordering::SeqCst);
        *w2.value.lock().unwrap() += 1;
        ToolResult::ok("incremented")
    })
    .observe(counter(w))
    .verify(|_, b, a, _| {
        let (b, a) = (b.clone().unwrap_or_default(), a.clone().unwrap_or_default());
        match a.as_i64() == b.as_i64().map(|b| b + 1) {
            true => Verification::Passed(format!("{b} → {a}")),
            false => Verification::Failed(format!("{b} → {a}")),
        }
    });
    let mail = Fake::new("send_email", RiskClass::Send, move |a| {
        let (to, body) = (a["to"].as_str().unwrap(), a["body"].as_str().unwrap());
        w3.sent.lock().unwrap().push((to.into(), body.into()));
        ToolResult::ok("sent")
    })
    .args("string", &["to", "body"])
    .observe(move || json!(w4.sent()))
    .verify(|_, b, a, _| {
        match a.as_ref().and_then(Value::as_u64)
            == b.as_ref().and_then(Value::as_u64).map(|b| b + 1)
        {
            true => Verification::Passed("message is in Sent".into()),
            false => Verification::Failed("message not found in Sent".into()),
        }
    })
    .describe(|a| ActionTarget {
        target_app: "mail".into(),
        verb: "send_email".into(),
        target_label: a["to"].as_str().map(String::from),
        text_preview: a["body"].as_str().map(String::from),
    });
    let typing = Fake::new("type_text", RiskClass::Edit, move |a| {
        w5.typed
            .lock()
            .unwrap()
            .push(a["text"].as_str().unwrap().into());
        ToolResult::ok("typed")
    })
    .args("string", &["text"]);
    let delete = Fake::new("delete_all", RiskClass::Destroy, |_| {
        panic!("delete_all must never run in tests")
    });
    let login = Fake::new("login", RiskClass::Observe, |_| ToolResult::ok("signed in"))
        .args("string", &["username", "password"]);
    [set, inc, mail, typing, delete, login]
        .into_iter()
        .fold(ToolRegistry::new(), |r, t| r.with(t.arc()))
}

/// Returns fixed untrusted text under `name`.
pub fn page(name: &'static str, text: &str) -> Arc<dyn Tool> {
    let text = text.to_string();
    Fake::new(name, RiskClass::Observe, move |_| {
        ToolResult::untrusted(text.clone())
    })
    .arc()
}

/// Shows `rows` as untrusted text whose lines are the result's on-screen targets.
pub fn screen(name: &'static str, rows: &[&str]) -> Arc<dyn Tool> {
    let rows: Vec<String> = rows.iter().map(|s| s.to_string()).collect();
    Fake::new(name, RiskClass::Observe, move |_| {
        ToolResult::untrusted(rows.join("\n")).with_targets(rows.clone())
    })
    .arc()
}

/// Engages the kill switch while it runs (the user hits the hotkey mid-action).
pub fn kill_switch(gate: &Arc<FakeGate>) -> Arc<dyn Tool> {
    let gate = gate.clone();
    Fake::new("look", RiskClass::Observe, move |_| {
        gate.halt();
        ToolResult::ok("a window")
    })
    .arc()
}

/// An edit that takes `.0` to run and sets `.1` when it completes.
pub struct Slow(pub Duration, pub Arc<AtomicBool>);

#[async_trait]
impl Tool for Slow {
    fn name(&self) -> &str {
        "slow_edit"
    }
    fn description(&self) -> &str {
        "A slow edit."
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Edit
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        tokio::time::sleep(self.0).await;
        self.1.store(true, Ordering::SeqCst);
        ToolResult::ok("done")
    }
}

// --- Harness ---

pub fn agent(model: &Arc<Scripted>, tools: ToolRegistry, gate: &Arc<FakeGate>) -> Agent {
    Agent::new(Model::Provider(model.clone()), tools, gate.clone())
}

/// Run `turns` against `tools` with an allow-all gate and [`cfg`].
pub async fn exec(tools: ToolRegistry, turns: Vec<Turn>, goal: &str) -> (AgentRun, Arc<Scripted>) {
    exec_cfg(tools, turns, goal, cfg()).await
}

pub async fn exec_cfg(
    tools: ToolRegistry,
    turns: Vec<Turn>,
    goal: &str,
    cfg: AgentConfig,
) -> (AgentRun, Arc<Scripted>) {
    let model = Scripted::new(turns);
    let run = agent(&model, tools, &FakeGate::new())
        .run(Task::new(format!("t-{goal}"), goal), cfg)
        .await;
    (run, model)
}

/// Collects events; answers confirmation requests with `answer` (`None`: never answer).
pub fn events(
    agent: Agent,
    answer: Option<bool>,
) -> (Agent, tokio::task::JoinHandle<Vec<AgentEvent>>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let agent = agent.with_events(Arc::new(tx));
    let confirmations = agent.confirmations();
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let (AgentEvent::ConfirmationNeeded { request, .. }, Some(a)) = (&ev, answer) {
                assert!(confirmations.answer(&request.request_id, a));
            }
            seen.push(ev);
        }
        seen
    });
    (agent, handle)
}

/// Confirmations asked for `tool` (all tools: `None`), and whether each was forced by injection.
pub fn asked(events: &[AgentEvent], tool: Option<&str>) -> Vec<bool> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ConfirmationNeeded { request, .. }
                if tool.is_none_or(|t| request.tool == t) =>
            {
                Some(request.forced_by_injection)
            }
            _ => None,
        })
        .collect()
}

/// Questions the run suspended on, with their options.
pub fn suspensions(events: &[AgentEvent]) -> Vec<(String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Suspended {
                question, options, ..
            } => Some((question.clone(), options.clone())),
            _ => None,
        })
        .collect()
}

pub fn executed(run: &AgentRun, tool: &str) -> usize {
    run.actions
        .iter()
        .filter(|a| a.tool == tool && a.executed)
        .count()
}
