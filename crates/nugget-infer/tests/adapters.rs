//! The three wire adapters against mock servers: request shape, stream decoding, HTTP errors and
//! retries.

mod common;

use common::*;
use nugget_core::Error;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_infer::{ChatEvent, Message, Part, ProviderKind, StopReason, ToolSpec};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ANTHROPIC: &str = "id = \"claude\"\nkind = \"anthropic\"\nmodel = \"test-model\"\napi_key = \"env:ANTHROPIC_API_KEY\"";
const RESPONSES: &str = "id = \"oai\"\nkind = \"openai_responses\"\nmodel = \"test-model\"\napi_key = \"env:OPENAI_API_KEY\"";
const LOCAL: &str = "id = \"ollama\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"";
const KEYED: &str = "id = \"router\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\napi_key = \"env:OPENROUTER_API_KEY\"";

fn anthropic_text() -> String {
    sse_raw(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":12,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"ping"}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#,
        r#"{"type":"message_stop"}"#,
    ])
}

fn compat_ok() -> String {
    sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ])
}

fn lookup_tool(schema: Value) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "lookup".into(),
        description: "weather".into(),
        input_schema: schema,
    }]
}

#[tokio::test]
async fn anthropic_streams_text_tools_and_thinking() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2026-10-21"))
        .and(header("accept", "text/event-stream"))
        .and(body_partial_json(
            json!({"model": "test-model", "stream": true, "max_tokens": 16000}),
        ))
        .respond_with(sse_response(anthropic_text()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tools = sse_raw(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":50}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"lookup","input":{}}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Ber"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"lin\", \"days\": 3}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    respond(&server, sse_response(tools)).await;
    let (p, sink) = provider(ANTHROPIC, &server.uri());
    assert_eq!(p.meta().kind, ProviderKind::Anthropic);
    assert_eq!(p.meta().locality, Locality::OnDevice);

    let r = collect(p.as_ref(), user("hi").with_system("be nice"))
        .await
        .unwrap();
    assert_eq!(r.text(), "Hello world");
    assert_eq!(
        (r.stop_reason, r.input_tokens, r.output_tokens),
        (StopReason::EndTurn, Some(12), Some(6))
    );
    assert!(
        sink.events().is_empty(),
        "loopback: on-device, nothing recorded"
    );
    let body = sent_body(&server, 0).await;
    assert_eq!(body["system"], "be nice");
    assert_eq!(
        body["messages"][0],
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})
    );

    let mut req = nugget_infer::ChatRequest::new(vec![Message::user(vec![
        Part::image("image/png", "iVBORw0KGgo="),
        Part::text("weather?"),
    ])])
    .with_tools(lookup_tool(json!({"type": "object"})))
    .with_json_schema(json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}));
    req.temperature = Some(0.5);
    let r = collect(p.as_ref(), req).await.unwrap();
    assert_eq!(r.stop_reason, StopReason::ToolUse);
    let calls = r.tool_calls();
    assert_eq!((calls.len(), calls[0].id.as_str()), (1, "toolu_01"));
    assert_eq!(calls[0].arguments, json!({"city": "Berlin", "days": 3}));
    let sent = sent_body(&server, 1).await;
    assert_eq!(
        sent["messages"][0]["content"][0],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}})
    );
    assert_eq!(sent["tools"][0]["eager_input_streaming"], true);
    assert_eq!(sent["output_config"]["format"]["type"], "json_schema");
    assert!(sent.get("temperature").is_none(), "rejected on 2026-10-21");
    // The thinking block is kept for verbatim replay.
    let msg = r.into_message();
    assert!(matches!(&msg.content[0], Part::Opaque { data, .. } if data["signature"] == "sig=="));
}

#[tokio::test]
async fn openai_compatible_streams_text_and_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model": "test-model", "stream": true, "stream_options": {"include_usage": true}, "messages": [{"role": "user", "content": "hi"}]})))
        .respond_with(sse_response(sse_raw(&[
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}"#,
            "[DONE]",
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(sse_response(sse_raw(&[
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":""}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":\"rust"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":" sse\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#,
            "[DONE]",
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let (local, sink) = provider(LOCAL, &format!("{}/v1", server.uri()));
    assert_eq!(local.meta().locality, Locality::OnDevice);
    let r = collect(local.as_ref(), user("hi")).await.unwrap();
    assert_eq!(r.text(), "Hello");
    assert_eq!(
        (r.stop_reason, r.input_tokens, r.output_tokens),
        (StopReason::EndTurn, Some(9), Some(2))
    );
    assert!(
        server.received_requests().await.unwrap()[0]
            .headers
            .get("authorization")
            .is_none()
    );
    assert!(sink.events().is_empty());

    let (keyed, _) = provider(KEYED, &format!("{}/api/v1", server.uri()));
    let req = user("find it").with_tools(lookup_tool(json!({"type": "object"})));
    let mut events: Vec<ChatEvent> = chat_events(keyed.as_ref(), req)
        .await
        .unwrap()
        .into_iter()
        .map(Result::unwrap)
        .collect();
    // Built-in adapters send usage detail and timings just before `Done`.
    let ChatEvent::Stats(stats) = events.remove(events.len() - 2) else {
        panic!("{events:?}")
    };
    assert_eq!(
        (stats.input_tokens, stats.output_tokens),
        (Some(5), Some(7))
    );
    assert!(stats.timings.is_some());
    let done = ChatEvent::Done {
        stop_reason: StopReason::ToolUse,
        input_tokens: Some(5),
        output_tokens: Some(7),
    };
    let call = ChatEvent::ToolCall {
        id: "call_1".into(),
        name: "lookup".into(),
        arguments: json!({"q": "rust sse"}),
    };
    assert_eq!(events, vec![call, done]);
    assert_eq!(
        sent_body(&server, 1).await["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
}

#[tokio::test]
async fn openai_responses_streams_and_replays_reasoning_and_tool_calls() {
    let server = MockServer::start().await;
    let body = sse_raw(&[
        r#"{"type":"response.created","response":{"id":"resp_1"}}"#,
        r#"{"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"gAAA"}}"#,
        r#"{"type":"response.output_text.delta","delta":"Checking"}"#,
        r#"{"type":"response.output_text.delta","delta":" now."}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"city\":"}"#,
        r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"lookup","arguments":"{\"city\":\"Rome\"}"}}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":40,"output_tokens":25}}}"#,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_partial_json(json!({"model": "test-model", "stream": true, "store": false, "include": ["reasoning.encrypted_content"]})))
        .respond_with(sse_response(body))
        .expect(2)
        .mount(&server)
        .await;
    let (p, _) = provider(RESPONSES, &format!("{}/v1", server.uri()));
    let strict = json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false});
    let req = nugget_infer::ChatRequest::new(vec![Message::user(vec![
        Part::text("weather in Rome?"),
        Part::image("image/webp", "UklGRg=="),
    ])])
    .with_system("You are terse.")
    .with_tools(lookup_tool(strict));
    let r = collect(p.as_ref(), req).await.unwrap();
    assert_eq!(r.text(), "Checking now.");
    assert_eq!(
        (r.stop_reason.clone(), r.input_tokens, r.output_tokens),
        (StopReason::ToolUse, Some(40), Some(25))
    );
    let calls = r.tool_calls();
    assert_eq!(
        (calls[0].id.as_str(), &calls[0].arguments),
        ("call_abc", &json!({"city": "Rome"}))
    );
    assert!(
        matches!(&r.parts[0], Part::Opaque { kind: ProviderKind::OpenaiResponses, data } if data["encrypted_content"] == "gAAA")
    );
    let sent = sent_body(&server, 0).await;
    assert_eq!(sent["instructions"], "You are terse.");
    assert_eq!(
        sent["input"][0]["content"][1]["image_url"],
        "data:image/webp;base64,UklGRg=="
    );
    assert_eq!(sent["tools"][0]["strict"], true);

    // The follow-up turn replays reasoning and the call, and sends the tool output.
    let history = vec![
        Message::user_text("weather in Rome?"),
        r.into_message(),
        Message::user(vec![Part::tool_result("call_abc", "sunny")]),
    ];
    collect(p.as_ref(), nugget_infer::ChatRequest::new(history))
        .await
        .unwrap();
    let input = sent_body(&server, 1).await["input"].clone();
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

/// Retries, rate limits and error mapping (shared HTTP layer).
#[tokio::test]
async fn http_errors_retries_and_rate_limits() {
    let anthropic_err =
        |t: &str, m: &str| json!({"type": "error", "error": {"type": t, "message": m}});
    // (config, failing response, times it fails, max_retries, requests expected, outcome check)
    type Check = fn(&std::result::Result<String, Error>) -> bool;
    let cases: Vec<(&str, ResponseTemplate, u64, u32, usize, Check)> = vec![
        // Transient statuses are retried, then the request succeeds.
        (LOCAL, ResponseTemplate::new(502), 1, 2, 2, |r| {
            matches!(r.as_deref(), Ok("ok"))
        }),
        (
            RESPONSES,
            ResponseTemplate::new(503).set_body_json(json!({"error": {"message": "busy"}})),
            2,
            2,
            3,
            |r| r.is_ok(),
        ),
        // Rate limits surface as RateLimited once retries run out (529 = Anthropic overloaded).
        (
            LOCAL,
            ResponseTemplate::new(429).set_body_json(json!({"error": "too many requests"})),
            9,
            2,
            3,
            |r| {
                matches!(
                    r,
                    Err(Error::RateLimited {
                        retry_after_ms: None
                    })
                )
            },
        ),
        (
            ANTHROPIC,
            ResponseTemplate::new(529)
                .set_body_json(anthropic_err("overloaded_error", "Overloaded")),
            9,
            1,
            2,
            |r| {
                matches!(
                    r,
                    Err(Error::RateLimited {
                        retry_after_ms: None
                    })
                )
            },
        ),
        // A retry-after beyond the maximum delay fails fast and passes the hint on.
        (
            RESPONSES,
            ResponseTemplate::new(429).insert_header("retry-after", "3600"),
            9,
            2,
            1,
            |r| {
                matches!(
                    r,
                    Err(Error::RateLimited {
                        retry_after_ms: Some(3_600_000)
                    })
                )
            },
        ),
        (
            LOCAL,
            ResponseTemplate::new(500).set_body_string("internal"),
            9,
            0,
            1,
            |r| matches!(r, Err(Error::Provider(m)) if m.contains("HTTP 500") && m.contains("internal")),
        ),
        // Auth errors are not retried, carry a hint, and never echo the key.
        (
            ANTHROPIC,
            ResponseTemplate::new(401)
                .set_body_json(anthropic_err("authentication_error", "invalid x-api-key")),
            9,
            2,
            1,
            |r| matches!(r, Err(Error::Provider(m)) if m.contains("authentication_error") && m.contains("api_key") && !m.contains("test-key")),
        ),
    ];
    for (i, (cfg, fail, times, retries, want_requests, check)) in cases.into_iter().enumerate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(fail)
            .up_to_n_times(times)
            .mount(&server)
            .await;
        let ok = if cfg == ANTHROPIC {
            anthropic_text()
        } else if cfg == RESPONSES {
            sse_raw(&[
                r#"{"type":"response.output_text.delta","delta":"ok"}"#,
                r#"{"type":"response.completed","response":{}}"#,
            ])
        } else {
            compat_ok()
        };
        respond(&server, sse_response(ok)).await;
        let base = if cfg == ANTHROPIC {
            server.uri()
        } else {
            format!("{}/v1", server.uri())
        };
        let (p, _) = provider(&format!("{cfg}\nmax_retries = {retries}"), &base);
        let got = collect(p.as_ref(), user("x")).await.map(|r| r.text());
        assert!(check(&got), "case {i}: {got:?}");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            want_requests,
            "case {i}"
        );
    }
}

#[tokio::test]
async fn retried_attempts_are_each_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("slow down"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    respond(&server, sse_response(anthropic_text())).await;
    // A localhost gateway declared as cloud: egress must be recorded.
    let (p, sink) = provider(&format!("{ANTHROPIC}\nlocality = \"cloud\""), &server.uri());
    assert_eq!(
        collect(p.as_ref(), user("hi")).await.unwrap().text(),
        "Hello world"
    );
    let events = sink.events();
    assert_eq!(events.len(), 2, "one ledger entry per attempt");
    for e in &events {
        assert_eq!(e.feature, "chat");
        assert!(e.host.starts_with("127.0.0.1:") && e.bytes_sent > 0 && e.bytes_received > 0);
    }
    assert_eq!(events[1].bytes_received as usize, anthropic_text().len());
}

/// Streams that fail after starting end with exactly one error, after the content before it.
#[tokio::test]
async fn broken_streams_end_with_an_error() {
    let cases = [
        (
            ANTHROPIC,
            sse(&[
                (
                    None,
                    json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "partial"}}),
                ),
                (
                    Some("error"),
                    json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
                ),
            ]),
            "overloaded_error: Overloaded",
        ),
        (
            LOCAL,
            sse_raw(&[
                r#"{"choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
                r#"{"error":{"message":"Provider returned error","code":502}}"#,
            ]),
            "502: Provider returned error",
        ),
        (
            RESPONSES,
            sse_raw(&[
                r#"{"type":"response.output_text.delta","delta":"partial"}"#,
                r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"The model crashed"}}}"#,
            ]),
            "server_error: The model crashed",
        ),
        (
            ANTHROPIC,
            sse_raw(&[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"partial"}}"#,
            ]),
            "ended before completion",
        ),
    ];
    for (cfg, body, want) in cases {
        let server = MockServer::start().await;
        respond(&server, sse_response(body)).await;
        let base = if cfg == ANTHROPIC {
            server.uri()
        } else {
            format!("{}/v1", server.uri())
        };
        let (p, _) = provider(cfg, &base);
        let events = chat_events(p.as_ref(), user("x")).await.unwrap();
        assert_eq!(events.len(), 2, "{want}");
        assert_eq!(
            events[0].as_ref().unwrap(),
            &ChatEvent::TextDelta("partial".into())
        );
        let err = events[1].as_ref().unwrap_err().to_string();
        assert!(err.contains(want), "{err}");
    }
    // A JSON body where a stream was expected.
    let server = MockServer::start().await;
    respond(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"type": "message", "content": []})),
    )
    .await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let events = chat_events(p.as_ref(), user("x")).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("expected an event stream")
    );
}

/// Echoes one deterministic vector per input, in reverse order with explicit indexes.
struct EmbedResponder;

impl Respond for EmbedResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let mut data: Vec<Value> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"index": i, "embedding": [t.as_str().unwrap().len() as f32, 1.0]}))
            .collect();
        data.reverse();
        ResponseTemplate::new(200).set_body_json(json!({"data": data}))
    }
}

#[tokio::test]
async fn embeddings_batch_keep_order_and_list_items() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(
            json!({"model": "test-model", "encoding_format": "float"}),
        ))
        .respond_with(EmbedResponder)
        .mount(&server)
        .await;
    let (p, sink) = provider(
        &format!("{LOCAL}\nembed_batch_size = 2\nlocality = \"local_network\""),
        &format!("{}/v1", server.uri()),
    );
    let texts: Vec<String> = ["a", "bb", "ccc"].iter().map(|s| s.to_string()).collect();
    let v = p.embed(&texts).await.unwrap();
    assert_eq!(v, vec![vec![1.0, 1.0], vec![2.0, 1.0], vec![3.0, 1.0]]);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "batched"
    );
    assert!(p.embed(&[]).await.unwrap().is_empty());

    // GG8-6: each request's ledger row lists the items its texts came from.
    let item = |k: &str| ItemRef {
        source: SourceId::new("notes"),
        key: ItemKey::new(k),
    };
    p.embed_items(&texts, &[item("a"), item("a"), item("b")])
        .await
        .unwrap();
    let rows: Vec<_> = sink.events().into_iter().map(|e| e.items).collect();
    assert_eq!(rows[2..], [vec![item("a")], vec![item("b")]]);

    // Only openai_compatible has embeddings.
    for cfg in [ANTHROPIC, RESPONSES] {
        let (p, _) = provider(cfg, "http://127.0.0.1:9/v1");
        assert!(matches!(
            p.embed(&["x".into()]).await,
            Err(Error::Unsupported(_))
        ));
    }
}

#[tokio::test]
async fn embeddings_report_task_prefixes_and_send_texts_as_given() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(EmbedResponder)
        .mount(&server)
        .await;
    let texts = vec!["cats".to_string(), "dogs".to_string()];
    let pair = |q: &str, d: &str| (q.to_owned(), d.to_owned());
    for (model, extra, want) in [
        (
            "nomic-embed-text-v1.5-q8",
            "",
            pair("search_query: ", "search_document: "),
        ),
        // An empty prefix disables it; the other keeps its default.
        (
            "nomic-embed-text-v1.5-q8",
            "embed_query_prefix = \"\"",
            pair("", "search_document: "),
        ),
        ("test-model", "", pair("", "")),
    ] {
        let toml =
            format!("id = \"e\"\nkind = \"openai_compatible\"\nmodel = \"{model}\"\n{extra}");
        let (p, _) = provider(&toml, &format!("{}/v1", server.uri()));
        p.embed(&texts).await.unwrap();
        p.embed_query(&texts).await.unwrap();
        assert_eq!(p.embed_prefixes(), want, "{model} {extra}");
    }
    // Callers add the prefixes: texts go out as given.
    for r in server.received_requests().await.unwrap() {
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["input"], json!(["cats", "dogs"]));
    }
}
