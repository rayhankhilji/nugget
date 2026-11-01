//! Prompt-cache markers on the wire, and prewarm: what it sends and what the egress ledger records.

mod common;

use common::*;
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_infer::{
    CacheHint, CacheTtl, ChatEvent, ChatRequest, ChatStats, FEATURE_PREWARM, Message, Part, Role,
    Router, TierMap, ToolSpec,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// A draft-shaped request: stable system prompt, exemplars up to a breakpoint, then the
/// per-request context.
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

fn llama_stream() -> String {
    sse_raw(&[
        r#"{"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"timings":{"cache_n":40,"prompt_n":2}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":40}}}"#,
        "[DONE]",
    ])
}

/// The [`ChatStats`] sent just before `Done`.
async fn stats(p: &dyn nugget_infer::Provider, req: ChatRequest) -> ChatStats {
    let events: Vec<ChatEvent> = chat_events(p, req)
        .await
        .unwrap()
        .into_iter()
        .map(Result::unwrap)
        .collect();
    match &events[..] {
        [.., ChatEvent::Stats(s), ChatEvent::Done { .. }] => s.clone(),
        other => panic!("expected Stats before Done: {other:?}"),
    }
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    let reqs = server.received_requests().await.unwrap();
    reqs.iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test]
async fn anthropic_cache_control_placement_and_cache_usage() {
    let server = MockServer::start().await;
    respond(&server, sse_response(sse_raw(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":1}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ]))).await;
    let (p, _) = provider(ANTHROPIC, &server.uri());
    let hint = CacheHint {
        ttl: CacheTtl::OneHour,
        ..CacheHint::default()
    };
    let s = stats(p.as_ref(), draft_request(hint)).await;
    assert_eq!(
        (s.cached_input_tokens, s.cache_write_tokens),
        (Some(100), Some(20))
    );
    assert_eq!(
        s.input_tokens,
        Some(123),
        "input_tokens counts cached tokens too"
    );
    // No hint: plain system prompt, message breakpoints only.
    let mut req = draft_request(CacheHint::default());
    req.cache = None;
    stats(p.as_ref(), req).await;
    // A hint without a system prompt marks the last tool instead.
    let mut req = draft_request(CacheHint::default());
    req.system = None;
    stats(p.as_ref(), req).await;

    let b = bodies(&server).await;
    let cc = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(
        b[0]["system"],
        json!([{"type": "text", "text": "You write in the user's voice.", "cache_control": cc}])
    );
    // The system marker covers the tools rendered before it.
    assert!(b[0]["tools"][0].get("cache_control").is_none());
    let content = b[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        content.len(),
        3,
        "the marker itself is not sent: {content:?}"
    );
    let marks: Vec<_> = content.iter().map(|c| c.get("cache_control")).collect();
    assert_eq!(marks, [None, Some(&cc), None]);
    assert_eq!(content[2]["text"], SECRET_CONTEXT);

    assert_eq!(b[1]["system"], "You write in the user's voice.");
    assert!(b[1]["tools"][0].get("cache_control").is_none());
    assert_eq!(
        b[1]["messages"][0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        b[2]["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[tokio::test]
async fn anthropic_prewarm_prefix_is_prefill_only_and_metered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "message", "content": []})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;
    let (p, sink) = provider(ANTHROPIC, &server.uri());
    p.prewarm_prefix(&draft_request(CacheHint::default()))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let raw = String::from_utf8_lossy(&reqs[0].body);
    assert!(
        !raw.contains(SECRET_CONTEXT),
        "only the stable prefix: {raw}"
    );
    let b = bodies(&server).await.remove(0);
    assert_eq!((&b["max_tokens"], &b["stream"]), (&json!(0), &json!(false)));
    let ephemeral = json!({"type": "ephemeral"});
    assert_eq!(b["system"][0]["cache_control"], ephemeral);
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(
        (msgs.len(), msgs[0]["content"].as_array().unwrap().len()),
        (1, 2)
    );
    assert_eq!(msgs[0]["content"][1]["cache_control"], ephemeral);
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].feature, "draft",
        "the prefix is the draft's content"
    );
    assert_eq!(events[0].items, vec![item()]);

    // Nothing marked cacheable: only the connection is warmed.
    let mut req = draft_request(CacheHint::default());
    req.cache = None;
    req.messages = vec![Message::user_text(SECRET_CONTEXT)];
    p.prewarm_prefix(&req).await.unwrap();
    assert_eq!(
        server.received_requests().await.unwrap()[1].method.as_str(),
        "GET"
    );
}

#[tokio::test]
async fn openai_responses_sets_prompt_cache_key_and_reports_cached_tokens() {
    let server = MockServer::start().await;
    respond(&server, sse_response(sse_raw(&[
        r#"{"type":"response.output_text.delta","delta":"Hi"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":50,"input_tokens_details":{"cached_tokens":48},"output_tokens":1}}}"#,
    ]))).await;
    let (p, _) = provider(RESPONSES, &format!("{}/v1", server.uri()));
    let s = stats(p.as_ref(), draft_request(CacheHint::keyed("draft:email"))).await;
    assert_eq!(s.cached_input_tokens, Some(48));
    stats(p.as_ref(), draft_request(CacheHint::default())).await;
    let mut plain = draft_request(CacheHint::default());
    plain.cache = None;
    stats(p.as_ref(), plain).await;

    let b = bodies(&server).await;
    assert_eq!(b[0]["prompt_cache_key"], "draft:email");
    assert_eq!(
        b[0]["input"][0]["content"].as_array().unwrap().len(),
        3,
        "marker not sent"
    );
    assert_eq!(b[1]["prompt_cache_key"], "nugget:draft");
    assert!(b[2].get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn openai_compatible_llama_cache_prompt_and_slot_affinity() {
    let server = MockServer::start().await;
    respond(&server, sse_response(llama_stream())).await;
    let base = format!("{}/v1", server.uri());

    // On-device default: llama-server extensions on, no slot pinning.
    let (local, _) = provider(LLAMA, &base);
    assert_eq!(
        stats(local.as_ref(), draft_request(CacheHint::keyed("draft")))
            .await
            .cached_input_tokens,
        Some(40)
    );
    // Slot affinity: the same key always maps to the same slot.
    let (pinned, _) = provider(&format!("{LLAMA}\nslot_affinity = true"), &base);
    for _ in 0..2 {
        stats(pinned.as_ref(), draft_request(CacheHint::keyed("draft"))).await;
    }
    stats(pinned.as_ref(), draft_request(CacheHint::default())).await;
    // A cloud gateway gets none of it; neither does an explicit opt-out.
    for extra in ["locality = \"cloud\"", "llama_cpp = false"] {
        let (p, _) = provider(&format!("{LLAMA}\n{extra}"), &base);
        stats(p.as_ref(), draft_request(CacheHint::keyed("draft"))).await;
    }

    let b = bodies(&server).await;
    assert_eq!(b[0]["cache_prompt"], true);
    assert!(b[0].get("id_slot").is_none());
    let slot = b[1]["id_slot"]
        .as_u64()
        .expect("id_slot with slot_affinity");
    assert_eq!(b[2]["id_slot"].as_u64(), Some(slot), "stable per key");
    assert!(b[3].get("id_slot").is_none(), "no key, no pinning");
    for cold in &b[4..6] {
        assert!(
            cold.get("cache_prompt").is_none() && cold.get("id_slot").is_none(),
            "{cold}"
        );
    }
    assert_eq!(
        b[0]["messages"][1]["content"],
        format!("exemplar one\nexemplar two\n{SECRET_CONTEXT}")
    );
}

#[tokio::test]
async fn llama_prewarm_prefix_evaluates_the_prefix_into_the_kv_cache() {
    let server = MockServer::start().await;
    respond(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"choices": []})),
    )
    .await;
    let (p, sink) = provider(
        &format!("{LLAMA}\nslot_affinity = true"),
        &format!("{}/v1", server.uri()),
    );
    p.prewarm_prefix(&draft_request(CacheHint::keyed("draft")))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(!String::from_utf8_lossy(&reqs[0].body).contains(SECRET_CONTEXT));
    let b = bodies(&server).await.remove(0);
    for (k, v) in [
        ("n_predict", json!(0)),
        ("max_tokens", json!(1)),
        ("stream", json!(false)),
        ("cache_prompt", json!(true)),
    ] {
        assert_eq!(b[k], v, "{k}");
    }
    assert!(b.get("stream_options").is_none());
    assert!(
        b["id_slot"].is_u64(),
        "warm the slot the real request will use"
    );
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
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m"}]})))
        .mount(&server)
        .await;
    let gateway = "id = \"gw\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\napi_key = \"env:GW\"\nlocality = \"cloud\"";
    let v1 = format!("{}/v1", server.uri());
    let cases = [
        (ANTHROPIC, server.uri(), "x-api-key"),
        (RESPONSES, v1.clone(), "authorization"),
        (gateway, v1.clone(), "authorization"),
    ];
    for (i, (toml, base, auth)) in cases.iter().enumerate() {
        let (p, sink) = provider(toml, base);
        p.prewarm().await.unwrap();
        let reqs = server.received_requests().await.unwrap();
        let r = &reqs[i];
        assert_eq!((r.method.as_str(), r.url.path()), ("GET", "/v1/models"));
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
    let (p, sink) = provider(LLAMA, &v1);
    p.prewarm().await.unwrap();
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn router_prewarm_targets_the_routed_provider() {
    let server = MockServer::start().await;
    respond(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"choices": [], "data": []})),
    )
    .await;
    let (p, _) = provider(LLAMA, &format!("{}/v1", server.uri()));
    let mut tiers = TierMap::new();
    tiers.insert(Tier::Synthesis, vec![p.id().clone()]);
    let router = Router::new(vec![p.clone()], tiers);
    let none = Capabilities::default();
    assert_eq!(
        router
            .prewarm(Tier::Synthesis, &none, None)
            .await
            .unwrap()
            .id(),
        p.id()
    );
    // The prefix of a tool-using request needs a tool-capable provider; unprobed ones are not.
    assert!(
        router
            .prewarm(
                Tier::Synthesis,
                &none,
                Some(&draft_request(CacheHint::default()))
            )
            .await
            .is_err()
    );
    let mut no_tools = draft_request(CacheHint::default());
    no_tools.tools.clear();
    router
        .prewarm(Tier::Synthesis, &none, Some(&no_tools))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].method.as_str(), "GET");
    assert_eq!(sent_body(&server, 1).await["n_predict"], 0);
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
}
