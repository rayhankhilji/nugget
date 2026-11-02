//! The model endpoint against provider test doubles (real HTTP servers that stream with
//! delays and notice disconnects) and a real Postgres (see testkit). Covers auth, checks,
//! metering, quotas, streaming, cancellation, fallback, timeouts, logging, and the desktop's
//! provider end to end with time-to-first-token measured.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::gateway::Gateway;
use crate::plans::Catalog;
use crate::testkit::{FakeAuth, Pg, add_user, subscriber, test_db, test_prices};

// ---- provider doubles ---------------------------------------------------------------------

#[derive(Clone)]
enum Script {
    /// Headers after `wait`, then each chunk after `gap`; `hang` keeps the stream open after.
    Stream {
        wait: Duration,
        chunks: Vec<String>,
        gap: Duration,
        hang: bool,
    },
    Status(u16, Value),
    Json(Value),
}

#[derive(Default)]
struct Double {
    requests: Mutex<Vec<(axum::http::HeaderMap, Value)>>,
    script: Mutex<Option<Script>>,
    /// The response body was dropped before it finished (the caller went away).
    cut_off: AtomicBool,
    hits: AtomicUsize,
}

struct CutGuard(Arc<Double>, bool);
impl Drop for CutGuard {
    fn drop(&mut self) {
        if !self.1 {
            self.0.cut_off.store(true, Ordering::SeqCst);
        }
    }
}

async fn serve_double(d: Arc<Double>, path: &'static str) -> String {
    let handler = move |h: axum::http::HeaderMap, body: axum::body::Bytes| {
        let d = d.clone();
        async move {
            d.hits.fetch_add(1, Ordering::SeqCst);
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            d.requests.lock().unwrap().push((h, v));
            let script = d.script.lock().unwrap().clone().expect("script set");
            match script {
                Script::Status(s, v) => {
                    (StatusCode::from_u16(s).unwrap(), axum::Json(v)).into_response()
                }
                Script::Json(v) => axum::Json(v).into_response(),
                Script::Stream {
                    wait,
                    chunks,
                    gap,
                    hang,
                } => {
                    tokio::time::sleep(wait).await;
                    let guard = CutGuard(d.clone(), false);
                    let s = futures::stream::unfold(
                        (chunks.into_iter(), guard, false),
                        move |(mut it, mut g, first)| async move {
                            if first {
                                tokio::time::sleep(gap).await;
                            }
                            match it.next() {
                                Some(c) => Some((
                                    Ok::<_, std::io::Error>(axum::body::Bytes::from(c)),
                                    (it, g, true),
                                )),
                                None if hang => {
                                    std::future::pending::<()>().await;
                                    None
                                }
                                None => {
                                    g.1 = true;
                                    drop(g);
                                    None
                                }
                            }
                        },
                    );
                    let mut r = Response::new(Body::from_stream(s));
                    r.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    r
                }
            }
        }
    };
    let app = Router::new()
        .route(path, post(handler))
        .route("/v1/models", get(|| async { "{}" }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let l = axum::serve::ListenerExt::tap_io(l, |t| {
        let _ = t.set_nodelay(true);
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// An Anthropic stream: message_start (usage), one text delta per word, message_delta.
fn anthropic_stream(words: &[&str], input: u32, cache_read: u32, output: u32) -> Vec<String> {
    let mut v = vec![
        ev(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant",
              "model": "x", "content": [], "stop_reason": null,
              "usage": {"input_tokens": input, "cache_read_input_tokens": cache_read, "cache_creation_input_tokens": 0, "output_tokens": 1}}}),
        ),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
    ];
    for w in words {
        v.push(ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": w}}),
        ));
    }
    v.push(ev(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    ));
    v.push(ev(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": output}}),
    ));
    v.push(ev("message_stop", json!({"type": "message_stop"})));
    v
}

fn streaming(chunks: Vec<String>) -> Script {
    Script::Stream {
        wait: Duration::ZERO,
        chunks,
        gap: Duration::ZERO,
        hang: false,
    }
}

// ---- harness ------------------------------------------------------------------------------

struct H {
    state: Arc<LlmState>,
    app: Router,
    pool: Pg,
    anthropic: Arc<Double>,
    openai: Arc<Double>,
}

async fn harness_with(openai: bool, limits: Limits) -> Option<H> {
    harness_full(openai, limits, crate::ratelimit::Limits::default()).await
}

async fn harness_full(openai: bool, limits: Limits, rate: crate::ratelimit::Limits) -> Option<H> {
    let (db, pool) = test_db().await?;
    let auth = Arc::new(FakeAuth {
        deleted: Mutex::new(Vec::new()),
    });
    let plans = Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap();
    let gateway = Gateway::new(
        None,
        None,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let app_state = Arc::new(AppState::new(db, auth, gateway, Arc::new(plans)).with_limits(rate));
    let a = Arc::new(Double::default());
    let o = Arc::new(Double::default());
    let a_url = serve_double(a.clone(), "/v1/messages").await;
    let o_url = serve_double(o.clone(), "/v1/chat/completions").await;
    let cfg = LlmConfig {
        anthropic: Some(Upstream {
            base: a_url,
            key: "sk-ant-test".into(),
            fast: "claude-haiku-5-5".into(),
            smart: "claude-sonnet-5-5".into(),
        }),
        openai: openai.then(|| Upstream {
            base: o_url,
            key: "sk-oai-test".into(),
            fast: "gpt-5-nano".into(),
            smart: "gpt-5-mini".into(),
        }),
        limits,
    };
    let state = Arc::new(LlmState::new(app_state, cfg, upstream_client().unwrap()));
    Some(H {
        app: router(state.clone()),
        state,
        pool,
        anthropic: a,
        openai: o,
    })
}

async fn harness() -> Option<H> {
    harness_with(false, Limits::default()).await
}

fn body(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "max_tokens": 64,
        "stream": stream,
        "system": "You complete the user's sentence.",
        "messages": [{"role": "user", "content": "Thanks for the update, I will"}],
    })
}

fn request(user: Option<Uuid>, b: &Value) -> Request<Body> {
    let mut r = Request::builder()
        .method("POST")
        .uri("/llm/v1/messages")
        .header("content-type", "application/json");
    if let Some(u) = user {
        r = r.header("authorization", format!("Bearer tok-{u}"));
    }
    r.body(Body::from(b.to_string())).unwrap()
}

async fn send_json(app: &Router, user: Option<Uuid>, b: &Value) -> (StatusCode, Value, String) {
    let res = app.clone().oneshot(request(user, b)).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    )
}

/// Rows in `usage_events` for `user`: (model, route, input, output, cache_read, cost).
async fn usage(pool: &Pg, user: Uuid, want: usize) -> Vec<(String, String, i32, i32, i32, i64)> {
    for _ in 0..100 {
        let rows = pool
            .query(
                "select model, route, input_tokens, output_tokens, cache_read_tokens, cost_micros
                 from public.usage_events where user_id = $1 order by id",
                &[&user],
            )
            .await
            .unwrap();
        if rows.len() >= want {
            return rows
                .iter()
                .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
                .collect();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("expected {want} usage rows");
}

fn entitled(h: &H, plan: &str) -> Entitlements {
    Entitlements::for_plan(plan, h.state.app.plans.get(plan).unwrap())
}

fn last_sent(h: &H) -> Value {
    h.anthropic
        .requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
        .1
}

// ---- tests --------------------------------------------------------------------------------

/// No live subscription: 402 with `subscription_required`, nothing sent upstream (the desktop
/// falls back to local models).
#[tokio::test]
async fn a_subscription_is_required() {
    let Some(h) = harness().await else { return };
    let u = add_user(&h.pool).await;
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(v["error"]["code"], "subscription_required");
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn auth_is_required_and_either_header_works() {
    let Some(h) = harness().await else { return };
    let (s, v, _) = send_json(&h.app, None, &body(FAST, false)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "not_signed_in");
    let bad = Request::builder()
        .method("POST")
        .uri("/llm/v1/messages")
        .header("authorization", "Bearer not-a-token")
        .body(Body::from(body(FAST, false).to_string()))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), 0);

    // `x-api-key` (what a stock Anthropic client sends) is accepted too.
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Json(json!({
        "id": "m", "type": "message", "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 5, "output_tokens": 1}
    })));
    let r = Request::builder()
        .method("POST")
        .uri("/llm/v1/messages")
        .header("x-api-key", format!("tok-{u}"))
        .body(Body::from(body(FAST, false).to_string()))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(r).await.unwrap().status(),
        StatusCode::OK
    );

    let models = Request::builder()
        .uri("/llm/v1/models")
        .header("authorization", format!("Bearer tok-{u}"))
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(models).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let unauth = Request::builder()
        .uri("/llm/v1/models")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(unauth).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

/// The body sent upstream: tier mapped to the plan's model, unknown and server-side fields
/// dropped, output capped, a cache breakpoint on the system prompt, the user as metadata, our
/// key (never the user's token).
#[tokio::test]
async fn requests_are_checked_and_mapped_by_tier_and_plan() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Json(json!({
        "content": [{"type": "text", "text": "ok"}], "usage": {"input_tokens": 5, "output_tokens": 1}
    })));
    let mut b = body(SMART, false);
    b["max_tokens"] = json!(1_000_000);
    b["mcp_servers"] = json!([{"url": "https://evil.example"}]);
    b["service_tier"] = json!("priority");
    let (s, _, _) = send_json(&h.app, Some(u), &b).await;
    assert_eq!(s, StatusCode::OK);
    let (headers, sent) = h
        .anthropic
        .requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(headers["x-api-key"], "sk-ant-test");
    assert!(
        headers.get("authorization").is_none(),
        "the user's token stays here"
    );
    assert_eq!(
        sent["model"], "claude-sonnet-5-5",
        "within the smart allowance"
    );
    assert_eq!(sent["max_tokens"], 8_192, "Heed's smart output cap");
    assert!(sent.get("mcp_servers").is_none() && sent.get("service_tier").is_none());
    assert_eq!(sent["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(sent["metadata"]["user_id"], u.to_string());
    send_json(&h.app, Some(u), &body(FAST, false)).await;
    assert_eq!(last_sent(&h)["model"], "claude-haiku-5-5");

    // Past the plan's fair-use budget (below the hard cap), smart is served by the fast model.
    let spent = subscriber(&h.pool, "heed").await;
    let budget = entitled(&h, "heed").allowance.budget_micros;
    h.pool
        .execute(
            "insert into public.usage_events (user_id, kind, model, cost_micros) values ($1, 'llm', 'x', $2)",
            &[&spent, &budget],
        )
        .await
        .unwrap();
    send_json(&h.app, Some(spent), &body(SMART, false)).await;
    assert_eq!(last_sent(&h)["model"], "claude-haiku-5-5", "downgraded");

    // Heed Max: a larger output cap. A client's own breakpoints are kept as they are.
    let max = subscriber(&h.pool, "heed_max").await;
    let mut b = body(SMART, false);
    b["max_tokens"] = json!(1_000_000);
    b["system"] = json!([{"type": "text", "text": "rules"}, {"type": "text", "text": "voice", "cache_control": {"type": "ephemeral"}}]);
    send_json(&h.app, Some(max), &b).await;
    let sent = last_sent(&h);
    assert_eq!(sent["model"], "claude-sonnet-5-5");
    assert_eq!(sent["max_tokens"], 32_000);
    assert!(sent["system"][0].get("cache_control").is_none());

    // Refused before anything is sent upstream.
    let hits = h.anthropic.hits.load(Ordering::SeqCst);
    let mut cases = vec![
        json!({"model": "claude-opus", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}]}),
        json!({"model": FAST, "max_tokens": 5, "messages": []}),
        json!({"model": FAST, "messages": [{"role": "user", "content": "x"}]}),
        json!({"model": FAST, "max_tokens": 0, "stream": true, "messages": [{"role": "user", "content": "x"}]}),
        json!({"model": FAST, "max_tokens": 5, "messages": [{"role": "system", "content": "x"}]}),
    ];
    let mut tool = body(FAST, true);
    tool["tools"] = json!([{"type": "web_search_20250305", "name": "web_search"}]);
    cases.push(tool);
    for c in cases {
        let (s, v, _) = send_json(&h.app, Some(u), &c).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{c}");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(
            !msg.contains("Thanks for the update"),
            "errors never echo content"
        );
    }
    let r = Request::builder()
        .method("POST")
        .uri("/llm/v1/messages")
        .header("authorization", format!("Bearer tok-{u}"))
        .body(Body::from("{not json"))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(r).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), hits);
}

#[tokio::test]
async fn oversized_requests_are_refused() {
    let limits = Limits {
        max_body_bytes: 4 * 1024,
        ..Limits::default()
    };
    let Some(h) = harness_with(false, limits).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let mut b = body(FAST, true);
    b["messages"][0]["content"] = json!("x".repeat(10_000));
    let (s, v, _) = send_json(&h.app, Some(u), &b).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(v["error"]["type"], "request_too_large");
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), 0);
}

/// The provider's bytes are relayed unchanged and the request is metered with its cache reads.
#[tokio::test]
async fn streams_are_relayed_and_metered() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    let chunks = anthropic_stream(&["get", " back", " to you"], 1_000, 4_000, 6);
    let expected: String = chunks.concat();
    *h.anthropic.script.lock().unwrap() = Some(streaming(chunks));
    let (s, _, text) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(text, expected, "byte-for-byte relay");
    let rows = usage(&h.pool, u, 1).await;
    let (model, route, input, output, cache_read, cost) = rows[0].clone();
    assert_eq!(model, "claude-haiku-5-5");
    assert_eq!(route, "heed-fast:anthropic");
    assert_eq!((input, output, cache_read), (1_000, 6, 4_000));
    // 1,000 × 0.10 + 4,000 × 0.01 + 6 × 0.50 = 100 + 40 + 3 micro-USD.
    assert_eq!(cost, 143);
    assert_eq!(h.state.in_flight(u), 0, "slot released");
}

#[tokio::test]
async fn quota_is_enforced_from_usage_events() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    h.pool
        .execute(
            "insert into public.usage_events (user_id, kind, model, cost_micros) values ($1, 'llm', 'x', $2)",
            &[&u, &entitled(&h, "heed").allowance.hard_cap_micros],
        )
        .await
        .unwrap();
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["type"], "rate_limit_error");
    assert_eq!(v["error"]["code"], "quota_exceeded");
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), 0);

    // Costs metered within the cache window count at once.
    let v2 = subscriber(&h.pool, "heed").await;
    let quota = entitled(&h, "heed").allowance.hard_cap_micros;
    h.pool
        .execute(
            "insert into public.usage_events (user_id, kind, model, cost_micros) values ($1, 'llm', 'x', $2)",
            &[&v2, &(quota - 100)],
        )
        .await
        .unwrap();
    *h.anthropic.script.lock().unwrap() = Some(streaming(anthropic_stream(&["a"], 1_000, 0, 200)));
    let (s, _, _) = send_json(&h.app, Some(v2), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::OK);
    usage(&h.pool, v2, 2).await;
    let (s, _, _) = send_json(&h.app, Some(v2), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn concurrent_streams_per_user_are_capped() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::ZERO,
        chunks: anthropic_stream(&["a"], 10, 0, 1)[..1].to_vec(),
        gap: Duration::ZERO,
        hang: true,
    });
    let max = entitled(&h, "heed").max_concurrent;
    let mut open = Vec::new();
    for _ in 0..max {
        let res = h
            .app
            .clone()
            .oneshot(request(Some(u), &body(FAST, true)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        open.push(res);
    }
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "too_many_concurrent");
    drop(open);
    assert_eq!(h.state.in_flight(u), 0);
}

/// The client goes away mid-answer: the provider request is dropped at once and what was
/// relayed is metered (output estimated, since the provider never reported it).
#[tokio::test]
async fn client_disconnect_cancels_upstream_and_meters() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    let mut chunks = anthropic_stream(&["one two three"], 500, 0, 99);
    chunks.truncate(3); // start, block start, one delta; then the provider keeps going
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::ZERO,
        chunks,
        gap: Duration::from_millis(5),
        hang: true,
    });
    let res = h
        .app
        .clone()
        .oneshot(request(Some(u), &body(FAST, true)))
        .await
        .unwrap();
    let mut b = res.into_body();
    let mut got = String::new();
    while !got.contains("one two three") {
        let f = b.frame().await.unwrap().unwrap();
        got.push_str(&String::from_utf8_lossy(&f.into_data().unwrap()));
    }
    drop(b);
    for _ in 0..100 {
        if h.anthropic.cut_off.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        h.anthropic.cut_off.load(Ordering::SeqCst),
        "upstream dropped"
    );
    let rows = usage(&h.pool, u, 1).await;
    assert_eq!(rows[0].2, 500);
    assert_eq!(rows[0].3, 5, "13 chars relayed, estimated at 3 per token");
    assert_eq!(h.state.in_flight(u), 0);
}

#[tokio::test]
async fn provider_failures_map_safely() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    // Our key refused: a 502, never a 401 that would sign the user out.
    *h.anthropic.script.lock().unwrap() = Some(Script::Status(
        401,
        json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}),
    ));
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert!(!v.to_string().contains("x-api-key"));
    // The request refused: relayed as it is.
    *h.anthropic.script.lock().unwrap() = Some(Script::Status(
        400,
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": "messages.0: bad"}}),
    ));
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["message"], "messages.0: bad");
    // Overloaded without a fallback: 529 (the desktop falls back to a local model).
    *h.anthropic.script.lock().unwrap() = Some(Script::Status(529, json!({})));
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s.as_u16(), 529);
    assert_eq!(v["error"]["type"], "overloaded_error");
    // Nothing metered for refused requests.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n: i64 = h
        .pool
        .query_one(
            "select count(*) from public.usage_events where user_id = $1",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
}

/// Anthropic overloaded: the answer comes from OpenAI, translated to Anthropic events.
#[tokio::test]
async fn falls_back_to_openai_translated() {
    let Some(h) = harness_with(true, Limits::default()).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Status(529, json!({})));
    let data = |v: Value| format!("data: {v}\n\n");
    *h.openai.script.lock().unwrap() = Some(streaming(vec![
        data(
            json!({"id": "chatcmpl-1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": "get "}}]}),
        ),
        data(
            json!({"id": "chatcmpl-1", "choices": [{"index": 0, "delta": {"content": "back"}, "finish_reason": "stop"}]}),
        ),
        data(
            json!({"id": "chatcmpl-1", "choices": [], "usage": {"prompt_tokens": 300, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 100}}}),
        ),
        "data: [DONE]\n\n".into(),
    ]));
    let (s, _, text) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::OK);
    let mut p = sse::Parser::default();
    let mut evs = p.feed(text.as_bytes());
    evs.extend(p.finish());
    let names: Vec<&str> = evs.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"message_start"));
    assert_eq!(names.last(), Some(&"message_stop"));
    assert!(text.contains("get ") && text.contains("back"));
    let (_, sent) = h.openai.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent["model"], "gpt-5-nano");
    assert!(
        sent["prompt_cache_key"]
            .as_str()
            .unwrap()
            .ends_with(":heed-fast")
    );
    let rows = usage(&h.pool, u, 1).await;
    assert_eq!(rows[0].1, "heed-fast:openai");
    assert_eq!((rows[0].2, rows[0].3, rows[0].4), (200, 2, 100));
}

#[tokio::test]
async fn timeouts_end_requests() {
    let limits = Limits {
        headers_timeout: Duration::from_millis(150),
        idle_timeout: Duration::from_millis(150),
        ..Limits::default()
    };
    let Some(h) = harness_with(false, limits).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::from_secs(5),
        chunks: vec![],
        gap: Duration::ZERO,
        hang: false,
    });
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(v["error"]["type"], "timeout_error");
    // Silence mid-stream: the stream ends with an `error` event.
    let mut chunks = anthropic_stream(&["a"], 10, 0, 1);
    chunks.truncate(2);
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::ZERO,
        chunks,
        gap: Duration::ZERO,
        hang: true,
    });
    let (s, _, text) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        text.ends_with(
            &String::from_utf8(
                sse_error("timeout_error", "the AI provider stopped answering").to_vec()
            )
            .unwrap()
        ),
        "{text}"
    );
}

/// Everything this test binary logs, from the first call of [`captured_logs`] on (a global
/// subscriber: a thread-local one misses events whose callsite another test thread cached).
fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    static BUF: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    BUF.get_or_init(|| {
        #[derive(Clone)]
        struct W(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = W(buf.clone());
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || w.clone())
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
    .clone()
}

/// Logs carry sizes and timings, never the prompt, the answer, a provider's echo or a key.
#[tokio::test]
async fn prompts_are_never_logged() {
    let buf = captured_logs();
    let Some(h) = harness_with(true, Limits::default()).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Status(
        400,
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": "PROVIDER-ECHO-SECRET"}}),
    ));
    let mut b = body(FAST, true);
    b["messages"][0]["content"] = json!("PROMPT-SECRET-7731");
    send_json(&h.app, Some(u), &b).await;
    *h.anthropic.script.lock().unwrap() =
        Some(streaming(anthropic_stream(&["ANSWER-SECRET"], 5, 0, 1)));
    send_json(&h.app, Some(u), &b).await;
    usage(&h.pool, u, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        logs.lines()
            .any(|l| l.contains("llm request") && l.contains(&u.to_string())),
        "{logs}"
    );
    for secret in [
        "PROMPT-SECRET",
        "ANSWER-SECRET",
        "PROVIDER-ECHO-SECRET",
        "sk-ant-test",
    ] {
        assert!(!logs.contains(secret), "{secret} in logs");
    }
}

/// The desktop's provider (nugget-infer) against the endpoint over real HTTP, with the
/// provider double answering its first token immediately: time to first token measured cold
/// and warm, the warm request on the prewarmed connection, and the service's own overhead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_provider_end_to_end_time_to_first_token() {
    use nugget_infer::cloud::{CloudSession, build_cloud_providers};
    use nugget_infer::{ChatEvent, ChatRequest, Message};

    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(streaming(anthropic_stream(
        &[" get", " back to you"],
        40,
        0,
        4,
    )));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let l = axum::serve::ListenerExt::tap_io(l, |t| {
        let _ = t.set_nodelay(true);
    });
    let app = h.app.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    struct NoLedger;
    impl nugget_core::egress::EgressSink for NoLedger {
        fn record(&self, _: nugget_core::egress::EgressEvent) {}
    }
    let session = Arc::new(CloudSession::new());
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    session.set(nugget_infer::Secret::new(format!("tok-{u}")), now + 3600);
    let p = tokio::task::spawn_blocking(move || {
        build_cloud_providers(&format!("http://{addr}"), session, Arc::new(NoLedger))
    })
    .await
    .unwrap()
    .unwrap();

    async fn ttft(p: &dyn nugget_infer::Provider) -> nugget_infer::Timings {
        let req = ChatRequest::new(vec![Message::user_text("Thanks, I will")]);
        let mut s = p.chat(req).await.unwrap();
        let mut timings = None;
        let mut text = String::new();
        while let Some(e) = futures::StreamExt::next(&mut s).await {
            match e.unwrap() {
                ChatEvent::TextDelta(t) => text.push_str(&t),
                ChatEvent::Stats(st) => timings = st.timings,
                _ => {}
            }
        }
        assert_eq!(text, " get back to you");
        timings.unwrap()
    }

    let cold = ttft(p.fast.as_ref()).await;
    p.fast.prewarm().await.unwrap();
    let mut warm = Vec::new();
    let mut new_connections = 0;
    for _ in 0..20 {
        // Typing cadence: a pause between requests (the pool returns a connection once the
        // previous response is fully read, which a back-to-back request can race).
        tokio::time::sleep(Duration::from_millis(30)).await;
        let t = ttft(p.smart.as_ref()).await;
        new_connections += usize::from(t.connect.is_some());
        warm.push(t.first_token.unwrap());
    }
    assert!(
        new_connections <= 1,
        "{new_connections} of 20 warm requests opened a connection"
    );
    warm.sort();
    let p50 = warm[warm.len() / 2];
    let p95 = warm[warm.len() * 95 / 100];
    eprintln!(
        "ttft cold={:?} (connect {:?}) warm p50={p50:?} p95={p95:?} new connections={new_connections}",
        cold.first_token, cold.connect
    );
    // Auth is cached, entitlements and usage are cached, the provider connection is pooled:
    // the endpoint adds no database or TLS round trip on the warm path.
    assert!(p50 < Duration::from_millis(50), "warm p50 {p50:?}");
}

// ---- rate limit, deadlines, reserve-then-settle ---------------------------------------------

async fn spend(pool: &Pg, user: Uuid, micros: i64) {
    pool.execute(
        "insert into public.usage_events (user_id, kind, model, cost_micros) values ($1, 'llm', 'seed', $2)",
        &[&user, &micros],
    )
    .await
    .unwrap();
}

async fn wait_reserved(h: &H, user: Uuid, want: i64) {
    let db = h.state.app.db.clone();
    crate::testkit::wait_until(&format!("{want} micro-USD reserved"), 30, || {
        let db = db.clone();
        async move { db.ai_reserved(user).await.unwrap() == want }
    })
    .await;
}

/// The worst case the endpoint reserves for `b` on `model`, computed as the handler does.
fn estimate(h: &H, plan: &str, b: &Value, model: &str) -> i64 {
    let c = check(b.clone(), &entitled(h, plan), &h.state.cfg.limits).unwrap();
    estimate_micros(&c.body, model)
}

#[tokio::test]
async fn model_endpoint_is_rate_limited_per_user() {
    let rate = crate::ratelimit::Limits {
        llm_per_min: 3,
        ..crate::ratelimit::Limits::unlimited()
    };
    let Some(h) = harness_full(false, Limits::default(), rate).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let other = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Json(json!({
        "id": "m", "type": "message", "content": [], "usage": {"input_tokens": 5, "output_tokens": 1}
    })));
    let models = |user: Uuid| {
        Request::get("/llm/v1/models")
            .header("authorization", format!("Bearer tok-{user}"))
            .body(Body::empty())
            .unwrap()
    };
    // Models and messages share the bucket.
    assert_eq!(
        h.app.clone().oneshot(models(u)).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        send_json(&h.app, Some(u), &body(FAST, false)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        h.app.clone().oneshot(models(u)).await.unwrap().status(),
        StatusCode::OK
    );
    let res = h
        .app
        .clone()
        .oneshot(request(Some(u), &body(FAST, false)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(res.headers().get("retry-after").is_some());
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        (v["type"].as_str(), v["error"]["code"].as_str()),
        (Some("error"), Some("rate_limited"))
    );
    assert_eq!(
        h.anthropic.hits.load(Ordering::SeqCst),
        1,
        "the refused request never reached the provider"
    );
    assert_eq!(
        h.app.clone().oneshot(models(other)).await.unwrap().status(),
        StatusCode::OK,
        "per user"
    );
}

#[tokio::test]
async fn non_streaming_requests_get_a_deadline_and_streams_keep_theirs() {
    let limits = Limits {
        non_stream_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let Some(h) = harness_full(false, limits, crate::ratelimit::Limits::unlimited()).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    // The provider takes longer than the non-streaming deadline to answer.
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::from_millis(900),
        chunks: anthropic_stream(&["late"], 10, 0, 1),
        gap: Duration::ZERO,
        hang: false,
    });
    let (s, v, _) = send_json(&h.app, Some(u), &body(FAST, false)).await;
    assert_eq!(
        (s, v["error"]["type"].as_str()),
        (StatusCode::GATEWAY_TIMEOUT, Some("timeout_error"))
    );
    wait_reserved(&h, u, 0).await; // the cancelled request's reservation is released
    // The same latency is fine for a stream: it is bounded by the stream limits instead.
    let (s, _, text) = send_json(&h.app, Some(u), &body(FAST, true)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(text.contains("late"), "{text}");
}

#[tokio::test]
async fn concurrent_streams_cannot_jointly_pass_the_cap() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    let b = body(FAST, true);
    let est = estimate(&h, "heed", &b, "claude-haiku-5-5");
    assert!(est > 0);
    // Room for exactly two worst cases under Heed's hard cap.
    let cap = entitled(&h, "heed").allowance.hard_cap_micros;
    spend(&h.pool, u, cap - 2 * est - est / 2).await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::ZERO,
        chunks: anthropic_stream(&["a"], 10, 0, 1)[..1].to_vec(),
        gap: Duration::ZERO,
        hang: true,
    });
    let attempts =
        futures::future::join_all((0..4).map(|_| h.app.clone().oneshot(request(Some(u), &b))))
            .await;
    let mut open = Vec::new();
    let mut refused = 0;
    for r in attempts {
        let r = r.unwrap();
        match r.status() {
            StatusCode::OK => open.push(r),
            StatusCode::TOO_MANY_REQUESTS => {
                let v: Value =
                    serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                assert_eq!(v["error"]["code"], "quota_exceeded");
                refused += 1;
            }
            s => panic!("unexpected {s}"),
        }
    }
    assert_eq!(
        (open.len(), refused),
        (2, 2),
        "only what fits under the cap gets through"
    );
    assert_eq!(h.state.app.db.ai_reserved(u).await.unwrap(), 2 * est);
    assert_eq!(h.anthropic.hits.load(Ordering::SeqCst), 2);
    // The clients go away: both settle (with the tokens relayed so far) and release their holds.
    drop(open);
    wait_reserved(&h, u, 0).await;
}

#[tokio::test]
async fn settling_records_real_usage_and_releases_the_rest() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Json(json!({
        "id": "m", "type": "message", "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 20, "output_tokens": 2}
    })));
    let b = body(FAST, false);
    let est = estimate(&h, "heed", &b, "claude-haiku-5-5");
    let (s, _, _) = send_json(&h.app, Some(u), &b).await;
    assert_eq!(s, StatusCode::OK);
    let rows = usage(&h.pool, u, 1).await;
    let real = crate::gateway::cost_micros("claude-haiku-5-5", 20, 2);
    assert_eq!((rows[0].2, rows[0].3, rows[0].5), (20, 2, real));
    assert!(real < est, "the estimate is a worst case ({real} < {est})");
    wait_reserved(&h, u, 0).await;
}

#[tokio::test]
async fn a_disconnect_settles_with_the_relayed_tokens() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    let mut chunks = anthropic_stream(&["one two three"], 500, 0, 99);
    chunks.truncate(3);
    *h.anthropic.script.lock().unwrap() = Some(Script::Stream {
        wait: Duration::ZERO,
        chunks,
        gap: Duration::from_millis(5),
        hang: true,
    });
    let res = h
        .app
        .clone()
        .oneshot(request(Some(u), &body(FAST, true)))
        .await
        .unwrap();
    assert!(
        h.state.app.db.ai_reserved(u).await.unwrap() > 0,
        "held while streaming"
    );
    let mut b = res.into_body();
    let mut got = String::new();
    while !got.contains("one two three") {
        let f = b.frame().await.unwrap().unwrap();
        got.push_str(&String::from_utf8_lossy(&f.into_data().unwrap()));
    }
    drop(b);
    let rows = usage(&h.pool, u, 1).await;
    assert_eq!(rows[0].2, 500, "input from message_start");
    assert!(rows[0].3 > 0, "output estimated from what was relayed");
    wait_reserved(&h, u, 0).await;
}

#[tokio::test]
async fn smart_falls_back_to_fast_when_its_reserve_fails() {
    let Some(h) = harness().await else { return };
    let u = subscriber(&h.pool, "heed").await;
    *h.anthropic.script.lock().unwrap() = Some(Script::Json(json!({
        "id": "m", "type": "message", "content": [], "usage": {"input_tokens": 5, "output_tokens": 1}
    })));
    let b = body(SMART, false);
    let a = entitled(&h, "heed").allowance;
    // Under the fair-use budget by less than a smart worst case, far under the hard cap.
    let smart_est = estimate(&h, "heed", &b, "claude-sonnet-5-5");
    spend(&h.pool, u, a.budget_micros - smart_est / 2).await;
    let (s, _, _) = send_json(&h.app, Some(u), &b).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        last_sent(&h)["model"],
        "claude-haiku-5-5",
        "served by the fast model"
    );
    // With room for the smart worst case, smart is served.
    let v = subscriber(&h.pool, "heed").await;
    let (s, _, _) = send_json(&h.app, Some(v), &b).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(last_sent(&h)["model"], "claude-sonnet-5-5");
    // At the hard cap, nothing fits: 429.
    let w = subscriber(&h.pool, "heed").await;
    spend(&h.pool, w, a.hard_cap_micros).await;
    let (s, v, _) = send_json(&h.app, Some(w), &b).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("quota_exceeded"))
    );
}

#[test]
fn estimates_err_high_and_count_images_flat() {
    let b = json!({"max_tokens": 100, "system": "abc", "messages": [
        {"role": "user", "content": [{"type": "text", "text": "x".repeat(300)},
                                      {"type": "image", "source": {"type": "base64", "data": "A".repeat(100_000)}}]}
    ]});
    let Value::Object(m) = b else { unreachable!() };
    let t = estimate_input_tokens(&m);
    assert!(
        (100 + IMAGE_TOKENS_ESTIMATE..100 + IMAGE_TOKENS_ESTIMATE + 100).contains(&t),
        "{t}"
    );
    let (i, o) = price_per_mtok("claude-haiku-5-5");
    assert_eq!(
        estimate_micros(&m, "claude-haiku-5-5"),
        (t as f64 * i + 100.0 * o).ceil() as i64
    );
}
