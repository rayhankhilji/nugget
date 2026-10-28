mod common;

use common::*;
use nugget_core::Error;
use nugget_infer::{
    ChatEvent, ChatRequest, ChatResponse, Message, Part, ProviderKind, StopReason, ToolSpec,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CFG: &str = "id = \"oai\"\nkind = \"openai_responses\"\nmodel = \"test-model\"\napi_key = \"env:OPENAI_API_KEY\"";

fn ev(data: Value) -> (Option<&'static str>, Value) {
    // The Responses API names each SSE event after its `type`; the decoder reads the JSON.
    (None, data)
}

#[tokio::test]
async fn streams_text_reasoning_and_tool_calls() {
    let server = MockServer::start().await;
    let body = sse(&[
        ev(
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
        ),
        ev(
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
        ),
        ev(
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAA"}}),
        ),
        ev(
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
        ),
        ev(
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 1, "content_index": 0, "delta": "Checking"}),
        ),
        ev(
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 1, "content_index": 0, "delta": " now."}),
        ),
        ev(
            json!({"type": "response.output_text.done", "item_id": "msg_1", "output_index": 1, "content_index": 0, "text": "Checking now."}),
        ),
        ev(
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_abc", "name": "lookup", "arguments": ""}}),
        ),
        ev(
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 2, "delta": "{\"city\":"}),
        ),
        ev(
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 2, "delta": "\"Rome\"}"}),
        ),
        ev(
            json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "output_index": 2, "arguments": "{\"city\":\"Rome\"}"}),
        ),
        ev(
            json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_abc", "name": "lookup", "arguments": "{\"city\":\"Rome\"}", "status": "completed"}}),
        ),
        ev(
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": 40, "output_tokens": 25, "total_tokens": 65}}}),
        ),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_partial_json(json!({"model": "test-model", "stream": true, "store": false, "include": ["reasoning.encrypted_content"]})))
        .respond_with(sse_response(body))
        .expect(2)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &format!("{}/v1", server.uri()));
    assert_eq!(p.meta().kind, ProviderKind::OpenaiResponses);
    let req = ChatRequest::new(vec![Message::user(vec![
        Part::text("weather in Rome?"),
        Part::image("image/webp", "UklGRg=="),
    ])])
    .with_system("You are terse.")
    .with_tools(vec![ToolSpec {
        name: "lookup".into(),
        description: "weather".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false}),
    }]);
    let r = ChatResponse::collect(p.chat(req).await.unwrap())
        .await
        .unwrap();
    assert_eq!(r.text(), "Checking now.");
    assert_eq!(r.stop_reason, StopReason::ToolUse);
    assert_eq!((r.input_tokens, r.output_tokens), (Some(40), Some(25)));
    let calls = r.tool_calls();
    assert_eq!(calls[0].id, "call_abc");
    assert_eq!(calls[0].arguments, json!({"city": "Rome"}));
    assert!(
        matches!(&r.parts[0], Part::Opaque { kind: ProviderKind::OpenaiResponses, data } if data["encrypted_content"] == "gAAA")
    );

    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent["instructions"], "You are terse.");
    assert_eq!(
        sent["input"][0]["content"][1]["image_url"],
        "data:image/webp;base64,UklGRg=="
    );
    assert_eq!(sent["tools"][0]["type"], "function");
    assert_eq!(sent["tools"][0]["strict"], true);

    // Follow-up turn replays reasoning + call and sends the tool output.
    let mut history = vec![Message::user_text("weather in Rome?"), r.into_message()];
    history.push(Message::user(vec![Part::tool_result("call_abc", "sunny")]));
    ChatResponse::collect(p.chat(ChatRequest::new(history)).await.unwrap())
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[1].body).unwrap();
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(
        input[2],
        json!({"role": "assistant", "content": "Checking now."})
    );
    assert_eq!(input[3]["type"], "function_call");
    assert_eq!(
        input[4],
        json!({"type": "function_call_output", "call_id": "call_abc", "output": "sunny"})
    );
}

#[tokio::test]
async fn structured_output_request_shape() {
    let server = MockServer::start().await;
    let body = sse(&[
        ev(json!({"type": "response.output_text.delta", "delta": "{\"ok\":true}"})),
        ev(
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 1, "output_tokens": 2}}}),
        ),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_partial_json(json!({"text": {"format": {"type": "json_schema", "name": "output", "strict": true}}, "max_output_tokens": 64})))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &format!("{}/v1", server.uri()));
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let r = ChatResponse::collect(
        p.chat(
            ChatRequest::new(vec![Message::user_text("ok?")])
                .with_json_schema(schema)
                .with_max_tokens(64),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r.text(), "{\"ok\":true}");
    assert_eq!(r.stop_reason, StopReason::EndTurn);
}

#[tokio::test]
async fn failed_response_and_error_events() {
    let server = MockServer::start().await;
    let body = sse(&[
        ev(json!({"type": "response.output_text.delta", "delta": "Hi"})),
        ev(
            json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "The model crashed"}}}),
        ),
    ]);
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &format!("{}/v1", server.uri()));
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
    let msg = events[1].as_ref().unwrap_err().to_string();
    assert!(msg.contains("server_error: The model crashed"), "{msg}");
}

#[tokio::test]
async fn retries_503_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error": {"message": "busy", "type": "server_error"}})),
        )
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response(sse(&[
            ev(json!({"type": "response.output_text.delta", "delta": "ok"})),
            ev(json!({"type": "response.completed", "response": {}})),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &format!("{}/v1", server.uri()));
    let r = ChatResponse::collect(
        p.chat(ChatRequest::new(vec![Message::user_text("x")]))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(r.text(), "ok");
}

#[tokio::test]
async fn rate_limit_with_long_retry_after_fails_fast() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "3600")
                .set_body_json(json!({"error": {"message": "Rate limit reached", "type": "requests", "code": "rate_limit_exceeded"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(CFG, &format!("{}/v1", server.uri()));
    let err = p
        .chat(ChatRequest::new(vec![Message::user_text("x")]))
        .await
        .err()
        .unwrap();
    // Not retried (the hint exceeds max_retry_delay); the hint is passed on to the caller.
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(3_600_000)
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn embeddings_unsupported() {
    let (p, _) = provider(CFG, "http://127.0.0.1:9/v1");
    assert!(matches!(
        p.embed(&["x".into()]).await,
        Err(Error::Unsupported(_))
    ));
}
