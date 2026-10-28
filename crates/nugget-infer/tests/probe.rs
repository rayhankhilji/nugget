//! Probe logic against mocked servers. The "smart" mocks really decode the PNG sent for the
//! vision check, so a passing vision probe proves the image round-trips through the adapter.

mod common;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::*;
use nugget_core::capability::Tier;
use nugget_core::ids::ProviderId;
use nugget_infer::probe::vision::read_code_png;
use nugget_infer::{ProbeOptions, Router, probe, probe_with};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// What a capable model would answer, decided from the request.
enum Answer {
    Text(String),
    Tool { name: String, args: Value },
}

fn quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let end = start + s[start..].find('"')?;
    Some(s[start..end].to_string())
}

fn decode_data_url(url: &str) -> Vec<u8> {
    let b64 = url.split_once(";base64,").unwrap().1;
    B64.decode(b64).unwrap()
}

/// Answer for an OpenAI-compatible chat request.
fn answer_openai(body: &Value) -> Answer {
    let content = &body["messages"].as_array().unwrap().last().unwrap()["content"];
    if let Some(parts) = content.as_array()
        && let Some(img) = parts.iter().find(|p| p["type"] == "image_url")
    {
        let png = decode_data_url(img["image_url"]["url"].as_str().unwrap());
        return Answer::Text(format!(
            "The number is {}.",
            read_code_png(&png, 4).unwrap()
        ));
    }
    let text = content.as_str().unwrap_or_default();
    if body.get("tools").is_some() {
        return Answer::Tool {
            name: body["tools"][0]["function"]["name"]
                .as_str()
                .unwrap()
                .into(),
            args: json!({"value": quoted(text).unwrap()}),
        };
    }
    if body.get("response_format").is_some() {
        return Answer::Text(r#"{"answer": 7, "unit": "apples"}"#.into());
    }
    Answer::Text("pong".into())
}

/// Answer for an Anthropic Messages request.
fn answer_anthropic(body: &Value) -> Answer {
    let content = body["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    if let Some(img) = content.iter().find(|b| b["type"] == "image") {
        let png = B64.decode(img["source"]["data"].as_str().unwrap()).unwrap();
        return Answer::Text(read_code_png(&png, 4).unwrap());
    }
    let text = content
        .iter()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default();
    if body.get("tools").is_some() {
        return Answer::Tool {
            name: body["tools"][0]["name"].as_str().unwrap().into(),
            args: json!({"value": quoted(text).unwrap()}),
        };
    }
    if body.get("output_config").is_some() {
        return Answer::Text(r#"{"answer":7,"unit":"apples"}"#.into());
    }
    Answer::Text("pong".into())
}

struct SmartOpenAi;

impl Respond for SmartOpenAi {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        if req.url.path().ends_with("/embeddings") {
            let n = body["input"].as_array().unwrap().len();
            let data: Vec<Value> = (0..n)
                .map(|i| json!({"index": i, "embedding": [0.1 * (i as f32 + 1.0), 0.2, 0.3]}))
                .collect();
            return ResponseTemplate::new(200).set_body_json(json!({"data": data}));
        }
        let chunks = match answer_openai(&body) {
            Answer::Text(t) => {
                let (a, b) = t.split_at(t.len() / 2);
                vec![
                    json!({"choices": [{"index": 0, "delta": {"content": a}}]}).to_string(),
                    json!({"choices": [{"index": 0, "delta": {"content": b}, "finish_reason": "stop"}]}).to_string(),
                ]
            }
            Answer::Tool { name, args } => vec![
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}}]}).to_string(),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}).to_string(),
            ],
        };
        let mut datas: Vec<&str> = chunks.iter().map(String::as_str).collect();
        datas.push("[DONE]");
        sse_response(sse_raw(&datas))
    }
}

struct SmartAnthropic;

impl Respond for SmartAnthropic {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let (block_start, delta, stop) = match answer_anthropic(&body) {
            Answer::Text(t) => (
                json!({"type": "text", "text": ""}),
                json!({"type": "text_delta", "text": t}),
                "end_turn",
            ),
            Answer::Tool { name, args } => (
                json!({"type": "tool_use", "id": "toolu_1", "name": name, "input": {}}),
                json!({"type": "input_json_delta", "partial_json": args.to_string()}),
                "tool_use",
            ),
        };
        sse_response(sse(&[
            (
                None,
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
            ),
            (
                None,
                json!({"type": "content_block_start", "index": 0, "content_block": block_start}),
            ),
            (
                None,
                json!({"type": "content_block_delta", "index": 0, "delta": delta}),
            ),
            (None, json!({"type": "content_block_stop", "index": 0})),
            (
                None,
                json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 3}}),
            ),
            (None, json!({"type": "message_stop"})),
        ]))
    }
}

#[tokio::test]
async fn capable_openai_compatible_server_passes_every_check() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SmartOpenAi)
        .mount(&server)
        .await;
    let (p, _) = provider(
        // Declared cloud so the checks run concurrently.
        "id = \"smart\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\ncontext_tokens = 8192\nlocality = \"cloud\"",
        &format!("{}/v1", server.uri()),
    );
    let report = probe_with(p.as_ref(), &ProbeOptions::default()).await;
    for c in &report.checks {
        assert!(c.passed, "{} failed: {}", c.name, c.detail);
    }
    let caps = report.capabilities;
    assert!(caps.streaming && caps.vision && caps.tools && caps.json_schema && caps.embeddings);
    assert_eq!(caps.context_tokens, Some(8192));
    // Four chat checks + one embeddings call.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 5);
    assert_eq!(
        reqs.iter()
            .filter(|r| r.url.path().ends_with("/embeddings"))
            .count(),
        1
    );
}

#[tokio::test]
async fn capable_anthropic_server_passes_chat_checks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(SmartAnthropic)
        .mount(&server)
        .await;
    let (p, _) = provider(
        "id = \"claude\"\nkind = \"anthropic\"\nmodel = \"test-model\"\napi_key = \"env:K\"",
        &server.uri(),
    );
    let caps = probe(p.as_ref()).await;
    assert!(caps.streaming && caps.vision && caps.tools && caps.json_schema);
    assert!(!caps.embeddings, "Anthropic has no embeddings endpoint");
}

/// Streams the same unhelpful text for everything; has no embeddings endpoint.
struct Dumb;

impl Respond for Dumb {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        if req.url.path().ends_with("/embeddings") {
            return ResponseTemplate::new(404).set_body_json(json!({"error": "not found"}));
        }
        sse_response(sse_raw(&[
            r#"{"choices":[{"index":0,"delta":{"content":"I see 12 things"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]))
    }
}

#[tokio::test]
async fn incapable_server_only_proves_streaming() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Dumb)
        .mount(&server)
        .await;
    let (p, _) = provider(
        "id = \"dumb\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"",
        &format!("{}/v1", server.uri()),
    );
    let report = probe_with(p.as_ref(), &ProbeOptions::default()).await;
    let caps = &report.capabilities;
    assert!(caps.streaming);
    assert!(!caps.vision && !caps.tools && !caps.json_schema && !caps.embeddings);
    let detail = |n: &str| {
        report
            .checks
            .iter()
            .find(|c| c.name == n)
            .unwrap()
            .detail
            .clone()
    };
    assert!(
        detail("vision").starts_with("expected "),
        "{}",
        detail("vision")
    );
    assert!(detail("tools").contains("no tool call"));
    assert!(detail("json_schema").contains("not JSON"));
    assert!(detail("embeddings").contains("404"));
}

/// Never answers in time.
struct Slow;

impl Respond for Slow {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_delay(Duration::from_secs(5))
    }
}

#[tokio::test]
async fn unreachable_or_slow_provider_proves_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Slow)
        .mount(&server)
        .await;
    let (p, _) = provider(
        "id = \"slow\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"",
        &format!("{}/v1", server.uri()),
    );
    let opts = ProbeOptions {
        timeout: Duration::from_millis(200),
        check_embeddings: false,
        ..ProbeOptions::default()
    };
    let report = probe_with(p.as_ref(), &opts).await;
    assert_eq!(report.capabilities, Default::default());
    assert!(report.checks.iter().all(|c| !c.passed));
    assert!(report.checks[0].detail.contains("timed out"));
    assert_eq!(report.checks[4].detail, "skipped");
}

#[tokio::test]
async fn router_probe_records_capabilities() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SmartOpenAi)
        .mount(&server)
        .await;
    let (p, _) = provider(
        "id = \"smart\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"",
        &format!("{}/v1", server.uri()),
    );
    let id = ProviderId::new("smart");
    let router = Router::new(
        vec![p],
        [(Tier::Embedding, vec![id.clone()])].into_iter().collect(),
    );
    assert!(router.embed(&["x".into()]).await.is_err());
    router.probe(&id, &ProbeOptions::default()).await.unwrap();
    assert!(router.capabilities(&id).unwrap().embeddings);
    let (used, v) = router.embed(&["x".into()]).await.unwrap();
    assert_eq!((used, v.len()), (id, 1));
}
