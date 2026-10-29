//! [`BrowserController`]: the only path from the agent to the browser. Every snapshot and every
//! action is checked against the [`PolicyGate`] (kill switch first, then the decision), and
//! `Confirm` decisions go to a [`Confirmer`] before anything is sent.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_policy::{ActionContext, ActionDescriptor, Decision, PolicyGate, RiskClass};
use serde::{Deserialize, Serialize};

use crate::action::{BrowserAction, ValidationError, Verification, verify};
use crate::bridge::{BridgeError, BrowserBridge};
use crate::protocol::{ActResult, ErrorCode, HostRequest, SnapshotOptions};
use crate::snapshot::{PageSnapshot, SnapshotDiff};
use crate::tool::action_context;

/// Asks the user to approve an action the policy marked `Confirm`. Fails closed: anything but
/// an explicit yes is a no.
#[async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, action: &ActionDescriptor, reason: &str) -> bool;
}

/// A [`Confirmer`] that declines everything (for headless use and as a safe default).
pub struct DeclineAll;

#[async_trait]
impl Confirmer for DeclineAll {
    async fn confirm(&self, _: &ActionDescriptor, _: &str) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BrowserError {
    #[error("the kill switch is engaged; Nugget has stopped")]
    Halted,
    #[error("blocked by policy: {0}")]
    Denied(String),
    #[error("the user declined: {0}")]
    Declined(String),
    #[error("invalid action: {0}")]
    Invalid(#[from] ValidationError),
    #[error("Nugget has no access to this site; the user can allow it from the toolbar button")]
    PermissionRequired,
    #[error("the page changed since the last snapshot; take a new snapshot")]
    StaleSnapshot,
    #[error(transparent)]
    Bridge(BridgeError),
}

impl From<BridgeError> for BrowserError {
    fn from(e: BridgeError) -> Self {
        match e {
            BridgeError::Extension {
                code: ErrorCode::Halted,
                ..
            } => Self::Halted,
            BridgeError::Extension {
                code: ErrorCode::PermissionRequired,
                ..
            } => Self::PermissionRequired,
            BridgeError::Extension {
                code: ErrorCode::StaleSnapshot | ErrorCode::UnknownElement,
                ..
            } => Self::StaleSnapshot,
            other => Self::Bridge(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOutcome {
    /// The descriptor the policy gate approved.
    pub descriptor: ActionDescriptor,
    pub snapshot: PageSnapshot,
    pub diff: SnapshotDiff,
    pub verification: Verification,
}

#[derive(Default)]
struct State {
    latest: Option<PageSnapshot>,
    overlay_on: bool,
}

pub struct BrowserController {
    bridge: Arc<dyn BrowserBridge>,
    gate: Arc<dyn PolicyGate>,
    confirmer: Arc<dyn Confirmer>,
    state: Mutex<State>,
}

impl BrowserController {
    pub fn new(
        bridge: Arc<dyn BrowserBridge>,
        gate: Arc<dyn PolicyGate>,
        confirmer: Arc<dyn Confirmer>,
    ) -> Self {
        Self {
            bridge,
            gate,
            confirmer,
            state: Mutex::new(State::default()),
        }
    }

    pub fn latest_snapshot(&self) -> Option<PageSnapshot> {
        self.lock().latest.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Kill switch check. When engaged, also tell the extension so the page shows it.
    async fn check_halt(&self) -> Result<(), BrowserError> {
        if self.gate.halted() {
            let _ = self.bridge.request(HostRequest::Halt).await;
            return Err(BrowserError::Halted);
        }
        Ok(())
    }

    /// Ask the gate (and the user, if needed) about `descriptor`, seen in `context` (built from
    /// the snapshot the action is checked against, the same way the agent tools build it for
    /// the loop's gate).
    async fn authorize(
        &self,
        descriptor: &ActionDescriptor,
        context: &ActionContext,
    ) -> Result<(), BrowserError> {
        match self.gate.evaluate_with_context(descriptor, context) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => Err(BrowserError::Denied(reason)),
            Decision::Confirm(reason) => {
                if self.confirmer.confirm(descriptor, &reason).await {
                    // Approval can take a while; the user may have hit Stop meanwhile.
                    self.check_halt().await?;
                    // An approved action still counts against the task's rate limit.
                    if !self.gate.record_approved_with_context(
                        descriptor,
                        descriptor.declared_risk,
                        context,
                    ) {
                        return Err(BrowserError::Denied(format!(
                            "rate limit reached for {} actions in this task",
                            descriptor.declared_risk
                        )));
                    }
                    Ok(())
                } else {
                    Err(BrowserError::Declined(reason))
                }
            }
        }
    }

    /// Read the current page. The snapshot is taken first (the URL is not known before) and
    /// discarded unless the gate allows reading that site, so a denied site's content never
    /// reaches the agent.
    pub async fn snapshot(
        &self,
        task_id: &str,
        options: SnapshotOptions,
    ) -> Result<PageSnapshot, BrowserError> {
        self.check_halt().await?;
        let value = self
            .bridge
            .request(HostRequest::Snapshot {
                tab_id: None,
                options: Some(options),
            })
            .await?;
        let snap: PageSnapshot = serde_json::from_value(value)
            .map_err(|e| BrowserError::Bridge(BridgeError::Malformed(e.to_string())))?;
        let descriptor = ActionDescriptor {
            target_app: snap.host().unwrap_or_default(),
            verb: "snapshot".into(),
            target_label: Some(snap.title.clone()),
            text_preview: None,
            declared_risk: RiskClass::Observe,
            task_id: task_id.to_owned(),
        };
        // Reading judges the site (`target_app`) only. No page context: the classifier would
        // read a checkout page's URL and title as a purchase being committed, and a read
        // commits nothing.
        self.authorize(&descriptor, &ActionContext::default())
            .await?;
        self.lock().latest = Some(snap.clone());
        Ok(snap)
    }

    /// Validate, authorize, perform and verify one action.
    pub async fn act(
        &self,
        task_id: &str,
        action: BrowserAction,
    ) -> Result<ActionOutcome, BrowserError> {
        self.check_halt().await?;
        let latest = self.latest_snapshot();
        action.validate(latest.as_ref())?;
        let descriptor = action.describe(latest.as_ref(), task_id);
        if descriptor.target_app.is_empty() {
            return Err(
                ValidationError::Invalid("cannot tell which site this acts on".into()).into(),
            );
        }
        // `target_app` is the current page's host, or the destination for `navigate`.
        let context = action_context(Some(&action), latest.as_ref());
        self.authorize(&descriptor, &context).await?;

        self.show_overlay().await;
        let value = self
            .bridge
            .request(HostRequest::Act {
                tab_id: None,
                snapshot_id: latest
                    .as_ref()
                    .map(|s| s.snapshot_id.clone())
                    .unwrap_or_default(),
                action: action.clone(),
            })
            .await;
        let value = match value {
            Ok(v) => v,
            Err(e) => {
                if matches!(
                    e,
                    BridgeError::Extension {
                        code: ErrorCode::StaleSnapshot | ErrorCode::UnknownElement,
                        ..
                    }
                ) {
                    self.lock().latest = None;
                }
                return Err(e.into());
            }
        };
        let result: ActResult = serde_json::from_value(value)
            .map_err(|e| BrowserError::Bridge(BridgeError::Malformed(e.to_string())))?;
        let verification = verify(&action, latest.as_ref(), &result.snapshot, &result.diff);
        self.lock().latest = Some(result.snapshot.clone());
        Ok(ActionOutcome {
            descriptor,
            snapshot: result.snapshot,
            diff: result.diff,
            verification,
        })
    }

    async fn show_overlay(&self) {
        if self.lock().overlay_on {
            return;
        }
        let shown = self
            .bridge
            .request(HostRequest::Overlay {
                tab_id: None,
                active: true,
                label: None,
            })
            .await
            .is_ok();
        self.lock().overlay_on = shown;
    }

    /// End of a task: hide the overlay. Best effort.
    pub async fn finish(&self) {
        let was_on = std::mem::take(&mut self.lock().overlay_on);
        if was_on {
            let _ = self
                .bridge
                .request(HostRequest::Overlay {
                    tab_id: None,
                    active: false,
                    label: None,
                })
                .await;
        }
    }
}
