mod common;

use common::*;
use nugget_core::capability::Locality;
use nugget_infer::{ChatEvent, ChatRequest, ChatResponse, Message, StopReason, ToolSpec};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const LOCAL: &str = "id = \"ollama\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"";
const KEYED: &str = "id = \"router\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\napi_key = \"env:OPENROUTER_API_KEY\"";

#[tokio::test]
async fn streams_text_with_usage_and_no_auth_header() {
    let server = MockServer::start().await;
    let body = sse_raw(&[
        r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}"#,
        r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
        r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        r#"{"id":"c1","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}"#,
        "[DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model": "test-model", "stream": true, "stream_options": {"include_usage": true}, "messages": [{"role": "user", "content": "hi"}]})))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let (p, sink) = provider(LOCAL, &format!("{}/v1", server.uri()));
    assert_eq!(p.meta().locality, Locality::OnDevice);
    let r = ChatResponse::collect(
        p.chat(ChatRequest::new(vec![Message::user_text("hi")]))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r.text(), "Hello");
    assert_eq!(r.stop_reason, StopReason::EndTurn);
    assert_eq!((r.input_tokens, r.output_tokens), (Some(9), Some(2)));
    let req = &server.received_requests().await.unwrap()[0];
    assert!(req.headers.get("authorization").is_none());
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn streams_tool_calls_with_bearer_auth() {
    let server = MockServer::start().await;
    let body = sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":""}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":\"rust"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":" sse\"}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#,
        "[DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(KEYED, &format!("{}/api/v1", server.uri()));
    let req = ChatRequest::new(vec![Message::user_text("find it")]).with_tools(vec![ToolSpec {
        name: "search".into(),
        description: "search".into(),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    }]);
    let events = collect_all(p.chat(req).await.unwrap()).await;
    let events: Vec<ChatEvent> = events.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        events,
        vec![
            ChatEvent::ToolCall {
                id: "call_1".into(),
                name: "search".into(),
                arguments: json!({"q": "rust sse"})
            },
            ChatEvent::Done {
                stop_reason: StopReason::ToolUse,
                input_tokens: Some(5),
                output_tokens: Some(7)
            },
        ]
    );
    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent["tools"][0]["function"]["parameters"]["type"], "object");
}

#[tokio::test]
async fn mid_stream_error_payload() {
    let server = MockServer::start().await;
    let body = sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"par"}}]}"#,
        r#"{"error":{"message":"Provider returned error","code":502}}"#,
    ]);
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;
    let (p, _) = provider(LOCAL, &format!("{}/v1", server.uri()));
    let events = collect_all(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(events.len(), 2);
    assert!(
        events[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("Provider returned error")
    );
}

#[tokio::test]
async fn rate_limited_then_gives_up() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({"error": "too many requests"})),
        )
        .expect(3)
        .mount(&server)
        .await;
    let (p, _) = provider(LOCAL, &format!("{}/v1", server.uri()));
    let msg = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(msg.contains("HTTP 429: too many requests"), "{msg}");
}

#[tokio::test]
async fn bad_gateway_502_retried_then_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response(sse_raw(&[
            r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ])))
        .mount(&server)
        .await;
    let (p, _) = provider(LOCAL, &format!("{}/v1", server.uri()));
    let r = ChatResponse::collect(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r.text(), "ok");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

/// Echoes one deterministic vector per input, in reverse order with explicit indexes.
struct EmbedResponder;

impl Respond for EmbedResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let inputs = body["input"].as_array().unwrap();
        let mut data: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"object": "embedding", "index": i, "embedding": [t.as_str().unwrap().len() as f32, 1.0, 0.5]}))
            .collect();
        data.reverse();
        ResponseTemplate::new(200)
            .set_body_json(json!({"object": "list", "data": data, "model": "test-model"}))
    }
}

#[tokio::test]
async fn embeddings_batch_and_keep_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(
            json!({"model": "test-model", "encoding_format": "float"}),
        ))
        .respond_with(EmbedResponder)
        .expect(2)
        .mount(&server)
        .await;
    let (p, _) = provider(
        &format!("{LOCAL}\nembed_batch_size = 2"),
        &format!("{}/v1", server.uri()),
    );
    let texts: Vec<String> = ["a", "bb", "ccc"].iter().map(|s| s.to_string()).collect();
    let v = p.embed(&texts).await.unwrap();
    assert_eq!(
        v,
        vec![
            vec![1.0, 1.0, 0.5],
            vec![2.0, 1.0, 0.5],
            vec![3.0, 1.0, 0.5]
        ]
    );
    assert!(p.embed(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn embeddings_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {"message": "model does not support embeddings", "type": "invalid_request_error"}})))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(LOCAL, &format!("{}/v1", server.uri()));
    let msg = p.embed(&["x".into()]).await.unwrap_err().to_string();
    assert!(msg.contains("does not support embeddings"), "{msg}");
}
