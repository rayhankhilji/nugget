//! Plugin connectors as ordinary [`ConnectorFactory`]s, so the ingest pipeline runs them
//! unchanged.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    AuthKind, ChangeSink, Connector, ConnectorContext, ConnectorFactory, KindInfo,
};
use nugget_core::{Error, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::host::LoadedPlugin;
use crate::manifest::{ConnectorAuth, PluginKind};

/// The source kind under which a plugin connector registers: `plugin:<id>`.
pub fn plugin_kind(plugin_id: &str) -> String {
    format!("plugin:{plugin_id}")
}

/// Builds sources backed by one plugin connector.
pub struct PluginConnectorFactory {
    plugin: Arc<LoadedPlugin>,
    kind: &'static str,
    display_name: &'static str,
}

impl std::fmt::Debug for PluginConnectorFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginConnectorFactory")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl PluginConnectorFactory {
    /// Fails when the plugin does not provide a connector.
    pub fn new(plugin: Arc<LoadedPlugin>) -> Result<Self> {
        let manifest = plugin.manifest();
        let section = manifest
            .connector
            .as_ref()
            .filter(|_| manifest.provides(PluginKind::Connector))
            .ok_or_else(|| {
                Error::Unsupported(format!("plugin `{}` is not a connector", manifest.id))
            })?;
        Ok(Self {
            kind: intern(plugin_kind(&manifest.id)),
            display_name: intern(section.display_name.clone()),
            plugin,
        })
    }
}

impl ConnectorFactory for PluginConnectorFactory {
    fn info(&self) -> KindInfo {
        let section = self.plugin.manifest().connector.as_ref();
        KindInfo {
            kind: self.kind,
            display_name: self.display_name,
            auth: match section.map(|s| s.auth) {
                Some(ConnectorAuth::Password) => AuthKind::Password,
                _ => AuthKind::None,
            },
            settings_schema: section
                .and_then(|s| s.settings_schema.clone())
                .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        if !settings.is_object() {
            return Err(Error::Invalid("settings must be a JSON object".into()));
        }
        self.plugin.validate_settings(settings)
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        if !settings.is_object() {
            return Err(Error::Invalid("settings must be a JSON object".into()));
        }
        let poll = self
            .plugin
            .manifest()
            .connector
            .as_ref()
            .and_then(|c| c.poll_interval_secs)
            .unwrap_or(1800)
            .max(60);
        Ok(Arc::new(PluginConnector {
            plugin: self.plugin.clone(),
            kind: self.kind,
            settings: serde_json::to_string(settings)?,
            ctx,
            poll_interval: Duration::from_secs(poll),
        }))
    }
}

/// One configured source backed by a plugin.
pub struct PluginConnector {
    plugin: Arc<LoadedPlugin>,
    kind: &'static str,
    settings: String,
    ctx: ConnectorContext,
    poll_interval: Duration,
}

impl std::fmt::Debug for PluginConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginConnector")
            .field("kind", &self.kind)
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Connector for PluginConnector {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.plugin
            .sync(&self.settings, cursor, &self.ctx, sink, cancel)
            .await
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval
    }
}

/// `KindInfo` and `Connector::kind` need `&'static str`; plugin ids are dynamic. Each distinct
/// string is leaked once, so reloading a plugin does not leak again.
fn intern(s: String) -> &'static str {
    static SET: OnceLock<Mutex<BTreeSet<&'static str>>> = OnceLock::new();
    let mut set = SET
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = set.get(s.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    set.insert(leaked);
    leaked
}
