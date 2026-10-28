//! M3 latency features: prompt-cache markers on the wire, prewarm, timings and the router metric
//! hook. The local "llama-server" here is a wiremock stand-in: the repo's fake-llama-server
//! binary lives in nugget-runtime (which depends on this crate), so it is not reachable from
//! these tests.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_infer::{
    CacheHint, CacheTtl, ChatEvent, ChatMetric, ChatOutcome, ChatRequest, ChatResponse, ChatStats,
    FEATURE_PREWARM, Message, Part, Provider, Role, Router, StopReason, TierMap, ToolSpec,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ANTHROPIC: &str = "id = \"claude\"\nkind = \"anthropic\"\nmodel = \"test-model\"\napi_key = \"env:ANTHROPIC_API_KEY\"\nlocality = \"cloud\"";
const RESPONSES: &str = "id = \"oai\"\nkind = \"openai_responses\"\nmodel = \"test-model\"\napi_key = \"env:OPENAI_API_KEY\"\nlocality = \"cloud\"";
const LLAMA: &str = "id = \"local\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"";

const SECRET_CONTEXT: &str = "the user's private thread text";

fn item() -> ItemRef {
    ItemRef {
        source: SourceId::new("mail:me"),
        key: ItemKey::new("thread-1"),
    }
}

/// A draft-shaped request: stable system prompt (voice profile), exemplars up to a breakpoint,
/// then the per-request context.
fn draft_request(hint: CacheHint) -> ChatRequest {
    let mut req = ChatRequest::new(vec![Message::user(vec![
        Part::text("exemplar one"),
        Part::text("exemplar two"),
        Part::CacheBreakpoint,
        Part::text(SECRET_CONTEXT),
    ])])
    .with_system("You write in the user's voice.")
    .with_tools(vec![ToolSpec {
        name: "lookup".into(),
        description: "look something up".into(),
        input_schema: json!({"type": "object"}),
    }])
    .with_feature("draft")
    .with_cache(hint);
    req.items = vec![item()];
    req
}

fn body(r: &Request) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

fn anthropic_stream(cache_read: u64, cache_write: u64) -> String {
    sse(&[
        (
            Some("message_start"),
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 3, "cache_read_input_tokens": cache_read, "cache_creation_input_tokens": cache_write, "output_tokens": 1}}}),
        ),
        (
            Some("content_block_start"),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            Some("content_block_delta"),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}),
        ),
        (
            Some("content_block_stop"),
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            Some("message_delta"),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
        ),
        (Some("message_stop"), json!({"type": "message_stop"})),
    ])
}

fn llama_stream() -> String {
    sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"timings":{"cache_n":40,"prompt_n":2}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":40}}}"#,
        "[DONE]",
    ])
}

fn stats_of(events: &[ChatEvent]) -> ChatStats {
    let n = events.len();
    assert!(n >= 2, "{events:?}");
    assert!(
        matches!(events[n - 1], ChatEvent::Done { .. }),
        "{events:?}"
    );
    match &events[n - 2] {
        ChatEvent::Stats(s) => s.clone(),
        other => panic!("expected Stats before Done, got {other:?}"),
    }
}

async fn events_of(p: &dyn Provider, req: ChatRequest) -> Vec<ChatEvent> {
    collect_all(p.chat(req).await.unwrap())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn anthropic_cache_control_placement_and_cache_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(anthropic_stream(100, 20)))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let hint = CacheHint {
        ttl: CacheTtl::OneHour,
        ..CacheHint::default()
    };
    let events = events_of(p.as_ref(), draft_request(hint)).await;
    let stats = stats_of(&events);
    assert_eq!(stats.cached_input_tokens, Some(100));
    assert_eq!(stats.cache_write_tokens, Some(20));
    assert_eq!(
        stats.input_tokens,
        Some(123),
        "input_tokens counts cached tokens too"
    );

    let b = body(&server.received_requests().await.unwrap()[0]);
    let cc = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(
        b["system"],
        json!([{"type": "text", "text": "You write in the user's voice.", "cache_control": cc}])
    );
    // A system prompt is present, so the tools (rendered before it) are covered by its marker.
    assert!(b["tools"][0].get("cache_control").is_none());
    let content = b["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        content.len(),
        3,
        "the marker itself is not sent: {content:?}"
    );
    assert!(content[0].get("cache_control").is_none());
    assert_eq!(content[1]["text"], "exemplar two");
    assert_eq!(content[1]["cache_control"], cc);
    assert!(content[2].get("cache_control").is_none());
    assert_eq!(content[2]["text"], SECRET_CONTEXT);
}

#[tokio::test]
async fn anthropic_without_hint_sends_plain_system_and_only_message_breakpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(anthropic_stream(0, 0)))
        .mount(&server)
        .await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let mut req = draft_request(CacheHint::default());
    req.cache = None;
    req.system = None;
    events_of(p.as_ref(), req).await;
    let b = body(&server.received_requests().await.unwrap()[0]);
    assert!(b.get("system").is_none());
    assert!(b["tools"][0].get("cache_control").is_none());
    assert_eq!(
        b["messages"][0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );

    // A hint without a system prompt marks the last tool instead.
    let mut req = draft_request(CacheHint::default());
    req.system = None;
    events_of(p.as_ref(), req).await;
    let b = body(&server.received_requests().await.unwrap()[1]);
    assert_eq!(b["tools"][0]["cache_control"], json!({"type": "ephemeral"}));
}

#[tokio::test]
async fn anthropic_prewarm_prefix_is_prefill_only_and_metered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "message", "content": [], "stop_reason": "max_tokens",
            "usage": {"input_tokens": 1, "cache_creation_input_tokens": 600, "output_tokens": 0}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (p, sink) = provider(ANTHROPIC, &server.uri());
    p.prewarm_prefix(&draft_request(CacheHint::default()))
        .await
        .unwrap();
    let req = &server.received_requests().await.unwrap()[0];
    let raw = String::from_utf8_lossy(&req.body);
    assert!(
        !raw.contains(SECRET_CONTEXT),
        "only the stable prefix: {raw}"
    );
    let b = body(req);
    assert_eq!(b["max_tokens"], 0);
    assert_eq!(b["stream"], false);
    assert_eq!(
        b["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
    assert_eq!(
        msgs[0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].feature, "draft",
        "the prefix is the draft's content"
    );
    assert_eq!(events[0].items, vec![item()]);
}

#[tokio::test]
async fn anthropic_prewarm_prefix_without_cache_markers_only_warms_the_connection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let mut req = draft_request(CacheHint::default());
    req.cache = None;
    req.messages = vec![Message::user_text(SECRET_CONTEXT)];
    p.prewarm_prefix(&req).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method.as_str(), "GET");
}

#[tokio::test]
async fn openai_responses_sets_prompt_cache_key_and_reports_cached_tokens() {
    let server = MockServer::start().await;
    let stream = sse(&[
        (
            None,
            json!({"type": "response.output_text.delta", "delta": "Hi"}),
        ),
        (
            None,
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 50, "input_tokens_details": {"cached_tokens": 48}, "output_tokens": 1}}}),
        ),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sse_response(stream))
        .mount(&server)
        .await;
    let (p, _) = provider(RESPONSES, &format!("{}/v1", server.uri()));
    let events = events_of(p.as_ref(), draft_request(CacheHint::keyed("draft:email"))).await;
    assert_eq!(stats_of(&events).cached_input_tokens, Some(48));
    events_of(p.as_ref(), draft_request(CacheHint::default())).await;
    let mut plain = draft_request(CacheHint::default());
    plain.cache = None;
    events_of(p.as_ref(), plain).await;

    let reqs = server.received_requests().await.unwrap();
    let b0 = body(&reqs[0]);
    assert_eq!(b0["prompt_cache_key"], "draft:email");
    // Instructions and tools lead; the breakpoint marker is not sent.
    assert_eq!(b0["instructions"], "You write in the user's voice.");
    assert_eq!(b0["input"][0]["content"].as_array().unwrap().len(), 3);
    assert_eq!(body(&reqs[1])["prompt_cache_key"], "nugget:draft");
    assert!(body(&reqs[2]).get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn openai_compatible_llama_cache_prompt_and_slot_affinity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_response(llama_stream()))
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());

    // On-device default: llama-server extensions on, no slot pinning.
    let (local, _) = provider(LLAMA, &base);
    let events = events_of(local.as_ref(), draft_request(CacheHint::keyed("draft"))).await;
    assert_eq!(stats_of(&events).cached_input_tokens, Some(40));
    // Slot affinity: the same key always maps to the same slot.
    let (pinned, _) = provider(&format!("{LLAMA}\nslot_affinity = true"), &base);
    for _ in 0..2 {
        events_of(pinned.as_ref(), draft_request(CacheHint::keyed("draft"))).await;
    }
    events_of(pinned.as_ref(), draft_request(CacheHint::default())).await;
    // A cloud gateway gets none of it; neither does an explicit opt-out.
    let (cloud, _) = provider(&format!("{LLAMA}\nlocality = \"cloud\""), &base);
    events_of(cloud.as_ref(), draft_request(CacheHint::keyed("draft"))).await;
    let (off, _) = provider(&format!("{LLAMA}\nllama_cpp = false"), &base);
    events_of(off.as_ref(), draft_request(CacheHint::keyed("draft"))).await;

    let reqs = server.received_requests().await.unwrap();
    let b: Vec<Value> = reqs.iter().map(body).collect();
    assert_eq!(b[0]["cache_prompt"], true);
    assert!(b[0].get("id_slot").is_none());
    let slot = b[1]["id_slot"]
        .as_u64()
        .expect("id_slot with slot_affinity");
    assert_eq!(b[2]["id_slot"].as_u64(), Some(slot), "stable per key");
    assert!(b[3].get("id_slot").is_none(), "no key, no pinning");
    for cold in &b[4..6] {
        assert!(cold.get("cache_prompt").is_none(), "{cold}");
        assert!(cold.get("id_slot").is_none(), "{cold}");
    }
    // System, then one user string; the marker adds nothing.
    assert_eq!(
        b[0]["messages"][1]["content"],
        format!("exemplar one\nexemplar two\n{SECRET_CONTEXT}")
    );
}

#[tokio::test]
async fn llama_prewarm_prefix_evaluates_the_prefix_into_the_kv_cache() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": ""}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 30, "completion_tokens": 0}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (p, sink) = provider(
        &format!("{LLAMA}\nslot_affinity = true"),
        &format!("{}/v1", server.uri()),
    );
    let req = draft_request(CacheHint::keyed("draft"));
    p.prewarm_prefix(&req).await.unwrap();
    let r = &server.received_requests().await.unwrap()[0];
    assert!(!String::from_utf8_lossy(&r.body).contains(SECRET_CONTEXT));
    let b = body(r);
    assert_eq!(b["n_predict"], 0);
    assert_eq!(b["max_tokens"], 1);
    assert_eq!(b["stream"], false);
    assert_eq!(b["cache_prompt"], true);
    assert!(b.get("stream_options").is_none());
    assert!(
        b["id_slot"].is_u64(),
        "warm the slot the real request will use"
    );
    assert_eq!(b["messages"][0]["role"], "system");
    assert_eq!(b["messages"][1]["content"], "exemplar one\nexemplar two");
    assert_eq!(b["tools"][0]["function"]["name"], "lookup");
    assert!(
        sink.events().is_empty(),
        "on-device: nothing leaves the machine"
    );
}

/// `prewarm()` sends no content, is authenticated, and is recorded in the egress ledger.
#[tokio::test]
async fn prewarm_sends_no_user_content_and_records_egress() {
    let server = MockServer::start().await;
    for p in ["/v1/models"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m"}]})))
            .mount(&server)
            .await;
    }
    let cases = [
        (ANTHROPIC, server.uri(), "x-api-key"),
        (RESPONSES, format!("{}/v1", server.uri()), "authorization"),
        (
            "id = \"gw\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\napi_key = \"env:GW\"\nlocality = \"cloud\"",
            format!("{}/v1", server.uri()),
            "authorization",
        ),
    ];
    for (i, (toml, base, auth)) in cases.iter().enumerate() {
        let (p, sink) = provider(toml, base);
        p.prewarm().await.unwrap();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), i + 1);
        let r = &reqs[i];
        assert_eq!(r.method.as_str(), "GET");
        assert_eq!(r.url.path(), "/v1/models");
        assert!(r.body.is_empty(), "no content");
        assert!(r.headers.get(*auth).is_some(), "authenticated");
        let events = sink.events();
        assert_eq!(events.len(), 1, "{toml}");
        assert_eq!(events[0].feature, FEATURE_PREWARM);
        assert!(events[0].items.is_empty());
        assert_eq!(events[0].bytes_sent, 0);
        assert!(events[0].bytes_received > 0);
    }

    // On-device: warmed, nothing recorded.
    let (p, sink) = provider(LLAMA, &format!("{}/v1", server.uri()));
    p.prewarm().await.unwrap();
    assert!(sink.events().is_empty());
}

fn check_monotonic(stats: &ChatStats, at_least: Duration) {
    let t = stats.timings.as_ref().expect("timings");
    assert_eq!(t.attempts, 1);
    assert!(t.headers >= at_least, "{t:?}");
    let first_byte = t.first_byte.expect("first byte");
    let first_token = t.first_token.expect("first token");
    assert!(t.headers <= first_byte, "{t:?}");
    assert!(first_byte <= first_token, "{t:?}");
    assert!(first_token <= t.total, "{t:?}");
}

#[tokio::test]
async fn timings_are_present_monotonic_and_show_connection_reuse() {
    let server = MockServer::start().await;
    let delay = Duration::from_millis(120);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(anthropic_stream(0, 0)).set_delay(delay))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let req = || ChatRequest::new(vec![Message::user_text("hi")]);

    let cold = stats_of(&events_of(p.as_ref(), req()).await);
    check_monotonic(&cold, delay);
    assert!(
        cold.timings.as_ref().unwrap().connect.is_some(),
        "first request opens a connection"
    );
    let warm = stats_of(&events_of(p.as_ref(), req()).await);
    check_monotonic(&warm, delay);
    assert_eq!(
        warm.timings.unwrap().connect,
        None,
        "pooled connection reused"
    );

    // A fresh provider has an empty pool; prewarm opens the connection ahead of the chat.
    let (fresh, _) = provider(ANTHROPIC, &server.uri());
    fresh.prewarm().await.unwrap();
    let after = stats_of(&events_of(fresh.as_ref(), req()).await);
    assert_eq!(after.timings.unwrap().connect, None);

    // Collected responses keep the stats.
    let r = ChatResponse::collect(p.chat(req()).await.unwrap())
        .await
        .unwrap();
    assert!(r.stats.and_then(|s| s.timings).is_some());
}

#[derive(Default)]
struct Metrics(Mutex<Vec<ChatMetric>>);

/// Router metric hook plus a small local latency benchmark against a delayed SSE stand-in for
/// llama-server (cached prefix reported by the server).
#[tokio::test]
async fn router_metrics_hook_and_local_latency_microbenchmark() {
    let server = MockServer::start().await;
    let delay = Duration::from_millis(30);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_response(llama_stream()).set_delay(delay))
        .mount(&server)
        .await;
    let (p, _) = provider(LLAMA, &format!("{}/v1", server.uri()));
    let mut tiers = TierMap::new();
    tiers.insert(Tier::Synthesis, vec![p.id().clone()]);
    let metrics = Arc::new(Metrics::default());
    let m = metrics.clone();
    let id = p.id().clone();
    let router = Router::new(vec![p], tiers).with_metrics(Arc::new(move |metric: ChatMetric| {
        m.0.lock().unwrap().push(metric)
    }));
    router.set_capabilities(
        &id,
        Capabilities {
            streaming: true,
            tools: true,
            ..Default::default()
        },
    );

    let runs = 9;
    let mut wall = Vec::new();
    for _ in 0..runs {
        let started = Instant::now();
        let (_, stream) = router
            .chat(
                Tier::Synthesis,
                &Capabilities::default(),
                draft_request(CacheHint::keyed("draft")),
            )
            .await
            .unwrap();
        let r = ChatResponse::collect(stream).await.unwrap();
        assert_eq!(r.text(), "Hello");
        wall.push(started.elapsed());
    }
    let recorded = metrics.0.lock().unwrap().clone();
    assert_eq!(recorded.len(), runs);
    let mut ttft: Vec<Duration> = recorded
        .iter()
        .map(|m| {
            assert_eq!(m.outcome, ChatOutcome::Done(StopReason::EndTurn));
            assert_eq!(m.feature, "draft");
            assert_eq!(m.fallbacks, 0);
            let first_token = m.first_token.unwrap();
            assert!(m.first_event >= delay && first_token >= m.first_event);
            assert!(m.total >= first_token);
            let stats = m.stats.as_ref().unwrap();
            assert_eq!(stats.cached_input_tokens, Some(40));
            check_monotonic(stats, delay);
            first_token
        })
        .collect();
    ttft.sort();
    let p50 = ttft[runs / 2];
    eprintln!(
        "local TTFT p50 {p50:?} (server delay {delay:?}), max {:?}, wall max {:?}",
        ttft[runs - 1],
        wall.iter().max().unwrap()
    );
    // The client adds little on top of the server's own latency (generous for CI).
    assert!(p50 < delay + Duration::from_millis(500), "p50 {p50:?}");

    // A stream dropped before the end (cancelled draft) is still reported.
    let (_, stream) = router
        .chat(
            Tier::Synthesis,
            &Capabilities::default(),
            ChatRequest::new(vec![Message::user_text("hi")]),
        )
        .await
        .unwrap();
    drop(stream);
    let last = metrics.0.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.outcome, ChatOutcome::Dropped);
    assert!(last.stats.is_none());
}

#[tokio::test]
async fn router_prewarm_targets_the_routed_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    let (p, _) = provider(LLAMA, &format!("{}/v1", server.uri()));
    let mut tiers = TierMap::new();
    tiers.insert(Tier::Synthesis, vec![p.id().clone()]);
    let router = Router::new(vec![p.clone()], tiers);
    let warmed = router
        .prewarm(Tier::Synthesis, &Capabilities::default(), None)
        .await
        .unwrap();
    assert_eq!(warmed.id(), p.id());
    // The prefix of a tool-using request needs a tool-capable provider; unprobed ones are not.
    assert!(
        router
            .prewarm(
                Tier::Synthesis,
                &Capabilities::default(),
                Some(&draft_request(CacheHint::default()))
            )
            .await
            .is_err()
    );
    let mut no_tools = draft_request(CacheHint::default());
    no_tools.tools.clear();
    router
        .prewarm(Tier::Synthesis, &Capabilities::default(), Some(&no_tools))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].method.as_str(), "GET");
    assert_eq!(body(&reqs[1])["n_predict"], 0);
}

#[test]
fn cache_prefix_cuts_at_the_last_breakpoint() {
    let req = ChatRequest::new(vec![
        Message::user(vec![Part::text("a"), Part::CacheBreakpoint]),
        Message::assistant_text("b"),
        Message::user(vec![
            Part::text("c"),
            Part::CacheBreakpoint,
            Part::text("d"),
        ]),
        Message::assistant_text("e"),
    ])
    .with_system("s")
    .with_json_schema(json!({"type": "object"}))
    .with_max_tokens(9);
    let p = req.cache_prefix().unwrap();
    assert_eq!(p.messages.len(), 3);
    assert_eq!(
        p.messages[2].content,
        vec![Part::text("c"), Part::CacheBreakpoint]
    );
    assert_eq!(p.messages[2].role, Role::User);
    assert!(p.json_schema.is_none() && p.max_tokens.is_none());

    let system_only = ChatRequest::new(vec![Message::user_text("x")]).with_system("s");
    assert!(system_only.cache_prefix().unwrap().messages.is_empty());
    assert!(
        ChatRequest::new(vec![Message::user_text("x")])
            .cache_prefix()
            .is_none()
    );
    assert_eq!(
        serde_json::to_value(Part::CacheBreakpoint).unwrap(),
        json!({"type": "cache_breakpoint"})
    );
}
