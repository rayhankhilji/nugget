//! Daemon features: subsystems wired in after M2 (Double Tap, agent and safety, browser,
//! proactive cards, plugins).
//!
//! Each feature is one module that owns its state, its IPC methods, its background tasks and its
//! `[section]` of `config.toml` (a raw table in [`Config`]'s feature sections, parsed by the
//! feature with [`section`]). Features are added without touching [`State`] or `handler.rs`:
//! implement [`Feature`] in `features/<name>.rs`, declare the module below and add one line to
//! [`build`].
//!
//! Order of events: the daemon builds [`State`], then calls [`build`]; a feature that fails to
//! start is logged and left out (its methods answer "method not found"), so one broken feature
//! never stops the daemon. Methods are matched after the built-in ones in `handler.rs`. On
//! shutdown, [`Features::shutdown`] runs after the listener closed and before ingest stops.

// The seam lands before its first feature; drop this once one is registered in `build`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Config;
use crate::daemon::State;

/// One wired-in subsystem.
#[async_trait]
pub(crate) trait Feature: Send + Sync {
    /// Short name, used in logs.
    fn name(&self) -> &'static str;
    /// The IPC methods this feature serves. A method may belong to one feature only.
    fn methods(&self) -> &'static [&'static str];
    /// Serve one of [`Feature::methods`].
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError>;
    /// A connection opened (subscribe it to this feature's notifications, if any).
    fn connected(&self, _notify: &Notifier) {}
    /// Stop background work. Tasks spawned on `State::tasks` that watch `State::shutdown` stop
    /// on their own; this is for anything else (flushing, closing handles).
    async fn shutdown(&self) {}
}

/// What a feature's constructor gets.
pub(crate) struct FeatureContext<'a> {
    pub state: &'a Arc<State>,
    pub config: &'a Config,
}

/// Parse a feature's `[section]` (missing or empty means `T::default()`), naming the section in
/// the error.
pub(crate) fn section<T: DeserializeOwned + Default>(name: &str, table: &toml::Table) -> Result<T> {
    if table.is_empty() {
        return Ok(T::default());
    }
    toml::Value::Table(table.clone())
        .try_into()
        .map_err(|e| Error::Invalid(format!("[{name}]: {e}")))
}

/// The started features.
#[derive(Default)]
pub(crate) struct Features {
    list: Vec<Arc<dyn Feature>>,
    by_method: HashMap<&'static str, usize>,
}

impl Features {
    fn new(list: Vec<Arc<dyn Feature>>) -> Self {
        let mut by_method = HashMap::new();
        for (i, f) in list.iter().enumerate() {
            for m in f.methods() {
                if let Some(prev) = by_method.insert(*m, i) {
                    panic!(
                        "IPC method {m} is served by both {} and {}",
                        list[prev].name(),
                        f.name()
                    );
                }
            }
        }
        Self { list, by_method }
    }

    /// The feature serving `method`, if any.
    pub(crate) fn find(&self, method: &str) -> Option<&Arc<dyn Feature>> {
        self.by_method.get(method).map(|&i| &self.list[i])
    }

    pub(crate) fn connected(&self, notify: &Notifier) {
        for f in &self.list {
            f.connected(notify);
        }
    }

    pub(crate) async fn shutdown(&self) {
        for f in &self.list {
            f.shutdown().await;
        }
    }

    /// Names of the running features (status, tests).
    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.list.iter().map(|f| f.name()).collect()
    }
}

/// Start every feature. One line per feature; keep them in this order.
pub(crate) async fn build(cx: &FeatureContext<'_>) -> Features {
    let mut list: Vec<Arc<dyn Feature>> = Vec::new();
    // W1 Double Tap:   start(&mut list, "draft", draft::start(cx).await);
    // W3 agent/safety: start(&mut list, "agent", agent::start(cx).await);
    // W4 proactive:    start(&mut list, "proactive", proactive::start(cx).await);
    // W6 plugins:      start(&mut list, "plugins", plugins::start(cx).await);
    let _ = cx;
    let features = Features::new(std::mem::take(&mut list));
    tracing::info!(features = ?features.names(), "features started");
    features
}

/// Keep a started feature, or log why it could not start and leave it out.
fn start(list: &mut Vec<Arc<dyn Feature>>, name: &str, started: Result<Arc<dyn Feature>>) {
    match started {
        Ok(f) => list.push(f),
        Err(e) => tracing::error!(feature = name, error = %e, "feature did not start"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields, default)]
    struct Demo {
        enabled: bool,
        n: u32,
    }

    #[test]
    fn section_defaults_when_empty_and_names_the_section_on_error() {
        assert_eq!(
            section::<Demo>("demo", &toml::Table::new()).unwrap(),
            Demo::default()
        );
        let t: toml::Table = toml::from_str("enabled = true\nn = 3").unwrap();
        assert_eq!(
            section::<Demo>("demo", &t).unwrap(),
            Demo {
                enabled: true,
                n: 3
            }
        );
        let bad: toml::Table = toml::from_str("nope = 1").unwrap();
        let err = section::<Demo>("demo", &bad).unwrap_err().to_string();
        assert!(err.contains("[demo]"), "{err}");
    }

    struct Echo;
    #[async_trait]
    impl Feature for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn methods(&self) -> &'static [&'static str] {
            &["echo.say"]
        }
        async fn call(&self, _: &str, params: Value, _: Notifier) -> Result<Value, RpcError> {
            Ok(params)
        }
    }

    #[test]
    fn methods_route_to_their_feature() {
        let f = Features::new(vec![Arc::new(Echo)]);
        assert_eq!(f.find("echo.say").map(|f| f.name()), Some("echo"));
        assert!(f.find("echo.other").is_none());
        assert_eq!(f.names(), ["echo"]);
    }

    #[test]
    #[should_panic(expected = "served by both")]
    fn duplicate_methods_are_refused() {
        Features::new(vec![Arc::new(Echo), Arc::new(Echo)]);
    }
}
