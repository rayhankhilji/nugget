use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory, Document,
    KnownItems, SecretStore,
};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::secrets::{oauth_account, password_account};
use nugget_core::{Error, Result};
use serde_json::Value;
use wiremock::MockServer;

use crate::{Endpoints, RetryPolicy};

mod factory;
mod github;
mod linear;
mod notion;
mod slack;

#[derive(Default)]
pub(crate) struct VecSink(pub Mutex<Vec<EgressEvent>>);
impl EgressSink for VecSink {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

#[derive(Default)]
pub(crate) struct MemSecrets(pub Mutex<HashMap<String, String>>);
impl SecretStore for MemSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FakeKnown(pub Mutex<HashMap<ItemKey, Option<String>>>);
#[async_trait]
impl KnownItems for FakeKnown {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// Records changes, and (like the pipeline) applies them to `known` so later passes see them.
#[derive(Default)]
pub(crate) struct RecSink {
    pub changes: Mutex<Vec<Change>>,
    pub checkpoints: Mutex<Vec<Value>>,
    pub known: Option<Arc<FakeKnown>>,
}
#[async_trait]
impl ChangeSink for RecSink {
    async fn send(&self, change: Change) -> Result<()> {
        if let Some(k) = &self.known {
            let mut k = k.0.lock().unwrap();
            match &change {
                Change::Upsert(d) => {
                    k.insert(d.key.clone(), d.version.clone());
                }
                Change::Delete(key) => {
                    k.remove(key);
                }
            }
        }
        self.changes.lock().unwrap().push(change);
        Ok(())
    }
    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        self.checkpoints.lock().unwrap().push(cursor);
        Ok(())
    }
}

impl RecSink {
    /// `+key` for upserts, `-key` for deletes, in order.
    pub fn summary(&self) -> Vec<String> {
        self.changes
            .lock()
            .unwrap()
            .iter()
            .map(|c| match c {
                Change::Upsert(d) => format!("+{}", d.key),
                Change::Delete(k) => format!("-{k}"),
            })
            .collect()
    }

    pub fn upserts(&self) -> Vec<Document> {
        self.changes
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                Change::Upsert(d) => Some(d.clone()),
                Change::Delete(_) => None,
            })
            .collect()
    }

    pub fn doc(&self, key: &str) -> Document {
        self.upserts()
            .into_iter()
            .rev()
            .find(|d| d.key.as_str() == key)
            .unwrap_or_else(|| panic!("no upsert for {key}: {:?}", self.summary()))
    }

    pub fn text(&self, key: &str) -> String {
        match self.doc(key).content {
            nugget_connector::Content::Text(t) => t,
            other => panic!("not text: {other:?}"),
        }
    }
}

pub(crate) struct Harness {
    pub server: MockServer,
    pub egress: Arc<VecSink>,
    pub secrets: Arc<MemSecrets>,
    pub known: Arc<FakeKnown>,
    pub source: SourceId,
}

pub(crate) const SOURCE: &str = "saas:test";

impl Harness {
    pub async fn new() -> Self {
        Self {
            server: MockServer::start().await,
            egress: Arc::new(VecSink::default()),
            secrets: Arc::new(MemSecrets::default()),
            known: Arc::new(FakeKnown::default()),
            source: SourceId::new(SOURCE),
        }
    }

    pub fn endpoints(&self) -> Endpoints {
        Endpoints::with_base(&self.server.uri())
    }

    pub fn ctx(&self) -> ConnectorContext {
        ConnectorContext {
            source: self.source.clone(),
            paths: Paths::rooted(std::env::temp_dir().join("nugget-saas-test")),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: self.known.clone(),
        }
    }

    /// A sink that keeps `known` in step, like the ingest pipeline.
    pub fn sink(&self) -> RecSink {
        RecSink {
            known: Some(self.known.clone()),
            ..Default::default()
        }
    }

    /// A pasted token (`sources.secret.set`).
    pub fn paste(&self, token: &str) {
        self.secrets.set(&password_account(SOURCE), token).unwrap();
    }

    /// OAuth tokens valid for an hour (or already expired).
    pub fn seed_oauth(&self, access: &str, expired: bool) {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let t = serde_json::json!({
            "access_token": access,
            "refresh_token": "rt",
            "expires_at": if expired { now - 10 } else { now + 3600 },
        });
        self.secrets
            .set(&oauth_account(SOURCE), &t.to_string())
            .unwrap();
    }

    pub fn know(&self, key: &str, version: &str) {
        self.known
            .0
            .lock()
            .unwrap()
            .insert(ItemKey::new(key), Some(version.into()));
    }

    pub fn known_keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self
            .known
            .0
            .lock()
            .unwrap()
            .keys()
            .map(|k| k.as_str().to_string())
            .collect();
        k.sort();
        k
    }

    pub fn build(&self, factory: &dyn ConnectorFactory, settings: Value) -> Arc<dyn Connector> {
        factory.build(&settings, self.ctx()).unwrap()
    }

    /// Every request reached the mock server and was recorded with `feature`.
    pub async fn assert_egress(&self, feature: &str) {
        let requests = self.server.received_requests().await.unwrap();
        let events = self.egress.0.lock().unwrap();
        assert_eq!(events.len(), requests.len(), "one egress row per request");
        assert!(!events.is_empty());
        let host = format!(
            "127.0.0.1:{}",
            url::Url::parse(&self.server.uri()).unwrap().port().unwrap()
        );
        for e in events.iter() {
            assert_eq!(e.feature, feature);
            assert_eq!(e.host, host);
            assert!(e.bytes_sent > 0);
        }
    }
}

pub(crate) fn fast_retry(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(200),
    }
}

pub(crate) async fn run(
    conn: &Arc<dyn Connector>,
    cursor: Option<Value>,
    sink: &RecSink,
) -> Result<Value> {
    conn.sync(cursor, sink, &CancellationToken::new()).await
}

pub(crate) fn is_unauthorized<T: std::fmt::Debug>(r: &Result<T>) -> bool {
    matches!(r, Err(Error::Unauthorized(_)))
}
