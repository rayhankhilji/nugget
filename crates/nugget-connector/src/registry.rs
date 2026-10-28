use std::collections::BTreeMap;
use std::sync::Arc;

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{Connector, ConnectorContext};

/// How a kind authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    None,
    /// Interactive OAuth via [`Connector::begin_auth`].
    OAuth,
    /// A password or app password set through `sources.secret.set`.
    Password,
}

/// Static description of a connector kind, for UIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindInfo {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub auth: AuthKind,
    /// JSON Schema of the `settings` object.
    pub settings_schema: Value,
}

/// Builds connectors of one kind.
pub trait ConnectorFactory: Send + Sync {
    fn info(&self) -> KindInfo;
    /// Reject bad settings early (before the source is saved).
    fn validate(&self, settings: &Value) -> Result<()>;
    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>>;
}

/// All connector kinds available in this build.
#[derive(Default, Clone)]
pub struct Registry {
    kinds: BTreeMap<&'static str, Arc<dyn ConnectorFactory>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory. Panics on a duplicate kind (a programming error).
    pub fn register(&mut self, factory: Arc<dyn ConnectorFactory>) {
        let kind = factory.info().kind;
        assert!(
            self.kinds.insert(kind, factory).is_none(),
            "connector kind `{kind}` registered twice"
        );
    }

    pub fn get(&self, kind: &str) -> Result<&Arc<dyn ConnectorFactory>> {
        self.kinds
            .get(kind)
            .ok_or_else(|| Error::NotFound(format!("unknown source kind `{kind}`")))
    }

    pub fn kinds(&self) -> Vec<KindInfo> {
        self.kinds.values().map(|f| f.info()).collect()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.kinds.keys()).finish()
    }
}
