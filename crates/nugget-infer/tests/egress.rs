mod common;

use std::sync::Arc;

use common::*;
use futures::StreamExt;
use nugget_core::Error;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_infer::{ChatRequest, ChatResponse, Message, ProviderConfig, build_provider_with};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OAI: &str = "id = \"p\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"";

fn done_stream() -> String {
    sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ])
}

#[tokio::test]
async fn non_local_requests_record_feature_items_and_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_response(done_stream()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"index": 0, "embedding": [0.1, 0.2]}]})),
        )
        .mount(&server)
        .await;
    let (p, sink) = provider(
        &format!("{OAI}\nlocality = \"local_network\""),
        &format!("{}/v1", server.uri()),
    );
    assert_eq!(p.meta().locality, Locality::LocalNetwork);
    let item = ItemRef {
        source: SourceId::new("files:~/notes"),
        key: ItemKey::new("todo.md"),
    };
    let mut req =
        ChatRequest::new(vec![Message::user_text("summarise my note")]).with_feature("draft");
    req.items = vec![item.clone()];
    let body_len = {
        let r = ChatResponse::collect(p.chat(req).await.unwrap())
            .await
            .unwrap();
        assert_eq!(r.text(), "ok");
        server.received_requests().await.unwrap()[0].body.len() as u64
    };
    p.embed(&["x".into()]).await.unwrap();

    let events = sink.events();
    assert_eq!(events.len(), 2);
    let chat = &events[0];
    assert_eq!(chat.feature, "draft");
    assert_eq!(chat.items, vec![item]);
    assert_eq!(chat.host, format!("127.0.0.1:{}", server.address().port()));
    assert_eq!(chat.bytes_sent, body_len);
    assert_eq!(chat.bytes_received, done_stream().len() as u64);
    let embed = &events[1];
    assert_eq!(embed.feature, "embed");
    assert!(embed.items.is_empty());
    assert!(embed.bytes_received > 0);
}

#[tokio::test]
async fn on_device_records_nothing_even_on_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let (p, sink) = provider(
        &format!("{OAI}\nmax_retries = 1"),
        &format!("{}/v1", server.uri()),
    );
    assert_eq!(p.meta().locality, Locality::OnDevice);
    assert!(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .is_err()
    );
    assert!(p.embed(&["x".into()]).await.is_err());
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn dropped_stream_still_records_partial_bytes() {
    let server = MockServer::start().await;
    let body = sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"one"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"two"}}]}"#,
    ]);
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;
    let (p, sink) = provider(
        &format!("{OAI}\nlocality = \"cloud\""),
        &format!("{}/v1", server.uri()),
    );
    let mut stream = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert!(sink.events().is_empty(), "recorded once the exchange ends");
    drop(stream);
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(events[0].bytes_received > 0);
}

#[tokio::test]
async fn refused_connections_record_zero_bytes_per_attempt() {
    // Grab a free port, then close it so connections are refused.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (p, sink) = provider(
        &format!("{OAI}\nlocality = \"cloud\"\nmax_retries = 1"),
        &format!("http://127.0.0.1:{port}/v1"),
    );
    let err = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, Error::Network(m) if m.contains("connection failed")),
        "{err}"
    );
    let events = sink.events();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|e| e.bytes_sent == 0 && e.bytes_received == 0)
    );
}

#[test]
fn locality_from_config_without_network() {
    let sink = Arc::new(RecordingSink::default());
    for (url, want) in [
        ("http://localhost:11434/v1", Locality::OnDevice),
        ("http://192.168.1.9:1234/v1", Locality::LocalNetwork),
        ("https://openrouter.ai/api/v1", Locality::Cloud),
    ] {
        let cfg = ProviderConfig::from_toml_str(&format!("{OAI}\nbase_url = \"{url}\"")).unwrap();
        let p = build_provider_with(&cfg, sink.clone(), &FakeSecrets).unwrap();
        assert_eq!(p.meta().locality, want, "{url}");
    }
}

#[test]
fn missing_api_key_fails_to_build() {
    let cfg = ProviderConfig::from_toml_str(
        "id = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\napi_key = \"env:MISSING\"",
    )
    .unwrap();
    let err = build_provider_with(&cfg, Arc::new(RecordingSink::default()), &FakeSecrets)
        .err()
        .unwrap();
    assert!(
        matches!(err, Error::NotFound(ref m) if m.contains("env:MISSING")),
        "{err}"
    );
}

/// A provider that answers 3xx must not move the key, headers or body to another host behind the
/// ledger's back: the redirect is reported, the exchange with the original host is recorded, and
/// the target receives nothing.
#[tokio::test]
async fn redirects_are_not_followed() {
    let target = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(sse_response(done_stream()))
        .expect(0)
        .mount(&target)
        .await;
    let origin = MockServer::start().await;
    for (p, status) in [("/v1/chat/completions", 307), ("/v1/embeddings", 302)] {
        Mock::given(method("POST"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", format!("{}{p}", target.uri()).as_str()),
            )
            .mount(&origin)
            .await;
    }
    let (p, sink) = provider(
        &format!(
            "{OAI}\nlocality = \"cloud\"\napi_key = \"env:K\"\nmax_retries = 2\n\
             headers = {{ \"x-custom\" = \"v\" }}"
        ),
        &format!("{}/v1", origin.uri()),
    );
    let want = format!(
        "unexpected redirect to 127.0.0.1:{}",
        target.address().port()
    );
    let err = match p
        .chat(ChatRequest::new(vec![Message::user_text("secret note")]))
        .await
    {
        Err(e) => e,
        Ok(s) => collect_all(s)
            .await
            .into_iter()
            .find_map(|r| r.err())
            .unwrap(),
    };
    assert!(
        matches!(&err, Error::Provider(m) if m.contains(&want)),
        "{err}"
    );
    let err = p.embed(&["x".into()]).await.unwrap_err();
    assert!(
        matches!(&err, Error::Provider(m) if m.contains(&want)),
        "{err}"
    );

    assert!(target.received_requests().await.unwrap().is_empty());
    // Not retried; each exchange with the origin is recorded once.
    assert_eq!(origin.received_requests().await.unwrap().len(), 2);
    let events = sink.events();
    assert_eq!(events.len(), 2);
    let origin_host = format!("127.0.0.1:{}", origin.address().port());
    assert!(
        events
            .iter()
            .all(|e| e.host == origin_host && e.bytes_sent > 0)
    );
}
