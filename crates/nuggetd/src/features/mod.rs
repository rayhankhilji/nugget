//! Daemon features: subsystems (Double Tap, agent and safety, browser,
//! proactive cards, plugins).
//!
//! Each feature is one module that owns its state, its IPC methods, its background tasks and its
//! `[section]` of `config.toml` (a raw table in [`Config`]'s feature sections, parsed by the
//! feature with [`section`]). Features are added without touching [`State`] or `handler.rs`:
//! implement [`Feature`] in `features/<name>.rs`, declare the module below and add one line to
//! [`build`].
//!
//! Order of events: the daemon builds [`State`], then calls [`build`]; a feature that fails to
//! start is logged, listed in `status.failed_features`, and its methods (when known) answer with
//! the reason, so one broken feature never stops the daemon. Methods are matched after the built-in ones in `handler.rs`. On
//! shutdown, [`Features::shutdown`] runs after the listener closed and before ingest stops.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Config;
use crate::daemon::State;

mod agent;
mod ai_keys;
pub(crate) mod backup;
mod browser;
pub(crate) mod draft;
mod model_fit;
pub(crate) mod plugins;
mod policy;
mod proactive;
mod runtime_retry;
mod search_ui;

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
    /// Why the feature did not start, for a stand-in ([`Failed`]).
    fn failure(&self) -> Option<&str> {
        None
    }
}

/// Stands in for a feature that did not start (a bad `[section]`): its methods answer with the
/// reason instead of "method not found" (GG4-18).
struct Failed {
    name: &'static str,
    methods: &'static [&'static str],
    error: String,
}

#[async_trait]
impl Feature for Failed {
    fn name(&self) -> &'static str {
        self.name
    }
    fn methods(&self) -> &'static [&'static str] {
        self.methods
    }
    async fn call(&self, method: &str, _: Value, _: Notifier) -> Result<Value, RpcError> {
        Err(RpcError::new(
            nugget_ipc::code::UNSUPPORTED,
            format!("{method}: {} did not start: {}", self.name, self.error),
        ))
    }
    fn failure(&self) -> Option<&str> {
        Some(&self.error)
    }
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
        self.list
            .iter()
            .filter(|f| f.failure().is_none())
            .map(|f| f.name())
            .collect()
    }

    /// Features that did not start, with why.
    pub(crate) fn failed(&self) -> std::collections::BTreeMap<String, String> {
        self.list
            .iter()
            .filter_map(|f| Some((f.name().to_owned(), f.failure()?.to_owned())))
            .collect()
    }
}

/// Start every feature. One line per feature; keep them in this order.
pub(crate) async fn build(cx: &FeatureContext<'_>) -> Features {
    let mut list: Vec<Arc<dyn Feature>> = Vec::new();
    start(&mut list, "draft", &[], draft::start(cx).await);
    // Plugins before the agent, which registers their tools.
    start(&mut list, "plugins", &[], plugins::start(cx).await);
    let safety = agent::register(cx, &mut list).await; // policy, browser, agent
    start(
        &mut list,
        "proactive",
        proactive::METHODS,
        proactive::start(cx, safety.as_ref()).await,
    );
    start(&mut list, "ai_keys", &[], ai_keys::start(cx));
    start(&mut list, "model_fit", &[], model_fit::start(cx));
    start(&mut list, "backup", &[], backup::start(cx));
    start(&mut list, "search_ui", &[], search_ui::start(cx)); // faceted search, timelines, previews
    start(&mut list, "runtime_retry", &[], runtime_retry::start(cx));
    let features = Features::new(std::mem::take(&mut list));
    tracing::info!(features = ?features.names(), "features started");
    features
}

/// Keep a started feature, or log why it could not start and keep a [`Failed`] stand-in
/// serving `methods`.
fn start(
    list: &mut Vec<Arc<dyn Feature>>,
    name: &'static str,
    methods: &'static [&'static str],
    started: Result<Arc<dyn Feature>>,
) {
    match started {
        Ok(f) => list.push(f),
        Err(e) => {
            tracing::error!(feature = name, error = %e, "feature did not start");
            list.push(Arc::new(Failed {
                name,
                methods,
                error: e.to_string(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// GG4-18: a feature that did not start is listed, and its methods say why.
    #[test]
    fn a_failed_feature_says_why() {
        let mut list = Vec::new();
        start(
            &mut list,
            "proactive",
            &["brief.get"],
            Err(Error::Invalid(
                "[proactive]: brief_time \"25:00\" is not HH:MM".into(),
            )),
        );
        let f = Features::new(list);
        assert!(f.names().is_empty());
        assert_eq!(f.failed().len(), 1);
        let stand_in = f.find("brief.get").unwrap();
        assert!(stand_in.failure().unwrap().contains("brief_time"));
    }
}
