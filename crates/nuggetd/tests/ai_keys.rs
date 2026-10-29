//! "Bring your own key" through IPC (`ai.key.test`, `ai.key.save`) against a mock
//! OpenAI-compatible server: a good key lists models and is saved (OS keychain stand-in plus a
//! `keychain:NAME` provider in config.toml, never the key), a bad key and a wrong address are
//! explained, a taken name is refused, and every request is in the egress ledger.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::SecretStore;
use nugget_core::egress::EgressEvent;
use nugget_core::ipc::method;
use nugget_core::paths::Paths;
use nugget_core::secrets::provider_account;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, IpcError, code};
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use wiremock::matchers::{header, method as http_method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const WAIT: Duration = Duration::from_secs(30);
const KEY: [u8; 32] = [9; 32];
const GOOD: &str = "sk-e2e-good";

async fn mock_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(http_method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {GOOD}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "e2e-model"}, {"id": "e2e-other"}],
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(http_method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "invalid key"})))
        .with_priority(2)
        .mount(&server)
        .await;
    server
}

async fn call(client: &Client, m: &str, params: Value) -> Result<Value, IpcError> {
    tokio::time::timeout(WAIT, client.call(m, &params))
        .await
        .unwrap_or_else(|_| panic!("{m} timed out"))
}

#[tokio::test]
async fn keys_are_checked_saved_to_the_secret_store_and_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), "[runtime]\nenabled = false\n").unwrap();
    let secrets = Arc::new(MemorySecrets::new());
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes(KEY));
    opts.source_secrets = Some(secrets.clone());
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();
    let provider = mock_provider().await;
    let base = format!("{}/v1", provider.uri());

    // Test: good key, bad key, wrong path.
    let ok = call(
        &client,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": base, "api_key": GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["reason"], "ok");
    assert_eq!(ok["models"], json!(["e2e-model", "e2e-other"]));
    let bad = call(
        &client,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": base, "api_key": "sk-e2e-bad"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (bad["ok"].clone(), bad["reason"].clone()),
        (json!(false), json!("bad_key"))
    );
    let wrong = call(
        &client,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": provider.uri(), "api_key": GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(wrong["reason"], "wrong_url", "{wrong}");

    // Malformed params are errors; validation outcomes never are.
    let err = call(
        &client,
        "ai.key.test",
        json!({"kind": "nope", "api_key": GOOD}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INVALID_PARAMS));
    let err = call(
        &client,
        "ai.key.save",
        json!({"name": "Bad Name", "kind": "openai_compatible", "base_url": base, "api_key": GOOD, "model": "m"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INVALID_PARAMS));

    // A bad key is not saved.
    let save = json!({
        "name": "custom", "kind": "openai_compatible", "base_url": base,
        "api_key": "sk-e2e-bad", "model": "e2e-model",
    });
    let r = call(&client, "ai.key.save", save.clone()).await.unwrap();
    assert_eq!(r["saved"], false, "{r}");
    assert_eq!(r["reason"], "bad_key");
    assert!(secrets.get(&provider_account("custom")).unwrap().is_none());

    // A good key is.
    let mut good = save.clone();
    good["api_key"] = json!(GOOD);
    let r = call(&client, "ai.key.save", good.clone()).await.unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(r["provider_id"], "custom");
    assert_eq!(r["restart_required"], true);
    assert_eq!(
        secrets.get(&provider_account("custom")).unwrap().as_deref(),
        Some(GOOD)
    );
    let config = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(!config.contains(GOOD), "plaintext key in config.toml");
    assert!(config.contains("api_key = \"keychain:custom\""), "{config}");
    assert!(config.contains("[runtime]"), "existing settings kept");

    // The same name again is refused.
    let r = call(&client, "ai.key.save", good).await.unwrap();
    assert_eq!(
        (r["saved"].clone(), r["reason"].clone()),
        (json!(false), json!("exists"))
    );

    // Every request to the provider is in the ledger (3 tests + 2 saves; `exists` is refused
    // before going out), and the key is in no audit entry.
    let events: Vec<EgressEvent> = serde_json::from_value(
        call(&client, method::EGRESS_RECENT, json!({"limit": 50}))
            .await
            .unwrap(),
    )
    .unwrap();
    let ours: Vec<_> = events.iter().filter(|e| e.feature == "ai_setup").collect();
    assert_eq!(ours.len(), 5, "{events:?}");
    let host = provider.address().to_string();
    assert!(
        ours.iter().all(|e| e.host == host && e.bytes_sent > 0),
        "{ours:?}"
    );

    let _ = tx.send(());
    tokio::time::timeout(WAIT, server)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}
