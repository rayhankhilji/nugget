//! Test doubles: a scripted model, a configurable policy gate, and tools with observable state.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use nugget_agent::{
    ActionTarget, Agent, AgentEvent, AgentRun, Model, Snapshot, Tool, ToolCtx, ToolRegistry,
    ToolResult, Verification,
};
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason,
};
use nugget_policy::{ActionDescriptor, Decision, PolicyGate, RiskClass};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------------------------
// Scripted model

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

pub fn text_turn(text: &str) -> Turn {
    Turn {
        text: text.into(),
        stop: StopReason::EndTurn,
        ..turn(vec![])
    }
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
                nugget_infer::Part::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => Some((call_id.clone(), content.clone(), *is_error)),
                _ => None,
            })
            .collect()
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

// ---------------------------------------------------------------------------------------------
// Policy gate

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

// ---------------------------------------------------------------------------------------------
// Tools

/// A number the agent can set; `broken` makes `set_value` silently do nothing.
#[derive(Default)]
pub struct World {
    pub value: Mutex<i64>,
    pub broken: AtomicBool,
    /// Fail only the next `set_value`.
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
}

pub struct SetValue(pub Arc<World>);

#[async_trait]
impl Tool for SetValue {
    fn name(&self) -> &str {
        "set_value"
    }
    fn description(&self) -> &str {
        "Set the counter."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"value": {"type": "integer"}}, "required": ["value"]})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Edit
    }
    async fn observe(&self, _args: &Value, _ctx: &ToolCtx) -> Snapshot {
        Some(json!(self.0.value()))
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        self.0.set_calls.fetch_add(1, Ordering::SeqCst);
        let once = self.0.broken_once.swap(false, Ordering::SeqCst);
        if !self.0.broken.load(Ordering::SeqCst) && !once {
            *self.0.value.lock().unwrap() = args["value"].as_i64().unwrap();
        }
        ToolResult::ok("set")
    }
    fn verify(
        &self,
        args: &Value,
        _b: &Snapshot,
        after: &Snapshot,
        _r: &ToolResult,
    ) -> Verification {
        if after.as_ref() == Some(&args["value"]) {
            Verification::Passed(format!("counter reads {}", args["value"]))
        } else {
            Verification::Failed(format!(
                "counter reads {}, expected {}",
                after.clone().unwrap_or_default(),
                args["value"]
            ))
        }
    }
}

pub struct Increment(pub Arc<World>);

#[async_trait]
impl Tool for Increment {
    fn name(&self) -> &str {
        "increment"
    }
    fn description(&self) -> &str {
        "Add one to the counter."
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Edit
    }
    async fn observe(&self, _args: &Value, _ctx: &ToolCtx) -> Snapshot {
        Some(json!(self.0.value()))
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        self.0.increments.fetch_add(1, Ordering::SeqCst);
        *self.0.value.lock().unwrap() += 1;
        ToolResult::ok("incremented")
    }
    fn verify(
        &self,
        _a: &Value,
        before: &Snapshot,
        after: &Snapshot,
        _r: &ToolResult,
    ) -> Verification {
        let b = before.as_ref().and_then(Value::as_i64).unwrap_or_default();
        let a = after.as_ref().and_then(Value::as_i64).unwrap_or_default();
        if a == b + 1 {
            Verification::Passed(format!("{b} → {a}"))
        } else {
            Verification::Failed(format!("{b} → {a}"))
        }
    }
}

pub struct SendEmail(pub Arc<World>);

#[async_trait]
impl Tool for SendEmail {
    fn name(&self) -> &str {
        "send_email"
    }
    fn description(&self) -> &str {
        "Send an email."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"to": {"type": "string"}, "body": {"type": "string"}}, "required": ["to", "body"]})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Send
    }
    fn describe(&self, args: &Value) -> ActionTarget {
        ActionTarget {
            target_app: "mail".into(),
            verb: "send_email".into(),
            target_label: args["to"].as_str().map(String::from),
            text_preview: args["body"].as_str().map(String::from),
        }
    }
    async fn observe(&self, _args: &Value, _ctx: &ToolCtx) -> Snapshot {
        Some(json!(self.0.sent.lock().unwrap().len()))
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        self.0.sent.lock().unwrap().push((
            args["to"].as_str().unwrap().into(),
            args["body"].as_str().unwrap().into(),
        ));
        ToolResult::ok("sent")
    }
    fn verify(
        &self,
        _a: &Value,
        before: &Snapshot,
        after: &Snapshot,
        _r: &ToolResult,
    ) -> Verification {
        if after.as_ref().and_then(Value::as_u64)
            == before.as_ref().and_then(Value::as_u64).map(|b| b + 1)
        {
            Verification::Passed("message is in Sent".into())
        } else {
            Verification::Failed("message not found in Sent".into())
        }
    }
}

pub struct DeleteAll;

#[async_trait]
impl Tool for DeleteAll {
    fn name(&self) -> &str {
        "delete_all"
    }
    fn description(&self) -> &str {
        "Delete everything."
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Destroy
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        panic!("delete_all must never run in tests");
    }
}

/// Edits without any way to check the result.
pub struct TypeText(pub Arc<World>);

#[async_trait]
impl Tool for TypeText {
    fn name(&self) -> &str {
        "type_text"
    }
    fn description(&self) -> &str {
        "Type text into the focused field."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Edit
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        self.0
            .typed
            .lock()
            .unwrap()
            .push(args["text"].as_str().unwrap().into());
        ToolResult::ok("typed")
    }
}

/// Returns fixed untrusted page text.
pub struct ReadPage(pub String);

#[async_trait]
impl Tool for ReadPage {
    fn name(&self) -> &str {
        "read_page"
    }
    fn description(&self) -> &str {
        "Read the current page."
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        ToolResult::untrusted(self.0.clone())
    }
}

/// Engages the kill switch while it runs (the user hits the hotkey mid-action).
pub struct PressKillSwitch(pub Arc<FakeGate>);

#[async_trait]
impl Tool for PressKillSwitch {
    fn name(&self) -> &str {
        "look"
    }
    fn description(&self) -> &str {
        "Look at the screen."
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        self.0.halt();
        ToolResult::ok("a window")
    }
}

/// Takes `secs` to run (for interrupting mid-action).
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

/// Takes a password; must never appear in the log.
pub struct Login;

#[async_trait]
impl Tool for Login {
    fn name(&self) -> &str {
        "login"
    }
    fn description(&self) -> &str {
        "Sign in."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"username": {"type": "string"}, "password": {"type": "string"}}, "required": ["username", "password"]})
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        ToolResult::ok("signed in")
    }
}

// ---------------------------------------------------------------------------------------------
// Harness

pub fn registry(world: &Arc<World>) -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(SetValue(world.clone())))
        .with(Arc::new(Increment(world.clone())))
        .with(Arc::new(SendEmail(world.clone())))
        .with(Arc::new(TypeText(world.clone())))
        .with(Arc::new(DeleteAll))
        .with(Arc::new(Login))
}

pub fn agent(model: &Arc<Scripted>, tools: ToolRegistry, gate: &Arc<FakeGate>) -> Agent {
    Agent::new(Model::Provider(model.clone()), tools, gate.clone())
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

pub fn executed(run: &AgentRun, tool: &str) -> usize {
    run.actions
        .iter()
        .filter(|a| a.tool == tool && a.executed)
        .count()
}
