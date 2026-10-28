//! The event stream the UI follows, and the channel confirmations are answered through.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nugget_policy::ActionDescriptor;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::injection::InjectionFinding;
use crate::tool::Verification;
use crate::types::{ConfirmAnswer, Outcome};

/// A request for the user to approve one action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmationRequest {
    /// Answer with [`Confirmations::answer`] using this id.
    pub request_id: String,
    pub action_id: String,
    pub tool: String,
    pub action: ActionDescriptor,
    /// What will happen and why the user is being asked.
    pub reason: String,
    /// Raised by the injection guard rather than by the policy.
    pub forced_by_injection: bool,
    /// No answer within this long counts as a denial.
    pub timeout_ms: u64,
}

/// Progress of a run, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    RunStarted {
        task_id: String,
        goal: String,
    },
    StepStarted {
        task_id: String,
        step: u32,
    },
    ModelReplied {
        task_id: String,
        step: u32,
        text: String,
        tool_calls: Vec<String>,
    },
    ActionProposed {
        task_id: String,
        step: u32,
        action_id: String,
        tool: String,
        action: ActionDescriptor,
    },
    ConfirmationNeeded {
        task_id: String,
        request: ConfirmationRequest,
    },
    ConfirmationResolved {
        task_id: String,
        request_id: String,
        answer: ConfirmAnswer,
    },
    ActionDenied {
        task_id: String,
        action_id: String,
        tool: String,
        reason: String,
    },
    ActionDone {
        task_id: String,
        action_id: String,
        tool: String,
        is_error: bool,
        summary: String,
        replayed: bool,
    },
    Verification {
        task_id: String,
        action_id: String,
        tool: String,
        verification: Verification,
    },
    InjectionSuspected {
        task_id: String,
        action_id: String,
        tool: String,
        finding: InjectionFinding,
    },
    Suspended {
        task_id: String,
        question: String,
    },
    Finished {
        task_id: String,
        outcome: Outcome,
        summary: Option<String>,
    },
}

/// Where events go. Must not block.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: AgentEvent);
}

/// Drops every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _event: AgentEvent) {}
}

impl EventSink for mpsc::UnboundedSender<AgentEvent> {
    fn emit(&self, event: AgentEvent) {
        let _ = self.send(event);
    }
}

impl EventSink for broadcast::Sender<AgentEvent> {
    fn emit(&self, event: AgentEvent) {
        let _ = self.send(event);
    }
}

/// Pending confirmation requests, answered by the UI (through IPC) by request id. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Confirmations {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl Confirmations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer a pending request. Returns `false` if it is unknown or already resolved.
    pub fn answer(&self, request_id: &str, approve: bool) -> bool {
        match self.lock().remove(request_id) {
            Some(tx) => tx.send(approve).is_ok(),
            None => false,
        }
    }

    /// Ids of requests still waiting for the user.
    pub fn pending(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.lock().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub(crate) fn register(&self, request_id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(request_id.to_string(), tx);
        rx
    }

    pub(crate) fn withdraw(&self, request_id: &str) {
        self.lock().remove(request_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<bool>>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
}
