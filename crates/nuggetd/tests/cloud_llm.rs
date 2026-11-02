//! Heed cloud model routing through IPC (`cloud.llm.session`, `cloud.llm.status`) against a
//! test double of the cloud endpoint and a local provider: the handed-over token makes the
//! cloud providers serve first (fast tier for light work, smart for synthesis) with the token
//! as bearer and every request in the egress ledger under the cloud host; quota answers,
//! sign-out and expiry fall back to the local model; bad handoffs are refused; the token is in
//! no audit entry, status or listing.

mod common;

use common::*;
use nugget_core::egress::EgressEvent;
use nugget_core::ipc::{CloudLlmStatus, ProviderInfo, method};
use serde_json::{Value, json};
use wiremock::matchers::{header, method as http_method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "eyJ.cloud-access-token.sig";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn anthropic_sse(text: &str) -> String {
    [
        json!({"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "model": "heed-fast", "content": [], "usage": {"input_tokens": 5, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|d| format!("event: {}\ndata: {d}\n\n", d["type"].as_str().unwrap()))
    .collect()
}

async fn start() -> (Harness, MockServer, MockServer) {
    let cloud = MockServer::start().await;
    let local = MockServer::start().await;
    Mock::given(http_method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_text("from local"))
        .mount(&local)
        .await;
    let config = format!(
        "{NO_RUNTIME}\n[[providers]]\nid = \"local-chat\"\nkind = \"openai_compatible\"\n\
         base_url = \"{}/v1\"\nmodel = \"small\"\ntiers = [\"light\", \"synthesis\"]\nmax_retries = 0\n",
        local.uri()
    );
    (Harness::start(&config).await, cloud, local)
}

async fn session(h: &Harness, params: Value) -> CloudLlmStatus {
    h.call(method::CLOUD_LLM_SESSION, params).await
}

async fn served_by(h: &Harness, tier: &str) -> (String, String) {
    let watcher = collect(&h.client);
    let (started, text) = h.chat(&watcher, chat_params(tier, "hello")).await;
    (started.unwrap().provider.to_string(), text.unwrap())
}

#[tokio::test]
async fn signed_in_the_cloud_serves_first_and_local_is_the_fallback() {
    let (h, cloud, _local) = start().await;
    assert_eq!(
        served_by(&h, "light").await.0,
        "local-chat",
        "signed out: local"
    );

    Mock::given(http_method("POST"))
        .and(path("/llm/v1/messages"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_sse("from cloud")),
        )
        .up_to_n_times(2)
        .mount(&cloud)
        .await;
    let st = session(
        &h,
        json!({"api_base": cloud.uri(), "access_token": TOKEN, "expires_at": now() + 3600}),
    )
    .await;
    assert!(st.active, "{st:?}");
    assert_eq!(
        st.api_host.as_deref(),
        Some(cloud.address().to_string().as_str())
    );
    assert_eq!(st.providers.len(), 2);
    assert!(!serde_json::to_string(&st).unwrap().contains(TOKEN));

    assert_eq!(
        served_by(&h, "light").await,
        ("heed-cloud-fast".into(), "from cloud".into())
    );
    assert_eq!(
        served_by(&h, "synthesis").await,
        ("heed-cloud-smart".into(), "from cloud".into())
    );
    let sent: Vec<Value> = cloud
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.body_json().unwrap())
        .collect();
    assert_eq!(sent[0]["model"], "heed-fast");
    assert_eq!(sent[1]["model"], "heed-smart");

    // Listed (with Cloud locality), and in the egress ledger under the cloud's host.
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    let fast = list
        .iter()
        .find(|p| p.id.as_str() == "heed-cloud-fast")
        .unwrap();
    assert_eq!(fast.locality, nugget_core::capability::Locality::Cloud);
    let events: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
    let host = cloud.address().to_string();
    assert!(
        events.iter().filter(|e| e.host == host).count() >= 2,
        "{events:?}"
    );

    // Quota spent (429): the local model answers, at once.
    Mock::given(http_method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "type": "error", "error": {"type": "rate_limit_error", "message": "monthly AI quota used up", "code": "quota_exceeded"}
        })))
        .mount(&cloud)
        .await;
    assert_eq!(served_by(&h, "light").await.0, "local-chat");

    // Signed out: the cloud providers leave the router; nothing more goes to the cloud.
    let before = cloud.received_requests().await.unwrap().len();
    let st = session(&h, json!({"access_token": null})).await;
    assert!(!st.active && st.providers.is_empty());
    assert_eq!(served_by(&h, "synthesis").await.0, "local-chat");
    assert_eq!(cloud.received_requests().await.unwrap().len(), before);
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert!(!list.iter().any(|p| p.id.as_str().starts_with("heed-cloud")));

    // The token is in no audit entry; on and off are.
    let audit = h.audit();
    assert!(audit.iter().any(|(a, ..)| a == "cloud.llm.on"));
    assert!(audit.iter().any(|(a, ..)| a == "cloud.llm.off"));
    assert!(!format!("{audit:?}").contains(TOKEN));
}

#[tokio::test]
async fn bad_handoffs_are_refused() {
    let (h, cloud, _local) = start().await;
    let n = now();
    for (params, why) in [
        (
            json!({"api_base": cloud.uri(), "access_token": TOKEN, "expires_at": n - 1}),
            "expired",
        ),
        (
            json!({"api_base": cloud.uri(), "access_token": TOKEN, "expires_at": n + 5}),
            "inside the margin",
        ),
        (
            json!({"api_base": cloud.uri(), "access_token": TOKEN, "expires_at": n + 90 * 24 * 3600}),
            "long-lived",
        ),
        (
            json!({"api_base": cloud.uri(), "access_token": TOKEN}),
            "no expiry",
        ),
        (
            json!({"api_base": "http://heed.example", "access_token": TOKEN, "expires_at": n + 600}),
            "plain http off-device",
        ),
        (
            json!({"access_token": TOKEN, "expires_at": n + 600}),
            "no api_base",
        ),
        (
            json!({"api_base": cloud.uri(), "access_token": "has space", "expires_at": n + 600}),
            "not a token",
        ),
    ] {
        let (code, msg) = h.call_err(method::CLOUD_LLM_SESSION, params).await;
        assert_eq!(code, nugget_ipc::code::INVALID_PARAMS, "{why}: {msg}");
        assert!(!msg.contains(TOKEN), "{why}: {msg}");
    }
    let st: CloudLlmStatus = h.call(method::CLOUD_LLM_STATUS, json!({})).await;
    assert!(!st.active);
    assert!(cloud.received_requests().await.unwrap().is_empty());
}

/// A refreshed token replaces the old one without rebuilding the providers; a token the cloud
/// refuses (401) stops cloud use until the next handoff.
#[tokio::test]
async fn refreshed_tokens_are_used_and_a_refused_one_falls_back() {
    let (h, cloud, _local) = start().await;
    Mock::given(http_method("POST"))
        .and(path("/llm/v1/messages"))
        .and(header("authorization", "Bearer second"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_sse("fresh")),
        )
        .mount(&cloud)
        .await;
    Mock::given(http_method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error", "error": {"type": "authentication_error", "message": "sign in"}
        })))
        .mount(&cloud)
        .await;
    let base = cloud.uri();
    session(
        &h,
        json!({"api_base": base, "access_token": "first", "expires_at": now() + 3600}),
    )
    .await;
    assert_eq!(
        served_by(&h, "light").await.0,
        "local-chat",
        "401: fell back"
    );
    let st: CloudLlmStatus = h.call(method::CLOUD_LLM_STATUS, json!({})).await;
    assert!(!st.active, "a refused token is dropped");
    session(
        &h,
        json!({"api_base": base, "access_token": "second", "expires_at": now() + 3600}),
    )
    .await;
    assert_eq!(
        served_by(&h, "light").await,
        ("heed-cloud-fast".into(), "fresh".into())
    );
}
