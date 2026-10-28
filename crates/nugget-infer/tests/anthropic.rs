mod common;

use common::*;
use nugget_core::Error;
use nugget_core::capability::Locality;
use nugget_infer::{
    ChatEvent, ChatRequest, ChatResponse, Message, Part, ProviderKind, StopReason, ToolSpec,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CFG: &str = "id = \"claude\"\nkind = \"anthropic\"\nmodel = \"test-model\"\napi_key = \"env:ANTHROPIC_API_KEY\"";

fn text_stream() -> String {
    sse(&[
        (
            Some("message_start"),
            json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "usage": {"input_tokens": 12, "output_tokens": 1}}}),
        ),
        (
            Some("content_block_start"),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (Some("ping"), json!({"type": "ping"})),
        (
            Some("content_block_delta"),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}}),
        ),
        (
            Some("content_block_delta"),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": " world"}}),
        ),
        (
            Some("content_block_stop"),
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            Some("message_delta"),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 6}}),
        ),
        (Some("message_stop"), json!({"type": "message_stop"})),
    ])
}

#[tokio::test]
async fn streams_text_with_required_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2026-10-21"))
        .and(header("accept", "text/event-stream"))
        .and(body_partial_json(
            json!({"model": "test-model", "stream": true, "max_tokens": 16000}),
        ))
        .respond_with(sse_response(text_stream()))
        .expect(1)
        .mount(&server)
        .await;
    let (p, sink) = provider(CFG, &server.uri());
    assert_eq!(p.meta().kind, ProviderKind::Anthropic);
    assert_eq!(p.meta().locality, Locality::OnDevice);
    let stream = p
        .chat(ChatRequest::new(vec![Message::user_text("hi")]).with_system("be nice"))
        .await
        .unwrap();
    let r = ChatResponse::collect(stream).await.unwrap();
    assert_eq!(r.text(), "Hello world");
    assert_eq!(r.stop_reason, StopReason::EndTurn);
    assert_eq!((r.input_tokens, r.output_tokens), (Some(12), Some(6)));
    // Loopback server: on-device, nothing recorded.
    assert!(sink.events().is_empty());
    let req = &server.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["system"], "be nice");
    assert_eq!(
        body["messages"][0],
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})
    );
}

#[tokio::test]
async fn tool_calls_images_and_structured_output() {
    let server = MockServer::start().await;
    let body = sse(&[
        (
            None,
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 50}}}),
        ),
        (
            None,
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        ),
        (
            None,
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig=="}}),
        ),
        (None, json!({"type": "content_block_stop", "index": 0})),
        (
            None,
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_01", "name": "lookup", "input": {}}}),
        ),
        (
            None,
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": ""}}),
        ),
        (
            None,
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"city\": \"Ber"}}),
        ),
        (
            None,
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "lin\", \"days\": 3}"}}),
        ),
        (None, json!({"type": "content_block_stop", "index": 1})),
        (
            None,
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
        ),
        (None, json!({"type": "message_stop"})),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &server.uri());
    let mut req = ChatRequest::new(vec![Message::user(vec![
        Part::image("image/png", "iVBORw0KGgo="),
        Part::text("weather?"),
    ])])
    .with_tools(vec![ToolSpec {
        name: "lookup".into(),
        description: "Look up weather".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}, "required": ["city"]}),
    }])
    .with_json_schema(json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}));
    req.temperature = Some(0.5);
    let r = ChatResponse::collect(p.chat(req).await.unwrap())
        .await
        .unwrap();
    assert_eq!(r.stop_reason, StopReason::ToolUse);
    let calls = r.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_01");
    assert_eq!(calls[0].arguments, json!({"city": "Berlin", "days": 3}));

    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        sent["messages"][0]["content"][0],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}})
    );
    assert_eq!(sent["tools"][0]["name"], "lookup");
    assert_eq!(sent["tools"][0]["eager_input_streaming"], true);
    assert_eq!(sent["output_config"]["format"]["type"], "json_schema");
    assert!(
        sent.get("temperature").is_none(),
        "temperature is rejected on 2026-10-21"
    );

    // The assistant turn (with its thinking block) replays verbatim on the next request.
    let msg = r.into_message();
    assert!(matches!(&msg.content[0], Part::Opaque { data, .. } if data["signature"] == "sig=="));
}

#[tokio::test]
async fn mid_stream_error_event_ends_stream_with_error() {
    let server = MockServer::start().await;
    let body = sse(&[
        (
            None,
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
        ),
        (
            None,
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            None,
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
        ),
        (
            Some("error"),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ]);
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &server.uri());
    let events = collect_all(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].as_ref().unwrap(),
        &ChatEvent::TextDelta("partial".into())
    );
    let err = events[1].as_ref().unwrap_err().to_string();
    assert!(err.contains("overloaded_error"), "{err}");
}

#[tokio::test]
async fn retries_429_then_succeeds_and_records_each_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}})),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response(text_stream()))
        .expect(1)
        .mount(&server)
        .await;
    // A localhost gateway declared as cloud: egress must be recorded.
    let (p, sink) = provider(&format!("{CFG}\nlocality = \"cloud\""), &server.uri());
    assert_eq!(p.meta().locality, Locality::Cloud);
    let r = ChatResponse::collect(
        p.chat(ChatRequest::new(vec![Message::user_text("hi")]))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r.text(), "Hello world");
    let events = sink.events();
    assert_eq!(events.len(), 2, "one ledger entry per attempt");
    for e in &events {
        assert_eq!(e.feature, "chat");
        assert!(e.host.starts_with("127.0.0.1:"));
        assert!(e.bytes_sent > 0);
        assert!(e.bytes_received > 0);
    }
    assert_eq!(events[1].bytes_received as usize, text_stream().len());
}

#[tokio::test]
async fn overloaded_529_exhausts_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_json(
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    let (p, _) = provider(&format!("{CFG}\nmax_retries = 1"), &server.uri());
    let err = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap();
    let msg = err.to_string();
    assert!(matches!(err, Error::Provider(_)));
    assert!(
        msg.contains("HTTP 529") && msg.contains("overloaded_error: Overloaded"),
        "{msg}"
    );
}

#[tokio::test]
async fn server_error_500_without_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal"))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(&format!("{CFG}\nmax_retries = 0"), &server.uri());
    let msg = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("HTTP 500") && msg.contains("internal"),
        "{msg}"
    );
}

#[tokio::test]
async fn auth_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &server.uri());
    let msg = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("authentication_error") && msg.contains("api_key"),
        "{msg}"
    );
    assert!(!msg.contains("test-key"), "secret leaked into error: {msg}");
}

#[tokio::test]
async fn json_instead_of_stream_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "message", "content": []})),
        )
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &server.uri());
    let events = collect_all(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("expected an event stream")
    );
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let server = MockServer::start().await;
    let body = sse(&[
        (
            None,
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
        ),
        (
            None,
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "Hi"}}),
        ),
    ]);
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &server.uri());
    let events = collect_all(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        events[0].as_ref().unwrap(),
        &ChatEvent::TextDelta("Hi".into())
    );
    assert!(
        events[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("ended before completion")
    );
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn embeddings_unsupported() {
    let (p, _) = provider(CFG, "http://127.0.0.1:9");
    assert!(matches!(
        p.embed(&["x".to_string()]).await,
        Err(Error::Unsupported(_))
    ));
}
