//! `[agent]`: the agent runtime (`nugget-agent`). Serves `agent.*` and broadcasts `agent.event`.
//!
//! One [`Agent`] is built per run (its tool registry is filtered by the request), sharing the
//! policy engine as the gate, the hash-chained action log ([`ChainedLog`] over
//! [`Safety::log`]), one action ledger, and one event sink ([`Runs`]). The sink broadcasts every
//! [`AgentEvent`] as `agent.event`, keeps each task's state for `agent.list` / `agent.get`, and
//! mirrors open confirmations into [`Safety::pending`] for `policy.approvals`.
//!
//! Model: the request's `provider` when it names one; otherwise the router's agentic tier,
//! restricted to providers whose probe proved tool use and streaming. When no agentic provider
//! has been probed, the first unprobed provider configured for the agentic tier is used on its
//! own (listing a provider under `agentic` is taken as the user's word that it can call tools).
//!
//! History: `agent.list` / `agent.get` read the agent entries back from the chain, so tasks of
//! earlier daemon runs are listed too; one that never finished (or was waiting on `ask_user`,
//! whose state is not persisted) is `interrupted`.
//!
//! ```toml
//! [agent]
//! enabled = true            # false: agent.* answer "unsupported"
//! max_steps = 30            # default budgets (a run may lower or raise them)
//! deadline_secs = 600
//! max_tokens = 500000
//! confirm_timeout_secs = 120  # default: [policy] confirm_timeout_secs
//! max_concurrent = 4        # runs at once (waiting on ask_user does not count)
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nugget_agent::{
    Agent, AgentConfig, AgentEvent, AgentRun, Budgets, ChainedLog, ConfirmationRequest,
    Confirmations, EventSink, LogEntry, MemoryLedger, Model, Outcome, Record, RunState, Task,
    ToolRegistry, Verification,
};
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AgentAnswer, AgentConfirm, AgentEventNotice, AgentListRequest, AgentRunRequest, AgentStarted,
    AgentTaskDetail, AgentTaskRef, AgentTaskState, AgentTaskSummary, AgentVerificationCounts,
    ConfirmChoice, agent_method as m, agent_notification,
};
use nugget_core::{Error, Result};
use nugget_infer::Router;
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_policy::{ActionContext, PolicyGate, TaskApproval};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::policy::Safety;
use super::{Feature, FeatureContext, section, start};
use crate::hub::Hub;

/// Start the W3 features (policy, browser, agent) in that order. They share one [`Safety`].
pub(crate) async fn register(cx: &FeatureContext<'_>, list: &mut Vec<Arc<dyn Feature>>) {
    let safety = match Safety::open(cx) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "safety policy did not start; agent features left out");
            return;
        }
    };
    start(list, "policy", super::policy::start(cx, &safety));
    let browser = match super::browser::start(cx, &safety) {
        Ok(b) => {
            list.push(b.feature.clone());
            Some(b)
        }
        Err(e) => {
            tracing::error!(feature = "browser", error = %e, "feature did not start");
            None
        }
    };
    let mut tools: Vec<Arc<dyn nugget_agent::Tool>> = Vec::new();
    if let Some(b) = &browser {
        tools.extend(b.tools.iter().cloned());
    }
    tools.extend(computer_tools(cx, &safety));
    start(list, "agent", self::start_agent(cx, &safety, tools).await);
}

/// Computer-use tools (`nugget-computer`, built by W2 in parallel).
///
/// ORCHESTRATOR: when `nugget-computer` is merged, add it to `crates/nuggetd/Cargo.toml` and
/// return `nugget_computer::tools(...)` here. Until then there are none.
fn computer_tools(
    cx: &FeatureContext<'_>,
    safety: &Arc<Safety>,
) -> Vec<Arc<dyn nugget_agent::Tool>> {
    let _ = (cx, safety);
    Vec::new()
}

async fn start_agent(
    cx: &FeatureContext<'_>,
    safety: &Arc<Safety>,
    tools: Vec<Arc<dyn nugget_agent::Tool>>,
) -> Result<Arc<dyn Feature>> {
    let settings: AgentSettings = section("agent", &cx.config.agent)?;
    let state = cx.state;
    let mut by_name = BTreeMap::new();
    for t in tools {
        let name = t.name().to_string();
        if ToolRegistry::new().get(&name).is_some() {
            tracing::warn!(tool = %name, "a registered tool shadows a built-in; left out");
            continue;
        }
        if by_name.insert(name.clone(), t).is_some() {
            tracing::warn!(tool = %name, "two tools share a name; the later one is used");
        }
    }
    let runs = Arc::new(Runs {
        hub: state.hub.clone(),
        safety: safety.clone(),
        tasks: Mutex::new(HashMap::new()),
    });
    Ok(Arc::new(AgentFeature {
        settings,
        router: state.router.clone(),
        safety: safety.clone(),
        tools: by_name,
        log: Arc::new(ChainedLog::new(safety.log.clone())),
        ledger: Arc::new(MemoryLedger::new()),
        runs,
        spawner: state.tasks.clone(),
        shutdown: state.shutdown.clone(),
    }))
}

/// `[agent]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct AgentSettings {
    enabled: bool,
    max_steps: Option<u32>,
    deadline_secs: Option<u64>,
    max_tokens: Option<u64>,
    /// Overrides `[policy] confirm_timeout_secs` for agent runs.
    confirm_timeout_secs: Option<u64>,
    max_concurrent: usize,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_steps: None,
            deadline_secs: None,
            max_tokens: None,
            confirm_timeout_secs: None,
            max_concurrent: 4,
        }
    }
}

const METHODS: &[&str] = &[
    m::AGENT_RUN,
    m::AGENT_CANCEL,
    m::AGENT_CONFIRM,
    m::AGENT_ANSWER,
    m::AGENT_LIST,
    m::AGENT_GET,
];

/// Default `agent.list` length.
const LIST_DEFAULT: u32 = 50;

struct AgentFeature {
    settings: AgentSettings,
    router: Arc<Router>,
    safety: Arc<Safety>,
    /// Registered (non built-in) tools by name.
    tools: BTreeMap<String, Arc<dyn nugget_agent::Tool>>,
    log: Arc<ChainedLog>,
    ledger: Arc<MemoryLedger>,
    runs: Arc<Runs>,
    spawner: TaskTracker,
    shutdown: CancellationToken,
}

#[async_trait]
impl Feature for AgentFeature {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> Result<Value, RpcError> {
        if !self.settings.enabled {
            return Err(Error::Unsupported(
                "the agent is turned off ([agent] enabled = false)".into(),
            )
            .into());
        }
        match method {
            m::AGENT_RUN => to_result(self.run(parse_params(params)?)?),
            m::AGENT_CANCEL => {
                let r: AgentTaskRef = parse_params(params)?;
                self.cancel(&r.task_id)?;
                Ok(Value::Null)
            }
            m::AGENT_CONFIRM => {
                self.confirm(parse_params(params)?)?;
                Ok(Value::Null)
            }
            m::AGENT_ANSWER => {
                let a: AgentAnswer = parse_params(params)?;
                self.answer(&a.task_id, &a.answer)?;
                Ok(Value::Null)
            }
            m::AGENT_LIST => {
                let r: AgentListRequest = parse_params(params)?;
                to_result(self.list(r.limit.unwrap_or(LIST_DEFAULT) as usize))
            }
            m::AGENT_GET => {
                let r: AgentTaskRef = parse_params(params)?;
                to_result(self.get(&r.task_id)?)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    async fn shutdown(&self) {
        for t in self.runs.lock().values() {
            t.cancel.cancel();
        }
    }
}

impl AgentFeature {
    fn run(&self, req: AgentRunRequest) -> Result<AgentStarted> {
        let goal = req.goal.trim();
        if goal.is_empty() {
            return Err(Error::Invalid("goal is empty".into()));
        }
        let model = self.model(req.provider.as_ref())?;
        let registry = self.registry(req.tools.as_deref())?;
        let tools: Vec<String> = registry.names().map(String::from).collect();
        let cfg = self.config(&req.budgets);
        let task_id = uuid::Uuid::new_v4().simple().to_string();
        let mut task = Task::new(task_id.clone(), goal);
        task.context = req.context.filter(|c| !c.trim().is_empty());
        let gate: Arc<dyn PolicyGate> = self.safety.engine.clone();
        let agent = Arc::new(
            Agent::new(model, registry, gate)
                .with_log(self.log.clone())
                .with_events(self.runs.clone())
                .with_ledger(self.ledger.clone()),
        );
        {
            let mut tasks = self.runs.lock();
            let active = tasks.values().filter(|t| t.active()).count();
            if active >= self.settings.max_concurrent {
                return Err(Error::Unsupported(format!(
                    "{active} agent tasks are already running (max_concurrent = {})",
                    self.settings.max_concurrent
                )));
            }
            tasks.insert(
                task_id.clone(),
                TaskEntry::new(task.goal.clone(), agent.clone(), &cfg),
            );
        }
        tracing::info!(task = %task_id, tools = tools.len(), "agent task started");
        let runs = self.runs.clone();
        let engine = self.safety.engine.clone();
        self.spawner.spawn(async move {
            let id = task.id.clone();
            let run = agent.run(task, cfg).await;
            runs.after_run(&id, run, &engine);
        });
        Ok(AgentStarted { task_id, tools })
    }

    /// The request's provider, or the agentic tier.
    fn model(&self, provider: Option<&ProviderId>) -> Result<Model> {
        if let Some(id) = provider {
            let p = self
                .router
                .provider(id)
                .ok_or_else(|| Error::NotFound(format!("unknown provider {id}")))?;
            return Ok(Model::Provider(p));
        }
        let need = Capabilities {
            streaming: true,
            tools: true,
            ..Capabilities::default()
        };
        if !self.router.candidates(Tier::Agentic, &need).is_empty() {
            return Ok(Model::Router {
                router: self.router.clone(),
                tier: Tier::Agentic,
            });
        }
        let unprobed = self
            .router
            .candidates(Tier::Agentic, &Capabilities::default())
            .into_iter()
            .find(|p| self.router.capabilities(p.id()).is_none());
        match unprobed {
            Some(p) => {
                tracing::info!(provider = %p.id(), "no probed agentic provider; using the configured one as is");
                Ok(Model::Provider(p))
            }
            None => Err(Error::Unsupported(
                "no provider for the agentic tier can call tools (configure one with \
                 tiers = [\"agentic\"] and probe it)"
                    .into(),
            )),
        }
    }

    /// Built-ins plus the registered tools the request allows.
    fn registry(&self, allowed: Option<&[String]>) -> Result<ToolRegistry> {
        let mut r = ToolRegistry::new();
        match allowed {
            None => {
                for t in self.tools.values() {
                    r.register(t.clone());
                }
            }
            Some(names) => {
                for name in names {
                    if let Some(t) = self.tools.get(name) {
                        r.register(t.clone());
                    } else if r.get(name).is_none() {
                        let known: Vec<&str> = self.tools.keys().map(String::as_str).collect();
                        return Err(Error::Invalid(format!(
                            "unknown tool {name:?} (registered: {})",
                            if known.is_empty() {
                                "none".to_string()
                            } else {
                                known.join(", ")
                            }
                        )));
                    }
                }
            }
        }
        Ok(r)
    }

    fn config(&self, b: &nugget_core::ipc::AgentBudgets) -> AgentConfig {
        let s = &self.settings;
        let d = Budgets::default();
        let timeout = s
            .confirm_timeout_secs
            .unwrap_or_else(|| self.safety.engine.config().confirm_timeout_secs);
        AgentConfig {
            budgets: Budgets {
                max_steps: b.max_steps.or(s.max_steps).unwrap_or(d.max_steps),
                deadline: b
                    .deadline_secs
                    .or(s.deadline_secs)
                    .map_or(d.deadline, Duration::from_secs),
                max_tokens: b.max_tokens.or(s.max_tokens).unwrap_or(d.max_tokens),
                per_tool: d.per_tool,
            },
            confirm_timeout: Duration::from_secs(timeout),
            cancel: self.shutdown.child_token(),
            ..AgentConfig::default()
        }
    }

    fn cancel(&self, task_id: &str) -> Result<()> {
        let resume = {
            let mut tasks = self.runs.lock();
            let t = tasks
                .get_mut(task_id)
                .ok_or_else(|| unknown_task(task_id))?;
            t.cancel.cancel();
            // A run waiting on `ask_user` has no loop to stop: resume it cancelled, so it ends
            // (and is logged) like any other cancelled run.
            t.suspended.take().map(|st| {
                t.state = AgentTaskState::Running;
                t.question = None;
                (st, t.agent.clone(), t.cfg(self.shutdown.child_token()))
            })
        };
        if let Some((st, agent, cfg)) = resume {
            cfg.cancel.cancel();
            self.spawn_resume(
                task_id,
                agent,
                st,
                "(the user cancelled the task)".into(),
                cfg,
            );
        }
        Ok(())
    }

    fn confirm(&self, c: AgentConfirm) -> Result<()> {
        let (request, confirmations) = {
            let tasks = self.runs.lock();
            let t = tasks
                .get(&c.task_id)
                .ok_or_else(|| unknown_task(&c.task_id))?;
            let req =
                t.pending.get(&c.request_id).cloned().ok_or_else(|| {
                    Error::NotFound(format!("no open confirmation {}", c.request_id))
                })?;
            (req, t.confirmations.clone())
        };
        let engine = &self.safety.engine;
        let risk = engine
            .classify(&request.action, &ActionContext::default())
            .risk;
        let approve = match c.choice {
            ConfirmChoice::Deny => false,
            ConfirmChoice::Once | ConfirmChoice::Task => {
                if c.choice == ConfirmChoice::Task {
                    let approval = TaskApproval {
                        risk,
                        target: Some(request.action.target_app.clone()),
                    };
                    if let Err(e) = engine.approve_for_task(&c.task_id, approval) {
                        tracing::info!(task = %c.task_id, error = %e, "approval for the task refused; approving once");
                    }
                }
                // The loop counts the approval against the task's rate limit
                // (`PolicyGate::record_approved`) and declines the action when it is used up.
                true
            }
        };
        if !confirmations.answer(&c.request_id, approve) {
            return Err(Error::NotFound(format!(
                "confirmation {} is no longer open",
                c.request_id
            )));
        }
        Ok(())
    }

    fn answer(&self, task_id: &str, answer: &str) -> Result<()> {
        let (st, agent, cfg) = {
            let mut tasks = self.runs.lock();
            let t = tasks
                .get_mut(task_id)
                .ok_or_else(|| unknown_task(task_id))?;
            let Some(st) = t.suspended.take() else {
                return Err(Error::Invalid(format!(
                    "task {task_id} is not waiting for an answer"
                )));
            };
            t.state = AgentTaskState::Running;
            t.question = None;
            t.cancel = self.shutdown.child_token();
            let cfg = t.cfg(t.cancel.clone());
            (st, t.agent.clone(), cfg)
        };
        self.spawn_resume(task_id, agent, st, answer.to_string(), cfg);
        Ok(())
    }

    fn spawn_resume(
        &self,
        task_id: &str,
        agent: Arc<Agent>,
        st: RunState,
        answer: String,
        cfg: AgentConfig,
    ) {
        let runs = self.runs.clone();
        let engine = self.safety.engine.clone();
        let id = task_id.to_string();
        self.spawner.spawn(async move {
            let run = agent.resume(st, &answer, cfg).await;
            runs.after_run(&id, run, &engine);
        });
    }

    fn list(&self, limit: usize) -> Vec<AgentTaskSummary> {
        let entries = self.log.entries();
        let mut by_id: HashMap<String, AgentTaskSummary> = summaries_from_log(&entries)
            .into_iter()
            .map(|s| (s.task_id.clone(), s))
            .collect();
        for (id, t) in self.runs.lock().iter() {
            by_id.insert(id.clone(), t.summary(id));
        }
        let mut out: Vec<AgentTaskSummary> = by_id.into_values().collect();
        out.sort_by(|a, b| {
            b.started_ms
                .cmp(&a.started_ms)
                .then_with(|| b.task_id.cmp(&a.task_id))
        });
        out.truncate(limit);
        out
    }

    fn get(&self, task_id: &str) -> Result<AgentTaskDetail> {
        let entries = self.log.for_task(task_id);
        let explain = nugget_agent::explain(&entries);
        let live = self.runs.lock().get(task_id).map(|t| {
            (
                t.summary(task_id),
                t.pending.values().map(to_json).collect::<Vec<_>>(),
                t.question.clone(),
            )
        });
        let (summary, pending, question) = match live {
            Some(l) => l,
            None => {
                let s = summaries_from_log(&entries)
                    .into_iter()
                    .next()
                    .ok_or_else(|| unknown_task(task_id))?;
                (s, Vec::new(), None)
            }
        };
        Ok(AgentTaskDetail {
            summary,
            entries: entries.iter().map(to_json).collect(),
            explain,
            pending,
            question,
        })
    }
}

fn unknown_task(id: &str) -> Error {
    Error::NotFound(format!("unknown agent task {id}"))
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What a summary shows as detail: the outcome's own detail, else the model's summary.
fn outcome_detail(outcome: &Outcome, summary: Option<&str>) -> Option<String> {
    match outcome {
        Outcome::Unverified(d)
        | Outcome::Failed(d)
        | Outcome::NeedsInput(d)
        | Outcome::Refused(d)
        | Outcome::Error(d) => Some(d.clone()),
        Outcome::BudgetExhausted(b) => Some(b.to_string()),
        _ => summary.map(String::from),
    }
}

fn count(v: &mut AgentVerificationCounts, verification: &Verification) {
    match verification {
        Verification::Passed(_) => v.passed += 1,
        Verification::Failed(_) => v.failed += 1,
        Verification::Unverifiable => v.unverifiable += 1,
    }
}

/// Task summaries rebuilt from agent log entries (tasks of earlier daemon runs). A task whose
/// last entry is not `finished` is `interrupted`: its run (or its `ask_user` state) is gone.
fn summaries_from_log(entries: &[LogEntry]) -> Vec<AgentTaskSummary> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, AgentTaskSummary> = HashMap::new();
    for e in entries {
        let s = by_id.entry(e.task_id.clone()).or_insert_with(|| {
            order.push(e.task_id.clone());
            AgentTaskSummary {
                task_id: e.task_id.clone(),
                goal: String::new(),
                state: AgentTaskState::Interrupted,
                outcome: None,
                detail: None,
                started_ms: e.at_ms,
                finished_ms: None,
                steps: 0,
                verification: AgentVerificationCounts::default(),
            }
        });
        match &e.record {
            Record::RunStarted { goal, .. } => {
                s.goal = goal.clone();
                s.started_ms = e.at_ms;
            }
            Record::ModelTurn { step, .. } => s.steps = s.steps.max(*step),
            Record::Executed {
                verification: Some(v),
                ..
            } => count(&mut s.verification, v),
            Record::Resumed { .. } | Record::Suspended { .. } => {
                s.state = AgentTaskState::Interrupted;
                s.outcome = None;
                s.detail = None;
                s.finished_ms = None;
            }
            Record::Finished {
                outcome,
                summary,
                steps,
                ..
            } => {
                s.state = AgentTaskState::Finished;
                s.outcome = Some(outcome.label().to_string());
                s.detail = outcome_detail(outcome, summary.as_deref());
                s.finished_ms = Some(e.at_ms);
                s.steps = (*steps).max(s.steps);
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// One task this daemon started.
struct TaskEntry {
    goal: String,
    state: AgentTaskState,
    outcome: Option<String>,
    detail: Option<String>,
    started_ms: u64,
    finished_ms: Option<u64>,
    steps: u32,
    verification: AgentVerificationCounts,
    /// Open confirmations by request id.
    pending: BTreeMap<String, ConfirmationRequest>,
    question: Option<String>,
    /// Set while the run waits on `ask_user`.
    suspended: Option<RunState>,
    /// The `suspended` event, held back until the run's state is stored (so a client that sees
    /// it can answer at once).
    deferred: Option<AgentEvent>,
    agent: Arc<Agent>,
    confirmations: Confirmations,
    cancel: CancellationToken,
    /// The run's settings, for a resume.
    budgets: Budgets,
    confirm_timeout: Duration,
}

impl TaskEntry {
    fn new(goal: String, agent: Arc<Agent>, cfg: &AgentConfig) -> Self {
        Self {
            goal,
            state: AgentTaskState::Running,
            outcome: None,
            detail: None,
            started_ms: unix_ms(),
            finished_ms: None,
            steps: 0,
            verification: AgentVerificationCounts::default(),
            pending: BTreeMap::new(),
            question: None,
            suspended: None,
            deferred: None,
            confirmations: agent.confirmations(),
            agent,
            cancel: cfg.cancel.clone(),
            budgets: cfg.budgets.clone(),
            confirm_timeout: cfg.confirm_timeout,
        }
    }

    /// Counts against `max_concurrent`.
    fn active(&self) -> bool {
        matches!(
            self.state,
            AgentTaskState::Running | AgentTaskState::Confirming
        )
    }

    fn cfg(&self, cancel: CancellationToken) -> AgentConfig {
        AgentConfig {
            budgets: self.budgets.clone(),
            confirm_timeout: self.confirm_timeout,
            cancel,
            ..AgentConfig::default()
        }
    }

    fn summary(&self, id: &str) -> AgentTaskSummary {
        AgentTaskSummary {
            task_id: id.to_string(),
            goal: self.goal.clone(),
            state: self.state,
            outcome: self.outcome.clone(),
            detail: self.detail.clone(),
            started_ms: self.started_ms,
            finished_ms: self.finished_ms,
            steps: self.steps,
            verification: self.verification,
        }
    }
}

/// The event sink shared by every run: task state, open confirmations, `agent.event`.
struct Runs {
    hub: Arc<Hub>,
    safety: Arc<Safety>,
    tasks: Mutex<HashMap<String, TaskEntry>>,
}

impl Runs {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, TaskEntry>> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn pending(&self) -> MutexGuard<'_, BTreeMap<String, Value>> {
        self.safety
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn broadcast(&self, task_id: &str, event: &AgentEvent) {
        let notice = AgentEventNotice {
            task_id: task_id.to_string(),
            event: to_json(event),
        };
        self.hub
            .broadcast(agent_notification::AGENT_EVENT, to_json(&notice));
    }

    /// Update the task for `event`. Returns `false` when the event is held back.
    fn track(&self, t: &mut TaskEntry, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::StepStarted { step, .. } => t.steps = t.steps.max(*step),
            AgentEvent::ConfirmationNeeded { request, .. } => {
                t.pending
                    .insert(request.request_id.clone(), request.clone());
                self.pending()
                    .insert(request.request_id.clone(), to_json(request));
                t.state = AgentTaskState::Confirming;
            }
            AgentEvent::ConfirmationResolved { request_id, .. } => {
                t.pending.remove(request_id);
                self.pending().remove(request_id);
                if t.pending.is_empty() && t.state == AgentTaskState::Confirming {
                    t.state = AgentTaskState::Running;
                }
            }
            AgentEvent::Verification { verification, .. } => {
                count(&mut t.verification, verification);
            }
            AgentEvent::Suspended { question, .. } => {
                t.question = Some(question.clone());
                t.deferred = Some(event.clone());
                return false;
            }
            AgentEvent::Finished {
                task_id,
                outcome,
                summary,
            } => {
                self.close(t);
                t.state = AgentTaskState::Finished;
                t.outcome = Some(outcome.label().to_string());
                t.detail = outcome_detail(outcome, summary.as_deref());
                t.finished_ms = Some(unix_ms());
                self.safety.engine.end_task(task_id);
            }
            _ => {}
        }
        true
    }

    /// Drop the task's open confirmations (a run stopped while one was open).
    fn close(&self, t: &mut TaskEntry) {
        let mut pending = self.pending();
        for id in std::mem::take(&mut t.pending).into_keys() {
            pending.remove(&id);
        }
        t.question = None;
        t.suspended = None;
        t.deferred = None;
    }

    /// A run (or a resumed leg) returned.
    fn after_run(&self, task_id: &str, run: AgentRun, engine: &nugget_policy::PolicyEngine) {
        let deferred = {
            let mut tasks = self.lock();
            let Some(t) = tasks.get_mut(task_id) else {
                return;
            };
            t.steps = t.steps.max(run.steps);
            match run.suspended {
                Some(st) => {
                    t.question = st.pending.as_ref().map(|p| p.question.clone());
                    t.suspended = Some(st);
                    t.state = AgentTaskState::NeedsInput;
                    t.deferred.take()
                }
                None => {
                    if t.state != AgentTaskState::Finished {
                        // Ended without a `finished` event (a suspension whose state was lost).
                        self.close(t);
                        t.state = AgentTaskState::Finished;
                        t.outcome = Some(run.outcome.label().to_string());
                        t.detail = outcome_detail(&run.outcome, run.summary.as_deref());
                        t.finished_ms = Some(unix_ms());
                    }
                    engine.end_task(task_id);
                    None
                }
            }
        };
        tracing::info!(task = %task_id, outcome = %run.outcome, steps = run.steps, "agent task returned");
        if let Some(ev) = deferred {
            self.broadcast(task_id, &ev);
        }
    }
}

fn event_task(event: &AgentEvent) -> &str {
    match event {
        AgentEvent::RunStarted { task_id, .. }
        | AgentEvent::StepStarted { task_id, .. }
        | AgentEvent::ModelReplied { task_id, .. }
        | AgentEvent::ActionProposed { task_id, .. }
        | AgentEvent::ConfirmationNeeded { task_id, .. }
        | AgentEvent::ConfirmationResolved { task_id, .. }
        | AgentEvent::ActionDenied { task_id, .. }
        | AgentEvent::ActionDone { task_id, .. }
        | AgentEvent::Verification { task_id, .. }
        | AgentEvent::InjectionSuspected { task_id, .. }
        | AgentEvent::Suspended { task_id, .. }
        | AgentEvent::Finished { task_id, .. } => task_id,
    }
}

impl EventSink for Runs {
    fn emit(&self, event: AgentEvent) {
        let task_id = event_task(&event).to_string();
        let send = match self.lock().get_mut(&task_id) {
            Some(t) => self.track(t, &event),
            None => true,
        };
        if send {
            self.broadcast(&task_id, &event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_agent::{Claim, Usage};
    use nugget_policy::{ActionDescriptor, KillSwitch, PolicyEngine, RiskClass};

    fn safety() -> Arc<Safety> {
        Arc::new(Safety {
            engine: Arc::new(PolicyEngine::default()),
            log: Arc::new(nugget_policy::ActionLog::in_memory()),
            kill: KillSwitch::new(),
            pending: Mutex::new(BTreeMap::new()),
        })
    }

    fn entry(task: &str, seq: u64, at_ms: u64, record: Record) -> LogEntry {
        LogEntry {
            task_id: task.into(),
            seq,
            at_ms,
            record,
        }
    }

    fn started(goal: &str) -> Record {
        Record::RunStarted {
            goal: goal.into(),
            tools: vec![],
            max_steps: 5,
            deadline_ms: 1000,
            max_tokens: 100,
        }
    }

    #[test]
    fn settings_default_and_parse() {
        let s: AgentSettings = section("agent", &toml::Table::new()).unwrap();
        assert!(s.enabled);
        assert_eq!(s.max_concurrent, 4);
        let t: toml::Table =
            toml::from_str("enabled = false\nmax_steps = 7\nconfirm_timeout_secs = 3").unwrap();
        let s: AgentSettings = section("agent", &t).unwrap();
        assert!(!s.enabled);
        assert_eq!(s.max_steps, Some(7));
        assert_eq!(s.confirm_timeout_secs, Some(3));
        assert_eq!(s.max_concurrent, 4);
    }

    #[test]
    fn summaries_from_the_log_mark_unfinished_tasks_interrupted() {
        let entries = vec![
            entry("a", 0, 10, started("first")),
            entry("b", 0, 20, started("second")),
            entry(
                "a",
                1,
                11,
                Record::ModelTurn {
                    step: 1,
                    text: String::new(),
                    tool_calls: vec![],
                    stop_reason: "ToolUse".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    duration_ms: 1,
                },
            ),
            entry(
                "a",
                2,
                12,
                Record::Executed {
                    step: 1,
                    action_id: "x".into(),
                    tool: "t".into(),
                    is_error: false,
                    result: "ok".into(),
                    verification: Some(Verification::Passed("yes".into())),
                    interrupted: false,
                    duration_ms: 1,
                },
            ),
            entry(
                "a",
                3,
                13,
                Record::Finished {
                    outcome: Outcome::Succeeded,
                    claim: Some(Claim::Success),
                    summary: Some("all done".into()),
                    steps: 1,
                    usage: Usage::default(),
                    elapsed_ms: 3,
                },
            ),
            entry(
                "b",
                1,
                21,
                Record::Suspended {
                    question: "which?".into(),
                },
            ),
        ];
        let s = summaries_from_log(&entries);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].task_id, "a");
        assert_eq!(s[0].goal, "first");
        assert_eq!(s[0].state, AgentTaskState::Finished);
        assert_eq!(s[0].outcome.as_deref(), Some("succeeded"));
        assert_eq!(s[0].detail.as_deref(), Some("all done"));
        assert_eq!(s[0].finished_ms, Some(13));
        assert_eq!(s[0].steps, 1);
        assert_eq!(s[0].verification.passed, 1);
        assert_eq!(s[1].state, AgentTaskState::Interrupted);
        assert_eq!(s[1].started_ms, 20);
        assert_eq!(s[1].outcome, None);
    }

    fn request(task: &str, id: &str) -> ConfirmationRequest {
        ConfirmationRequest {
            request_id: id.into(),
            action_id: "act".into(),
            tool: "mail.send".into(),
            action: ActionDescriptor {
                target_app: "mail".into(),
                verb: "send".into(),
                target_label: None,
                text_preview: None,
                declared_risk: RiskClass::Send,
                task_id: task.into(),
            },
            reason: "sends mail".into(),
            forced_by_injection: false,
            timeout_ms: 1000,
        }
    }

    #[tokio::test]
    async fn the_sink_tracks_state_and_open_confirmations() {
        let safety = safety();
        let runs = Arc::new(Runs {
            hub: Arc::new(Hub::default()),
            safety: safety.clone(),
            tasks: Mutex::new(HashMap::new()),
        });
        let router = Arc::new(Router::new(Vec::new(), Default::default()));
        let gate: Arc<dyn PolicyGate> = safety.engine.clone();
        let agent = Arc::new(Agent::new(
            Model::Router {
                router,
                tier: Tier::Agentic,
            },
            ToolRegistry::new(),
            gate,
        ));
        let cfg = AgentConfig::default();
        runs.lock()
            .insert("t".into(), TaskEntry::new("goal".into(), agent, &cfg));
        let summary = |runs: &Runs| runs.lock()["t"].summary("t");

        runs.emit(AgentEvent::StepStarted {
            task_id: "t".into(),
            step: 2,
        });
        runs.emit(AgentEvent::ConfirmationNeeded {
            task_id: "t".into(),
            request: request("t", "r1"),
        });
        assert_eq!(summary(&runs).state, AgentTaskState::Confirming);
        assert_eq!(summary(&runs).steps, 2);
        assert!(safety.pending.lock().unwrap().contains_key("r1"));
        runs.emit(AgentEvent::ConfirmationResolved {
            task_id: "t".into(),
            request_id: "r1".into(),
            answer: nugget_agent::ConfirmAnswer::Approved,
        });
        assert_eq!(summary(&runs).state, AgentTaskState::Running);
        assert!(safety.pending.lock().unwrap().is_empty());
        runs.emit(AgentEvent::Verification {
            task_id: "t".into(),
            action_id: "a".into(),
            tool: "x".into(),
            verification: Verification::Unverifiable,
        });

        // Suspended is held back until the run returns with its state.
        runs.emit(AgentEvent::Suspended {
            task_id: "t".into(),
            question: "which one?".into(),
        });
        assert!(runs.lock()["t"].deferred.is_some());
        assert_eq!(runs.lock()["t"].question.as_deref(), Some("which one?"));

        // A run stopped while a confirmation was open drops it.
        runs.emit(AgentEvent::ConfirmationNeeded {
            task_id: "t".into(),
            request: request("t", "r2"),
        });
        runs.emit(AgentEvent::Finished {
            task_id: "t".into(),
            outcome: Outcome::Halted,
            summary: None,
        });
        let s = summary(&runs);
        assert_eq!(s.state, AgentTaskState::Finished);
        assert_eq!(s.outcome.as_deref(), Some("halted"));
        assert_eq!(s.verification.unverifiable, 1);
        assert!(s.finished_ms.is_some());
        assert!(safety.pending.lock().unwrap().is_empty());
        assert!(runs.lock()["t"].pending.is_empty());
    }
}
