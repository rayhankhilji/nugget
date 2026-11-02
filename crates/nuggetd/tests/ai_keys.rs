//! "Bring your own key" through IPC (`ai.key.test`, `ai.key.save`) against a mock
//! OpenAI-compatible server: a good key lists models and is saved (OS keychain stand-in plus a
//! `keychain:NAME` provider in config.toml, never the key), a bad key and a wrong address are
//! explained, a taken name is refused, and every request is in the egress ledger. A saved
//! provider is routed at once (`providers.list`), its key can be replaced (`ai.key.update`) and
//! it can be removed (`ai.key.remove`) without a restart; without a working keychain the save
//! answers `no_keychain` and an `env:` reference is the fallback.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use nugget_connector::SecretStore;
use nugget_core::egress::EgressEvent;
use nugget_core::ipc::method;
use nugget_core::secrets::provider_account;
use nugget_ipc::{IpcError, code};
use serde_json::{Value, json};
use wiremock::matchers::{header, method as http_method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GOOD: &str = "sk-e2e-good";
const ROTATED: &str = "sk-e2e-rotated";

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
        .and(header(
            "authorization",
            format!("Bearer {ROTATED}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "e2e-model"}],
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(http_method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
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

async fn call(h: &Harness, m: &str, params: Value) -> Result<Value, IpcError> {
    h.try_call(m, params).await
}

#[tokio::test]
async fn keys_are_checked_saved_to_the_secret_store_and_recorded() {
    let mut h = Harness::start(NO_RUNTIME).await;
    let secrets = h.secrets.clone();
    let paths = h.paths.clone();
    let provider = mock_provider().await;
    let base = format!("{}/v1", provider.uri());

    // Test: good key, bad key, wrong path.
    let ok = call(
        &h,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": base, "api_key": GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["reason"], "ok");
    assert_eq!(ok["models"], json!(["e2e-model", "e2e-other"]));
    let bad = call(
        &h,
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
        &h,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": provider.uri(), "api_key": GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(wrong["reason"], "wrong_url", "{wrong}");

    // Malformed params are errors; validation outcomes never are.
    let err = call(&h, "ai.key.test", json!({"kind": "nope", "api_key": GOOD}))
        .await
        .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INVALID_PARAMS));
    let err = call(
        &h,
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
    let r = call(&h, "ai.key.save", save.clone()).await.unwrap();
    assert_eq!(r["saved"], false, "{r}");
    assert_eq!(r["reason"], "bad_key");
    assert!(secrets.get(&provider_account("custom")).unwrap().is_none());

    // A good key is.
    let mut good = save.clone();
    good["api_key"] = json!(GOOD);
    let r = call(&h, "ai.key.save", good.clone()).await.unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(r["provider_id"], "custom");
    assert_eq!(r["restart_required"], false);
    assert_eq!(
        secrets.get(&provider_account("custom")).unwrap().as_deref(),
        Some(GOOD)
    );
    let config = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(!config.contains(GOOD), "plaintext key in config.toml");
    assert!(config.contains("api_key = \"keychain:custom\""), "{config}");
    assert!(config.contains("[runtime]"), "existing settings kept");

    // Routed right away, no restart.
    let listed = provider_ids(&h).await;
    assert!(listed.contains(&"custom".to_owned()), "{listed:?}");
    let saved = call(&h, "ai.key.saved", json!({})).await.unwrap();
    assert_eq!(saved["providers"][0]["id"], "custom", "{saved}");
    assert_eq!(saved["providers"][0]["key"], "keychain");
    assert_eq!(saved["providers"][0]["available"], true, "{saved}");

    // The same name again is refused.
    let r = call(&h, "ai.key.save", good.clone()).await.unwrap();
    assert_eq!(
        (r["saved"].clone(), r["reason"].clone()),
        (json!(false), json!("exists"))
    );

    // Every request to the provider is in the ledger (3 tests + 2 saves; `exists` is refused
    // before going out), and the key is in no audit entry.
    let events: Vec<EgressEvent> = serde_json::from_value(
        call(&h, method::EGRESS_RECENT, json!({"limit": 50}))
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

    // Replace the key: a bad one is refused and changes nothing; a good one is stored.
    let r = call(
        &h,
        "ai.key.update",
        json!({"id": "custom", "api_key": "sk-e2e-bad"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (r["saved"].clone(), r["reason"].clone()),
        (json!(false), json!("bad_key"))
    );
    assert_eq!(
        secrets.get(&provider_account("custom")).unwrap().as_deref(),
        Some(GOOD)
    );
    let r = call(
        &h,
        "ai.key.update",
        json!({"id": "custom", "api_key": ROTATED}),
    )
    .await
    .unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(r["restart_required"], false);
    assert_eq!(
        secrets.get(&provider_account("custom")).unwrap().as_deref(),
        Some(ROTATED)
    );
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        config
    );
    assert!(provider_ids(&h).await.contains(&"custom".to_owned()));
    // A routed chat now reaches the provider with the new key.
    call(
        &h,
        method::CHAT,
        json!({"provider": "custom", "tier": "light", "messages": [{"role": "user", "text": "hi"}], "items": []}),
    )
    .await
    .unwrap();
    let deadline = std::time::Instant::now() + CALL;
    let chat_auth = loop {
        let reqs = provider.received_requests().await.unwrap_or_default();
        if let Some(r) = reqs.iter().find(|r| r.url.path() == "/v1/chat/completions") {
            break r
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no chat request reached the provider"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        chat_auth.as_deref(),
        Some(format!("Bearer {ROTATED}").as_str())
    );
    // Nugget's own names are refused.
    let mut local = good.clone();
    local["name"] = json!("local-chat");
    let r = call(&h, "ai.key.save", local).await.unwrap();
    assert_eq!(
        (r["saved"].clone(), r["reason"].clone()),
        (json!(false), json!("reserved")),
        "{r}"
    );
    // Unknown ids are refused.
    let r = call(&h, "ai.key.update", json!({"id": "nope", "api_key": GOOD}))
        .await
        .unwrap();
    assert_eq!(r["reason"], "not_found", "{r}");
    let r = call(&h, "ai.key.remove", json!({"id": "nope"}))
        .await
        .unwrap();
    assert_eq!(
        (r["removed"].clone(), r["reason"].clone()),
        (json!(false), json!("not_found"))
    );

    // An env: reference to a variable the daemon does not have is explained, not saved.
    let r = call(
        &h,
        "ai.key.save",
        json!({
            "name": "from-env", "kind": "openai_compatible", "base_url": base,
            "key_ref": "env:NUGGET_TEST_SURELY_UNSET_7F3A_KEY", "model": "e2e-model",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        (r["saved"].clone(), r["reason"].clone()),
        (json!(false), json!("env_missing"))
    );
    // Any other variable of the daemon (PATH is always set) is not read, and the answer is the
    // same as for a missing one, so nothing reveals which variables exist.
    let other = call(
        &h,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": base, "key_ref": "env:PATH"}),
    )
    .await
    .unwrap();
    assert_eq!(other["reason"], "env_missing", "{other}");
    let missing = call(
        &h,
        "ai.key.test",
        json!({"kind": "openai_compatible", "base_url": base, "key_ref": "env:NUGGET_TEST_SURELY_UNSET_7F3A_KEY"}),
    )
    .await
    .unwrap();
    assert_eq!(
        other["message"].as_str().unwrap().replace("PATH", "VAR"),
        missing["message"]
            .as_str()
            .unwrap()
            .replace("NUGGET_TEST_SURELY_UNSET_7F3A_KEY", "VAR")
    );
    let err = call(
        &h,
        "ai.key.save",
        json!({"name": "x", "kind": "openai_compatible", "base_url": base, "key_ref": "env:1-bad", "model": "m"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INVALID_PARAMS));

    // Remove: gone from the router, config.toml (byte for byte as before the save) and the
    // secret store.
    let r = call(&h, "ai.key.remove", json!({"id": "custom"}))
        .await
        .unwrap();
    assert_eq!(r["removed"], true, "{r}");
    assert!(!provider_ids(&h).await.contains(&"custom".to_owned()));
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        NO_RUNTIME
    );
    assert!(secrets.get(&provider_account("custom")).unwrap().is_none());
    let saved = call(&h, "ai.key.saved", json!({})).await.unwrap();
    assert_eq!(saved["providers"], json!([]));

    h.stop().await;
}

async fn provider_ids(h: &Harness) -> Vec<String> {
    let list = call(h, method::PROVIDERS_LIST, json!({})).await.unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect()
}

/// A secret store like Linux without a Secret Service: every call fails.
struct NoKeyring;
impl SecretStore for NoKeyring {
    fn get(&self, _: &str) -> nugget_core::Result<Option<String>> {
        Err(nugget_core::Error::Storage(
            "keychain: Platform secure storage failure: no secret service provider or dbus session found".into(),
        ))
    }
    fn set(&self, a: &str, _: &str) -> nugget_core::Result<()> {
        self.get(a).map(|_| ())
    }
    fn delete(&self, a: &str) -> nugget_core::Result<()> {
        self.get(a).map(|_| ())
    }
}

#[tokio::test]
async fn without_a_keychain_the_save_explains_and_writes_nothing() {
    let before = NO_RUNTIME;
    let mut h = Harness::start_with(before, |o| o.source_secrets = Some(Arc::new(NoKeyring))).await;
    let paths = h.paths.clone();
    let provider = mock_provider().await;
    let base = format!("{}/v1", provider.uri());

    let r = call(
        &h,
        "ai.key.save",
        json!({"name": "my-openai", "kind": "openai_compatible", "base_url": base, "api_key": GOOD, "model": "e2e-model"}),
    )
    .await
    .unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["reason"], "no_keychain");
    assert_eq!(r["saved"], false);
    assert_eq!(r["suggested_env"], "NUGGET_MY_OPENAI_KEY");
    let msg = r["message"].as_str().unwrap();
    assert!(msg.contains("environment variable"), "{msg}");
    assert!(!msg.contains("dbus") && !msg.contains(GOOD), "{msg}");
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        before
    );
    assert!(!provider_ids(&h).await.contains(&"my-openai".to_owned()));

    h.stop().await;
}

/// A server on this computer is saved without a key (no keychain entry, no `api_key`), listed
/// as on-device, used for triage too, and removed again; a cloud kind still needs a key.
#[tokio::test]
async fn local_servers_are_saved_without_a_key() {
    let mut h = Harness::start(NO_RUNTIME).await;
    let server = MockServer::start().await;
    Mock::given(http_method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "data": [{"id": "qwen3:4b"}],
        })))
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());

    let detected = call(&h, "ai.local.detect", json!({})).await.unwrap();
    assert!(detected["servers"].is_array(), "{detected}");

    let r = call(
        &h,
        "ai.key.save",
        json!({"name": "ollama", "kind": "openai_compatible", "base_url": base, "model": "qwen3:4b"}),
    )
    .await
    .unwrap();
    assert_eq!(r["saved"], true, "{r}");
    let config = std::fs::read_to_string(h.paths.config_file()).unwrap();
    assert!(!config.contains("api_key"), "{config}");
    assert!(config.contains("\"triage\""), "{config}");
    assert!(
        h.secrets
            .get(&provider_account("ollama"))
            .unwrap()
            .is_none()
    );
    assert!(provider_ids(&h).await.contains(&"ollama".to_owned()));
    let saved = call(&h, "ai.key.saved", json!({})).await.unwrap();
    let p = &saved["providers"][0];
    assert_eq!(
        (p["id"].clone(), p["key"].clone(), p["locality"].clone()),
        (json!("ollama"), json!("none"), json!("on_device")),
        "{saved}"
    );
    // A keyless server has no key to replace.
    let r = call(
        &h,
        "ai.key.update",
        json!({"id": "ollama", "api_key": GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(r["reason"], "not_editable", "{r}");

    let r = call(&h, "ai.key.remove", json!({"id": "ollama"}))
        .await
        .unwrap();
    assert_eq!(r["removed"], true, "{r}");
    assert!(!provider_ids(&h).await.contains(&"ollama".to_owned()));

    // A cloud provider without a key is malformed.
    let err = call(
        &h,
        "ai.key.save",
        json!({"name": "x", "kind": "openai_compatible", "base_url": "https://api.example.com/v1", "model": "m"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INVALID_PARAMS));
    h.stop().await;
}
