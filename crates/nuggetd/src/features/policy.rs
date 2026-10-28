//! `[policy]`: the safety policy for actions (`nugget-policy`), shared by the agent and the
//! browser features. Serves `policy.*`.
//!
//! SKELETON (W3 lead): the shapes below are the seam between `policy.rs`, `browser.rs` and
//! `agent.rs`. Fill in the bodies; keep the signatures (or change all three files together).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use nugget_core::Result;
use nugget_policy::{ActionLog, KillSwitch, PolicyEngine};
use serde_json::Value;

use super::{Feature, FeatureContext};

/// What the policy, browser and agent features share. One per daemon (tests run several
/// daemons in one process, so nothing here is global).
pub(crate) struct Safety {
    pub engine: Arc<PolicyEngine>,
    /// The hash-chained action log under the data dir, shared by the engine and the agent.
    pub log: Arc<ActionLog>,
    /// This daemon's kill switch (the engine's).
    pub kill: KillSwitch,
    /// Open confirmation requests (`nugget_agent::ConfirmationRequest` as JSON) by request id,
    /// written by the agent feature, read by `policy.approvals`.
    pub pending: Mutex<BTreeMap<String, Value>>,
}

impl Safety {
    /// Load the policy config and open the action log under the data dir.
    pub(crate) fn open(cx: &FeatureContext<'_>) -> Result<Arc<Self>> {
        let _ = cx;
        todo!("W3: open Safety")
    }
}

/// The `policy` feature.
pub(crate) fn start(cx: &FeatureContext<'_>, safety: &Arc<Safety>) -> Result<Arc<dyn Feature>> {
    let _ = (cx, safety);
    todo!("W3: policy feature")
}
