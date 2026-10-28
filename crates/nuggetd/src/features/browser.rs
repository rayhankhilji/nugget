//! `[browser]`: the browser extension bridge. Serves `browser.status` and routes
//! `browser.host.*` to a `nugget_browser::BridgeHub`, so the native host can attach to the
//! daemon socket; provides the browser agent tools.
//!
//! SKELETON (W3 lead): keep the signatures (they are the seam with `agent.rs`).

use std::sync::Arc;

use nugget_browser::BridgeHub;
use nugget_core::Result;

use super::policy::Safety;
use super::{Feature, FeatureContext};

/// The browser feature, with what the agent feature needs from it.
pub(crate) struct Browser {
    pub hub: Arc<BridgeHub>,
    /// Browser tools for the agent's registry (empty when `[browser] enabled = false`).
    pub tools: Vec<Arc<dyn nugget_agent::Tool>>,
    pub feature: Arc<dyn Feature>,
}

pub(crate) fn start(cx: &FeatureContext<'_>, safety: &Arc<Safety>) -> Result<Browser> {
    let _ = (cx, safety);
    todo!("W3: browser feature")
}
