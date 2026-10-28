//! The agent adapter end to end: the hostile test plugin's tools through [`agent_tools`].
//! Returns early when the guests can't be built (see [`guest_wasm`]).

use std::sync::Arc;
use std::time::Duration;

use nugget_agent::ToolCtx;
use nugget_policy::RiskClass;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{HostConfig, Limits, LoadedPlugin, PluginHost, PluginTool, agent_tools};

const ID: &str = "dev.nugget.test.agent-tools";

fn load(kind: &str) -> Option<(tempfile::TempDir, Arc<LoadedPlugin>)> {
    let wasm = guest_wasm("hostile_plugin")?;
    let (dir, store) = temp_store();
    let k = key(11);
    store
        .install(
            &package(&manifest_toml(ID, "1.0.0", kind, ""), &wasm, &k),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let host = PluginHost::new(HostConfig {
        limits: Limits {
            memory_bytes: 32 * 1024 * 1024,
            call_timeout: Duration::from_secs(20),
            ..Limits::default()
        },
        state_dir: store.state_root(),
        egress: Arc::new(VecEgress::default()),
        secrets: None,
        http_client: None,
        allow_private_network: false,
    })
    .unwrap();
    let plugin = host.load(&store.load(ID).unwrap()).unwrap();
    Some((dir, plugin))
}

fn ctx() -> ToolCtx {
    ToolCtx {
        task_id: "t".into(),
        action_id: "a".into(),
        step: 1,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn plugin_tools_run_through_the_agent_adapter() {
    let Some((_dir, plugin)) = load("[\"connector\", \"tool\"]") else {
        return;
    };
    let tools = agent_tools(plugin.clone()).await.unwrap();
    let specs = plugin.tools().await.unwrap();
    assert_eq!(tools.len(), specs.len());
    let echo = tools
        .iter()
        .find(|t| t.name() == format!("plugin.{ID}.echo"))
        .expect("echo tool");
    assert_eq!(echo.declared_risk(), RiskClass::Observe);
    assert_eq!(
        echo.risk(&json!({ "method": "DELETE" })),
        RiskClass::Observe
    );
    assert!(echo.description().contains("hostile: echo"));
    assert_eq!(echo.schema(), json!({ "type": "object" }));
    let target = echo.describe(&json!({ "a": 1 }));
    assert_eq!(target.target_app, format!("plugin:{ID}"));
    assert_eq!(target.verb, "echo");

    let r = echo.call(json!({ "a": 1 }), &ctx()).await;
    assert!(!r.is_error);
    assert!(r.untrusted);
    assert_eq!(r.data, Some(json!({ "a": 1 })));
    assert_eq!(r.content, r#"{"a":1}"#);

    // Guest failures come back as untrusted errors, not panics.
    let panic = tools.iter().find(|t| t.name().ends_with(".panic")).unwrap();
    let r = panic.call(json!({}), &ctx()).await;
    assert!(r.is_error && r.untrusted);

    // A cancelled run does not call the plugin.
    let c = ctx();
    c.cancel.cancel();
    let r = echo.call(json!({}), &c).await;
    assert!(r.is_error);
    assert_eq!(r.content, "cancelled");

    let debug = format!("{:?}", PluginTool::new(plugin.clone(), specs[0].clone()));
    assert!(
        debug.starts_with("PluginTool") && debug.contains(ID),
        "{debug}"
    );
}

#[tokio::test]
async fn connector_only_plugins_have_no_agent_tools() {
    let Some((_dir, plugin)) = load("[\"connector\"]") else {
        return;
    };
    assert!(agent_tools(plugin).await.unwrap().is_empty());
}
