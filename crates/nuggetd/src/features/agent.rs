//! `[agent]`: the agent runtime (`nugget-agent`). Serves `agent.*` and broadcasts `agent.event`.
//!
//! SKELETON (W3 lead).

use std::sync::Arc;

use nugget_core::Result;

use super::policy::Safety;
use super::{Feature, FeatureContext, start};

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
    let _ = (cx, safety, tools);
    todo!("W3: agent feature")
}
