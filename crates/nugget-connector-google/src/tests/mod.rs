use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    KnownItems, SecretStore,
};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::secrets::oauth_account;
use nugget_core::{Error, Result};
use serde_json::Value;
use wiremock::MockServer;

use crate::{GoogleEndpoints, RetryPolicy};

mod factory;
mod gcal;
mod gmail;

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

#[derive(Default)]
pub(crate) struct RecSink {
    pub changes: Mutex<Vec<Change>>,
    pub checkpoints: Mutex<Vec<Value>>,
}
#[async_trait]
impl ChangeSink for RecSink {
    async fn send(&self, change: Change) -> Result<()> {
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

    pub fn upserts(&self) -> Vec<nugget_connector::Document> {
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
}

pub(crate) struct Harness {
    pub server: MockServer,
    pub egress: Arc<VecSink>,
    pub secrets: Arc<MemSecrets>,
    pub known: Arc<FakeKnown>,
    pub source: SourceId,
}

pub(crate) const SOURCE: &str = "google:test";

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

    pub fn endpoints(&self) -> GoogleEndpoints {
        GoogleEndpoints::with_base(&self.server.uri())
    }

    pub fn ctx(&self) -> ConnectorContext {
        ConnectorContext {
            source: self.source.clone(),
            paths: Paths::rooted(std::env::temp_dir().join("nugget-google-test")),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: self.known.clone(),
        }
    }

    /// Store a token set valid for an hour (or already expired).
    pub fn seed_tokens(&self, access: &str, expired: bool) {
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
        max_delay: Duration::from_millis(50),
    }
}

pub(crate) async fn run(
    conn: &Arc<dyn Connector>,
    cursor: Option<Value>,
    sink: &RecSink,
) -> Result<Value> {
    conn.sync(cursor, sink, &CancellationToken::new()).await
}

pub(crate) fn is_unauthorized(r: &Result<Value>) -> bool {
    matches!(r, Err(Error::Unauthorized(_)))
}
