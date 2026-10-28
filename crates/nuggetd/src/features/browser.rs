//! `[browser]`: the browser extension bridge. Serves `browser.status` and routes
//! `browser.host.*` to a `nugget_browser::BridgeHub`, so the native host can attach to the
//! daemon socket; provides the browser agent tools.
//!
//! The tools' controller is gated twice: the agent loop evaluates the policy (and asks the user)
//! before each call, then the controller re-evaluates with what it sees (a
//! [`RecheckGate`] over the same engine, so the rate limit is counted once) and answers its own
//! confirmations only from what the loop just approved (see `nugget_browser::tool`, "Double
//! gating"); anything else fails closed. The Stop button in the page engages this daemon's kill
//! switch.
//!
//! ```toml
//! [browser]
//! enabled = true   # false: no browser tools and no host connection; browser.status still served
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use nugget_browser::{BridgeHub, HubHandler, loop_gated_tools};
use nugget_core::Result;
use nugget_core::ipc::{BrowserStatus, agent_method as m};
use nugget_ipc::{Notifier, RpcError, to_result};
use nugget_policy::{KillSwitch, RecheckGate};
use serde::Deserialize;
use serde_json::Value;

use super::policy::Safety;
use super::{Feature, FeatureContext, section};

/// The browser feature, with what the agent feature needs from it.
pub(crate) struct Browser {
    #[allow(
        dead_code,
        reason = "seam for the agent feature (events, status); not read yet"
    )]
    pub hub: Arc<BridgeHub>,
    /// Browser tools for the agent's registry (empty when `[browser] enabled = false`).
    pub tools: Vec<Arc<dyn nugget_agent::Tool>>,
    pub feature: Arc<dyn Feature>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct BrowserSection {
    enabled: bool,
}

impl Default for BrowserSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// The extension's Stop engages the daemon's kill switch (the policy feature broadcasts it).
struct StopHalts(KillSwitch);

impl nugget_browser::KillSwitch for StopHalts {
    fn engage(&self, reason: &str) {
        tracing::warn!(reason, "browser Stop: halting every executor");
        self.0.halt();
    }
}

pub(crate) fn start(cx: &FeatureContext<'_>, safety: &Arc<Safety>) -> Result<Browser> {
    let cfg: BrowserSection = section("browser", &cx.config.browser)?;
    Ok(build(cfg.enabled, safety))
}

fn build(enabled: bool, safety: &Arc<Safety>) -> Browser {
    let hub = Arc::new(BridgeHub::new(Some(Arc::new(StopHalts(
        safety.kill.clone(),
    )))));
    let tools = if enabled {
        let gate = Arc::new(RecheckGate(safety.engine.clone()));
        loop_gated_tools(hub.clone(), gate).1
    } else {
        Vec::new()
    };
    let feature = Arc::new(BrowserFeature {
        hub: hub.clone(),
        handler: HubHandler::new(hub.clone()),
        enabled,
        tool_names: tools.iter().map(|t| t.name().to_owned()).collect(),
    });
    Browser {
        hub,
        tools,
        feature,
    }
}

const METHODS: &[&str] = &[
    m::BROWSER_STATUS,
    m::BROWSER_HOST_ATTACH,
    m::BROWSER_HOST_DELIVER,
];
const STATUS_ONLY: &[&str] = &[m::BROWSER_STATUS];

struct BrowserFeature {
    hub: Arc<BridgeHub>,
    handler: HubHandler,
    enabled: bool,
    tool_names: Vec<String>,
}

impl BrowserFeature {
    fn status(&self) -> BrowserStatus {
        let info = self.hub.attached_info();
        BrowserStatus {
            connected: info.is_some(),
            origin: info.as_ref().map(|i| i.origin.clone()),
            host_version: info.map(|i| i.host_version),
            tools: self.tool_names.clone(),
        }
    }
}

#[async_trait]
impl Feature for BrowserFeature {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn methods(&self) -> &'static [&'static str] {
        if self.enabled { METHODS } else { STATUS_ONLY }
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        match method {
            m::BROWSER_STATUS => to_result(self.status()),
            m::BROWSER_HOST_ATTACH | m::BROWSER_HOST_DELIVER if self.enabled => {
                self.handler.call(method, params, notify).await
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_browser::bridge::AttachInfo;
    use nugget_browser::{BrowserEvent, ExtensionMessage};

    #[test]
    fn tools_status_and_stop_button() {
        let dir = tempfile::tempdir().unwrap();
        let safety = Safety::open_in(dir.path()).unwrap();
        let b = build(true, &safety);
        let names: Vec<&str> = b.tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"browser_snapshot"));
        assert!(names.contains(&"browser_submit"));
        assert_eq!(b.feature.methods(), METHODS);

        let f = BrowserFeature {
            hub: b.hub.clone(),
            handler: HubHandler::new(b.hub.clone()),
            enabled: true,
            tool_names: names.iter().map(|s| s.to_string()).collect(),
        };
        assert!(!f.status().connected);
        let (_gen, _rx) = b.hub.attach(AttachInfo {
            origin: "chrome-extension://abc/".into(),
            host_version: "1.0".into(),
        });
        let st = f.status();
        assert!(st.connected);
        assert_eq!(st.origin.as_deref(), Some("chrome-extension://abc/"));
        assert_eq!(st.tools.len(), names.len());

        // The page's Stop button halts this daemon's executors.
        assert!(!safety.engine.halted());
        b.hub
            .deliver(ExtensionMessage::Event(BrowserEvent::UserStop {
                reason: "user".into(),
            }));
        assert!(safety.engine.halted());
        assert!(!nugget_policy::KillSwitch::global().halted());
    }

    #[test]
    fn disabled_has_no_tools_but_status() {
        let dir = tempfile::tempdir().unwrap();
        let safety = Safety::open_in(dir.path()).unwrap();
        let b = build(false, &safety);
        assert!(b.tools.is_empty());
        assert_eq!(b.feature.methods(), STATUS_ONLY);
        let t: toml::Table = toml::from_str("enabled = false").unwrap();
        assert!(!section::<BrowserSection>("browser", &t).unwrap().enabled);
        assert!(
            section::<BrowserSection>("browser", &toml::Table::new())
                .unwrap()
                .enabled
        );
    }
}
