//! `IngestBuilder::settings_resolver` and `Ingest::reload`: values injected at build time reach
//! the connector but never `sources.toml`, `sources.list` or events; a reload rebuilds with the
//! current values; accounts written through `set_secret` are deleted on removal.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_connector::{
    AuthKind, CancellationToken, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    KindInfo, Registry, SecretStore,
};
use nugget_core::Result;
use nugget_core::ids::SourceId;
use nugget_core::ipc::SourceSpec;
use nugget_core::paths::Paths;
use nugget_ingest::{Ingest, IngestConfig, MemorySecrets, NoPowerPolicy, SettingsResolver};
use nugget_store::Store;
use serde_json::{Value, json};

/// Records the settings of every build.
#[derive(Default)]
struct Recorder {
    built: Mutex<Vec<Value>>,
}

struct Idle;

#[async_trait]
impl Connector for Idle {
    fn kind(&self) -> &'static str {
        "rec"
    }
    async fn sync(
        &self,
        cursor: Option<Value>,
        _sink: &dyn ChangeSink,
        _cancel: &CancellationToken,
    ) -> Result<Value> {
        Ok(cursor.unwrap_or(json!(1)))
    }
}

struct Factory(Arc<Recorder>);

impl ConnectorFactory for Factory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: "rec",
            display_name: "Recorder",
            auth: AuthKind::OAuth,
            settings_schema: json!({"type": "object"}),
        }
    }
    fn validate(&self, _settings: &Value) -> Result<()> {
        Ok(())
    }
    fn build(&self, settings: &Value, _ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        self.0.built.lock().unwrap().push(settings.clone());
        Ok(Arc::new(Idle))
    }
}

#[tokio::test]
async fn resolver_injects_at_build_time_only_and_reload_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(dir.path().to_path_buf());
    let rec = Arc::new(Recorder::default());
    let mut registry = Registry::new();
    registry.register(Arc::new(Factory(rec.clone())));
    let secrets = MemorySecrets::new();
    let account = |id: &SourceId| format!("oauth_client_secret:{id}");
    let resolver: SettingsResolver = {
        let secrets = secrets.clone();
        Arc::new(move |id, kind, mut settings| {
            assert_eq!(kind, "rec");
            if let Some(v) = secrets.get(&account(id))? {
                settings["client_secret"] = Value::String(v);
            }
            Ok(settings)
        })
    };
    let ingest = Ingest::builder(
        IngestConfig::default(),
        Store::open_in_memory().unwrap(),
        registry,
        paths.clone(),
    )
    .secrets(Arc::new(secrets.clone()))
    .power(Arc::new(NoPowerPolicy))
    .settings_resolver(resolver)
    .start()
    .await
    .unwrap();
    let mut events = ingest.subscribe();

    let info = ingest
        .add(SourceSpec {
            id: None,
            kind: "rec".into(),
            name: "r".into(),
            settings: json!({"client_id": "abc"}),
        })
        .await
        .unwrap();
    let id = info.id.clone();
    assert_eq!(
        rec.built.lock().unwrap().last().unwrap(),
        &json!({"client_id": "abc"})
    );

    ingest
        .set_secret(&id, &account(&id), "s3cr3t-value")
        .await
        .unwrap();
    ingest.reload(&id).await.unwrap();
    assert_eq!(
        rec.built.lock().unwrap().last().unwrap(),
        &json!({"client_id": "abc", "client_secret": "s3cr3t-value"})
    );

    // Never persisted or reported.
    let file = std::fs::read_to_string(paths.config.join(nugget_ingest::SOURCES_FILE)).unwrap();
    assert!(!file.contains("s3cr3t-value"), "{file}");
    assert!(
        file.contains(&account(&id)),
        "account name recorded: {file}"
    );
    let listed = serde_json::to_string(&ingest.list().await.unwrap()).unwrap();
    assert!(!listed.contains("s3cr3t-value"));
    while let Ok(ev) = events.try_recv() {
        assert!(!format!("{ev:?}").contains("s3cr3t-value"));
    }

    assert!(ingest.reload(&SourceId::new("nope")).await.is_err());
    ingest.remove(&id, true).await.unwrap();
    assert!(
        secrets.accounts().is_empty(),
        "client secret deleted on removal: {:?}",
        secrets.accounts()
    );
    ingest.shutdown().await;
}
