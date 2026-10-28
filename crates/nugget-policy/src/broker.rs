//! [`ConfirmationBroker`]: executors ask, the user answers, a timeout or the kill switch denies.
//!
//! ```ignore
//! match gate.evaluate_with_context(&action, &ctx) {
//!     Decision::Allow => run(action),
//!     Decision::Confirm(why) => match broker.request(&action, &ctx, why).await {
//!         ConfirmOutcome::Approved { .. } => run(action),
//!         ConfirmOutcome::Denied { .. } => skip(action),
//!     },
//!     Decision::Deny(why) => skip(action),
//! }
//! ```
//!
//! The UI subscribes with [`ConfirmationBroker::subscribe`] (or polls
//! [`ConfirmationBroker::pending`]) and answers with [`ConfirmationBroker::respond`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};

use crate::engine::{PolicyEngine, TaskApproval};
use crate::kill::HookGuard;
use crate::log::LogEvent;
use crate::redact::RedactedAction;
use crate::{ActionContext, ActionDescriptor, RiskClass};

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserDecision {
    ApproveOnce,
    /// Approve this risk class on this target for the rest of the task. For Spend and Security
    /// this is treated as [`UserDecision::ApproveOnce`].
    ApproveForTask,
    Deny,
}

/// Why a confirmation ended in deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    User,
    Timeout,
    Halted,
    /// Approved, but the task's rate limit for the class was used up meanwhile.
    RateLimited,
    /// The broker went away before an answer arrived.
    Dropped,
}

/// The executor's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ConfirmOutcome {
    Approved { for_task: bool },
    Denied { reason: DenyReason },
}

impl ConfirmOutcome {
    pub fn approved(&self) -> bool {
        matches!(self, ConfirmOutcome::Approved { .. })
    }
}

/// A question shown to the user (already redacted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequest {
    pub id: u64,
    pub action: RedactedAction,
    pub risk: RiskClass,
    pub explanation: String,
    /// Whether "approve for this task" is offered (Send and Destroy only).
    pub task_approval_offered: bool,
    pub timeout_ms: u64,
}

struct Pending {
    request: ConfirmationRequest,
    reply: oneshot::Sender<Option<UserDecision>>,
}

struct Inner {
    engine: Arc<PolicyEngine>,
    timeout: Duration,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    events: broadcast::Sender<ConfirmationRequest>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Routes confirmations between executors and the user. Cloneable.
#[derive(Clone)]
pub struct ConfirmationBroker {
    inner: Arc<Inner>,
    _halt_hook: Arc<HookGuard>,
}

impl std::fmt::Debug for ConfirmationBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmationBroker")
            .field("timeout", &self.inner.timeout)
            .field("pending", &self.inner.lock().len())
            .finish()
    }
}

impl ConfirmationBroker {
    /// A broker using the engine's configured timeout (`confirm_timeout_secs`).
    pub fn new(engine: Arc<PolicyEngine>) -> Self {
        let timeout = Duration::from_secs(engine.config().confirm_timeout_secs);
        Self::with_timeout(engine, timeout)
    }

    pub fn with_timeout(engine: Arc<PolicyEngine>, timeout: Duration) -> Self {
        let (events, _) = broadcast::channel(64);
        let inner = Arc::new(Inner {
            engine: Arc::clone(&engine),
            timeout,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            events,
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let hook = engine.kill_switch().on_change(move |halted| {
            if halted && let Some(inner) = weak.upgrade() {
                let drained: Vec<Pending> = inner.lock().drain().map(|(_, p)| p).collect();
                for p in drained {
                    let _ = p.reply.send(None);
                }
            }
        });
        Self {
            inner,
            _halt_hook: Arc::new(hook),
        }
    }

    /// New requests, for the UI.
    pub fn subscribe(&self) -> broadcast::Receiver<ConfirmationRequest> {
        self.inner.events.subscribe()
    }

    /// Requests waiting for an answer, oldest first.
    pub fn pending(&self) -> Vec<ConfirmationRequest> {
        let mut v: Vec<ConfirmationRequest> = self
            .inner
            .lock()
            .values()
            .map(|p| p.request.clone())
            .collect();
        v.sort_by_key(|r| r.id);
        v
    }

    /// Answer a request. Returns `false` if it is no longer pending (answered, timed out,
    /// halted).
    pub fn respond(&self, id: u64, decision: UserDecision) -> bool {
        match self.inner.lock().remove(&id) {
            Some(p) => p.reply.send(Some(decision)).is_ok(),
            None => false,
        }
    }

    /// Ask the user about `action` and wait for the answer, at most the broker's timeout.
    /// Timeout, the kill switch and a dropped request all count as deny. Every request and
    /// outcome is logged; an approval counts against the task's rate limit.
    pub async fn request(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        explanation: impl Into<String>,
    ) -> ConfirmOutcome {
        let engine = &self.inner.engine;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let risk = engine.classify(action, ctx).risk;
        let request = ConfirmationRequest {
            id,
            action: RedactedAction::new(action, ctx),
            risk,
            explanation: explanation.into(),
            task_approval_offered: matches!(risk, RiskClass::Send | RiskClass::Destroy),
            timeout_ms: self.inner.timeout.as_millis() as u64,
        };
        engine.record_event(LogEvent::ConfirmationRequested {
            id,
            action: request.action.clone(),
            risk,
            explanation: request.explanation.clone(),
        });

        let outcome = if engine.halted() {
            ConfirmOutcome::Denied {
                reason: DenyReason::Halted,
            }
        } else {
            let (tx, rx) = oneshot::channel();
            self.inner.lock().insert(
                id,
                Pending {
                    request: request.clone(),
                    reply: tx,
                },
            );
            // Close the race with a halt that happened between the check and the insert.
            if engine.halted()
                && let Some(p) = self.inner.lock().remove(&id)
            {
                let _ = p.reply.send(None);
            }
            let _ = self.inner.events.send(request);
            let answer = tokio::time::timeout(self.inner.timeout, rx).await;
            self.inner.lock().remove(&id);
            match answer {
                Err(_) => ConfirmOutcome::Denied {
                    reason: DenyReason::Timeout,
                },
                Ok(Err(_)) => ConfirmOutcome::Denied {
                    reason: DenyReason::Dropped,
                },
                Ok(Ok(None)) => ConfirmOutcome::Denied {
                    reason: DenyReason::Halted,
                },
                Ok(Ok(Some(UserDecision::Deny))) => ConfirmOutcome::Denied {
                    reason: DenyReason::User,
                },
                Ok(Ok(Some(decision))) => self.approve(action, ctx, risk, decision),
            }
        };

        engine.record_event(LogEvent::ConfirmationResolved {
            id,
            task_id: action.task_id.clone(),
            outcome,
        });
        outcome
    }

    fn approve(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        risk: RiskClass,
        decision: UserDecision,
    ) -> ConfirmOutcome {
        let engine = &self.inner.engine;
        if engine.halted() {
            return ConfirmOutcome::Denied {
                reason: DenyReason::Halted,
            };
        }
        if !engine.consume(&action.task_id, risk) {
            return ConfirmOutcome::Denied {
                reason: DenyReason::RateLimited,
            };
        }
        let for_task = decision == UserDecision::ApproveForTask
            && matches!(risk, RiskClass::Send | RiskClass::Destroy);
        if for_task {
            let target = crate::rules::target_candidates(action, ctx)
                .into_iter()
                .next()
                .unwrap_or_else(|| action.target_app.to_lowercase());
            let _ = engine.approve_for_task(
                &action.task_id,
                TaskApproval {
                    risk,
                    target: Some(target),
                },
            );
        }
        ConfirmOutcome::Approved { for_task }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::ActionLog;
    use crate::{Decision, KillSwitch, PolicyGate};

    fn setup(timeout_ms: u64) -> (Arc<PolicyEngine>, ConfirmationBroker, Arc<ActionLog>) {
        let log = Arc::new(ActionLog::in_memory());
        let engine = Arc::new(
            PolicyEngine::builder()
                .kill_switch(KillSwitch::new())
                .log(Arc::clone(&log))
                .build(),
        );
        let broker = ConfirmationBroker::with_timeout(
            Arc::clone(&engine),
            Duration::from_millis(timeout_ms),
        );
        (engine, broker, log)
    }

    fn send() -> ActionDescriptor {
        ActionDescriptor {
            target_app: "com.apple.mail".into(),
            verb: "click".into(),
            target_label: Some("Send".into()),
            text_preview: Some("Hi".into()),
            declared_risk: RiskClass::Send,
            task_id: "t1".into(),
        }
    }

    async fn answer_next(broker: &ConfirmationBroker, decision: UserDecision) {
        let mut rx = broker.subscribe();
        let req = rx.recv().await.unwrap();
        assert!(broker.respond(req.id, decision));
    }

    #[tokio::test]
    async fn approve_once() {
        let (engine, broker, _) = setup(5_000);
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::ApproveOnce).await });
        tokio::task::yield_now().await;
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        ui.await.unwrap();
        assert_eq!(out, ConfirmOutcome::Approved { for_task: false });
        assert_eq!(engine.used("t1", RiskClass::Send), 1);
        assert!(matches!(engine.evaluate(&send()), Decision::Confirm(_)));
        assert!(broker.pending().is_empty());
    }

    #[tokio::test]
    async fn approve_for_task_allows_the_rest() {
        let (engine, broker, _) = setup(5_000);
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::ApproveForTask).await });
        tokio::task::yield_now().await;
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        ui.await.unwrap();
        assert_eq!(out, ConfirmOutcome::Approved { for_task: true });
        assert_eq!(engine.evaluate(&send()), Decision::Allow);
    }

    #[tokio::test]
    async fn approve_for_task_on_spend_is_once() {
        let (engine, broker, _) = setup(5_000);
        let mut pay = send();
        pay.target_label = Some("Pay now".into());
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::ApproveForTask).await });
        tokio::task::yield_now().await;
        let out = broker
            .request(&pay, &ActionContext::default(), "pay?")
            .await;
        ui.await.unwrap();
        assert_eq!(out, ConfirmOutcome::Approved { for_task: false });
        assert!(matches!(engine.evaluate(&pay), Decision::Confirm(_)));
    }

    #[tokio::test]
    async fn user_deny() {
        let (_, broker, _) = setup(5_000);
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::Deny).await });
        tokio::task::yield_now().await;
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        ui.await.unwrap();
        assert_eq!(
            out,
            ConfirmOutcome::Denied {
                reason: DenyReason::User
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_counts_as_deny() {
        let (engine, broker, log) = setup(30_000);
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        assert_eq!(
            out,
            ConfirmOutcome::Denied {
                reason: DenyReason::Timeout
            }
        );
        assert!(broker.pending().is_empty());
        assert_eq!(engine.used("t1", RiskClass::Send), 0);
        // A late answer is refused.
        assert!(!broker.respond(1, UserDecision::ApproveOnce));
        let resolved = log
            .entries()
            .into_iter()
            .filter(|e| matches!(e.event, LogEvent::ConfirmationResolved { .. }))
            .count();
        assert_eq!(resolved, 1);
    }

    #[tokio::test]
    async fn halt_denies_pending() {
        let (engine, broker, _) = setup(60_000);
        let mut rx = broker.subscribe();
        let b2 = broker.clone();
        let waiter = tokio::spawn(async move {
            b2.request(&send(), &ActionContext::default(), "send?")
                .await
        });
        rx.recv().await.unwrap();
        assert_eq!(broker.pending().len(), 1);
        engine.halt();
        let out = waiter.await.unwrap();
        assert_eq!(
            out,
            ConfirmOutcome::Denied {
                reason: DenyReason::Halted
            }
        );
        // While halted, new requests are denied at once.
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        assert_eq!(
            out,
            ConfirmOutcome::Denied {
                reason: DenyReason::Halted
            }
        );
    }

    #[tokio::test]
    async fn rate_limit_applies_to_approvals() {
        let (engine, broker, _) = setup(5_000);
        engine
            .update_config("limit", |c| {
                c.rate_limits.insert(RiskClass::Send, 1);
            })
            .unwrap();
        assert!(engine.consume("t1", RiskClass::Send));
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::ApproveOnce).await });
        tokio::task::yield_now().await;
        let out = broker
            .request(&send(), &ActionContext::default(), "send?")
            .await;
        ui.await.unwrap();
        assert_eq!(
            out,
            ConfirmOutcome::Denied {
                reason: DenyReason::RateLimited
            }
        );
    }

    #[tokio::test]
    async fn every_step_is_logged_and_redacted() {
        let (_, broker, log) = setup(5_000);
        let mut a = send();
        a.verb = "type".into();
        a.target_label = Some("Password".into());
        a.text_preview = Some("hunter2".into());
        let b2 = broker.clone();
        let ui = tokio::spawn(async move { answer_next(&b2, UserDecision::Deny).await });
        tokio::task::yield_now().await;
        broker
            .request(&a, &ActionContext::default(), "type password?")
            .await;
        ui.await.unwrap();
        let mut buf = Vec::new();
        log.export_jsonl(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("confirmation_requested"));
        assert!(text.contains("confirmation_resolved"));
        assert!(!text.contains("hunter2"));
        log.verify_chain().unwrap();
    }
}
