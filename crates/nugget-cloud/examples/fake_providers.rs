//! Stand-ins for Composio, Anthropic, OpenAI and Stripe, for self-hosting smoke tests and the
//! desktop's `cloud` e2e scenario without real keys. Point the server at it with
//! `COMPOSIO_BASE_URL`, `NUGGET_ANTHROPIC_BASE_URL`, `NUGGET_OPENAI_BASE_URL` and
//! `NUGGET_STRIPE_BASE_URL`. Never use it in production: it accepts any key.
//!
//! ```sh
//! FAKE_PROVIDERS_BIND=0.0.0.0:9100 cargo run -p nugget-cloud --example fake_providers
//! ```
//!
//! - Composio: two connectable toolkits (Gmail, Calendar) with mail and an event dated relative to
//!   now, and the action tools (reply, send, create/patch event) recorded in `GET /__fake/calls`.
//! - Anthropic `/v1/messages`: answers by prompt (knowledge-graph JSON, a draft reply, the
//!   brief, a cited answer, ghost text), streamed when `stream` is set.
//! - Stripe: customers, Checkout, the portal, subscriptions and invoice previews. Opening a
//!   Checkout URL (`GET /c/{id}`) plays the browser paying: it creates a trialing subscription,
//!   sends a signed `checkout.session.completed` to `FAKE_STRIPE_WEBHOOK_URL` (signed with
//!   `FAKE_STRIPE_WEBHOOK_SECRET`) and redirects to the session's `success_url`.
//!   `FAKE_PUBLIC_URL` is this server's address as a browser sees it; `FAKE_COMPOSIO_DELAY_MS`
//!   slows each mail and calendar fetch, so an import can be watched.
//! - `POST /__fake/reset` forgets everything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use time::{Duration, OffsetDateTime};

#[derive(Default)]
struct Fake {
    /// user → (account id, toolkit)
    accounts: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Composio action executions: `{tool, user_id, connected_account_id, arguments}`.
    calls: Mutex<Vec<Value>>,
    /// Stripe objects by id (customers, checkout sessions, subscriptions).
    stripe: Mutex<HashMap<String, Value>>,
    /// Model requests: `{system, prompt}` (their first 300 characters).
    llm_calls: Mutex<Vec<Value>>,
    /// Stripe requests: `{method, path, form}`.
    stripe_calls: Mutex<Vec<Value>>,
    next: Mutex<u32>,
    /// This process's start time, in every id.
    nonce: u64,
    public_url: String,
    /// Latency of each mail/calendar fetch (`FAKE_COMPOSIO_DELAY_MS`), so imports are visible.
    fetch_delay: std::time::Duration,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
}

impl Fake {
    /// A fresh id; unique across restarts too (a database keeps the ids an earlier run handed out).
    fn id(&self, prefix: &str) -> String {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        format!("{prefix}fake{}x{n}", self.nonce)
    }
    fn reset(&self) {
        self.accounts.lock().unwrap().clear();
        self.calls.lock().unwrap().clear();
        self.llm_calls.lock().unwrap().clear();
        self.stripe.lock().unwrap().clear();
        self.stripe_calls.lock().unwrap().clear();
    }
}

type S = State<Arc<Fake>>;

fn rfc(t: OffsetDateTime) -> String {
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

// ---- Composio ----

async fn toolkits() -> Json<Value> {
    Json(json!({"items": [
        {"slug": "gmail", "name": "Gmail", "meta": {"logo": null}},
        {"slug": "googlecalendar", "name": "Google Calendar", "meta": {"logo": null}},
        {"slug": "slack", "name": "Slack", "meta": {"logo": null}}
    ]}))
}

async fn link(State(f): S, Json(b): Json<Value>) -> Json<Value> {
    let user = b["user_id"].as_str().unwrap_or_default().to_owned();
    let toolkit = if b["auth_config_id"]
        .as_str()
        .unwrap_or_default()
        .contains("cal")
    {
        "googlecalendar"
    } else {
        "gmail"
    };
    let id = f.id("ca_");
    f.accounts
        .lock()
        .unwrap()
        .entry(user)
        .or_default()
        .push((id.clone(), toolkit.into()));
    Json(json!({"connected_account_id": id,
                "redirect_url": format!("{}/connect/{id}", f.public_url),
                "link_token": "lt", "expires_at": "2099-01-01T00:00:00Z"}))
}

async fn accounts(State(f): S, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let user = q.get("user_ids").cloned().unwrap_or_default();
    let items: Vec<Value> = f
        .accounts
        .lock()
        .unwrap()
        .get(&user)
        .into_iter()
        .flatten()
        .map(|(id, t)| json!({"id": id, "status": "ACTIVE", "toolkit": {"slug": t}}))
        .collect();
    Json(json!({"items": items, "next_cursor": null}))
}

async fn delete_account(State(f): S, Path(id): Path<String>) -> Json<Value> {
    for list in f.accounts.lock().unwrap().values_mut() {
        list.retain(|(a, _)| *a != id);
    }
    Json(json!({"success": true}))
}

/// The mailbox: an old sent mail (so the owner's address is known), Dana asking for the signed
/// contract (an unanswered thread), and the lease renewal (the cited Ask answer).
fn mailbox(now: OffsetDateTime) -> Value {
    json!({"messages": [
        {"messageId": "m-sent1", "threadId": "t-sent1", "messageTimestamp": rfc(now - Duration::days(10)),
         "sender": "Me <me@me.example>", "to": "pat@other.example", "subject": "Hello",
         "labelIds": ["SENT"], "messageText": "Thanks Pat, talk soon."},
        {"messageId": "m-dana1", "threadId": "t-dana1", "messageTimestamp": rfc(now - Duration::days(3)),
         "sender": "Dana Okoro <dana@harbourline.example>", "to": "me@me.example",
         "subject": "Signed contract", "labelIds": ["INBOX", "UNREAD"],
         "messageText": "Hi, I'm Dana Okoro and I work at Harbourline. Could you send the signed contract by Friday?"},
        {"messageId": "m-lease1", "threadId": "t-lease1", "messageTimestamp": rfc(now - Duration::days(4)),
         "sender": "Landlord <ll@lettings.example>", "to": "me@me.example", "subject": "Lease renewal",
         "labelIds": ["INBOX"], "messageText": "Hi, your lease renews on 1 December. The rent stays the same."},
        {"messageId": "m-sam1", "threadId": "t-sam1", "messageTimestamp": rfc(now - Duration::days(2)),
         "sender": "Sam Lee <sam@studio.example>", "to": "me@me.example", "subject": "Venue for Thursday",
         "labelIds": ["INBOX", "UNREAD"],
         "messageText": "Hi, could you confirm the venue for Thursday's workshop?"},
        {"messageId": "m-priya1", "threadId": "t-priya1", "messageTimestamp": rfc(now - Duration::days(2)),
         "sender": "Priya Shah <priya@northwind.example>", "to": "me@me.example", "subject": "Quarterly numbers",
         "labelIds": ["INBOX", "UNREAD"],
         "messageText": "Hello, can you send me the quarterly numbers before our call?"}
    ]})
}

fn calendar(now: OffsetDateTime) -> Value {
    let start = now + Duration::days(1);
    json!({"items": [
        {"id": "ev-kickoff", "status": "confirmed", "summary": "Harbourline kickoff",
         "start": {"dateTime": rfc(start)}, "end": {"dateTime": rfc(start + Duration::hours(1))},
         "organizer": {"email": "me@me.example"},
         "attendees": [{"email": "me@me.example", "responseStatus": "accepted", "self": true},
                       {"email": "dana@harbourline.example", "displayName": "Dana Okoro", "responseStatus": "accepted"}],
         "description": "Kickoff with Dana"}
    ]})
}

async fn execute(State(f): S, Path(tool): Path<String>, Json(b): Json<Value>) -> Json<Value> {
    let now = OffsetDateTime::now_utc();
    if matches!(
        tool.as_str(),
        "GMAIL_FETCH_EMAILS" | "GOOGLECALENDAR_EVENTS_LIST"
    ) {
        tokio::time::sleep(f.fetch_delay).await;
    }
    let data = match tool.as_str() {
        "GMAIL_FETCH_EMAILS" => mailbox(now),
        "GOOGLECALENDAR_EVENTS_LIST" => calendar(now),
        "GMAIL_REPLY_TO_THREAD"
        | "GMAIL_SEND_EMAIL"
        | "GOOGLECALENDAR_CREATE_EVENT"
        | "GOOGLECALENDAR_PATCH_EVENT" => {
            let id = f.id("exec_");
            let mut call = b.clone();
            call["tool"] = json!(tool);
            f.calls.lock().unwrap().push(call);
            json!({"id": id, "threadId": b["arguments"]["thread_id"], "status": "ok"})
        }
        _ => json!({}),
    };
    Json(json!({"successful": true, "error": null, "data": data}))
}

// ---- Anthropic and OpenAI ----

/// Every string in a system prompt or messages value, joined (string or content blocks).
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(text_of).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => o
            .iter()
            .filter(|(k, _)| matches!(k.as_str(), "text" | "content"))
            .map(|(_, v)| text_of(v))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn answer(system: &str, prompt: &str) -> String {
    let all = format!("{system}\n{prompt}");
    if system.contains("knowledge graph") {
        if prompt.contains("Dana Okoro") {
            json!({
                "entities": [{"name": "Dana Okoro", "kind": "person"}, {"name": "Harbourline", "kind": "organization"}],
                "claims": [{"subject": "Dana Okoro", "predicate": "works_at", "object": "Harbourline",
                            "quote": "I work at Harbourline"}]
            })
            .to_string()
        } else {
            json!({"entities": [], "claims": []}).to_string()
        }
    } else if system.starts_with("You draft a short email reply") {
        "Hi Dana, the signed contract is attached. Best, Me".into()
    } else if system.starts_with("You write the user's morning brief") {
        "Dana is waiting on the signed contract [1].".into()
    } else if all.contains("Continue the user's text at the caret") {
        " and I will send it over tomorrow.".into()
    } else if prompt.contains("<source n=\"1\"") {
        "Your lease renews on 1 December [1].".into()
    } else if system.starts_with("Answer the user's question using only")
        && all.contains("Atlas room")
    {
        "The kickoff is in the Atlas room on the fourth floor [1].".into()
    } else {
        "I couldn't find that in your mail or calendar.".into()
    }
}

fn sse(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn log_llm(f: &Fake, system: &str, prompt: &str) {
    let head = |s: &str| s.chars().take(300).collect::<String>();
    f.llm_calls
        .lock()
        .unwrap()
        .push(json!({"system": head(system), "prompt": head(prompt)}));
}

async fn messages(State(f): S, Json(b): Json<Value>) -> Response {
    let system = text_of(&b["system"]);
    let prompt = text_of(&b["messages"]);
    log_llm(&f, &system, &prompt);
    let text = answer(&system, &prompt);
    let model = b["model"].as_str().unwrap_or("fake-model").to_owned();
    let (input, output) = (1000, (text.len() / 4).max(1) as u64);
    if b["stream"] != Value::Bool(true) {
        return Json(json!({"id": "msg_fake", "type": "message", "role": "assistant", "model": model,
            "content": [{"type": "text", "text": text}], "stop_reason": "end_turn", "stop_sequence": null,
            "usage": {"input_tokens": input, "output_tokens": output}}))
        .into_response();
    }
    let mut out = sse(
        "message_start",
        &json!({"type": "message_start", "message": {"id": "msg_fake", "type": "message", "role": "assistant",
            "model": model, "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": input, "output_tokens": 1}}}),
    );
    out += &sse(
        "content_block_start",
        &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    );
    // Two deltas, so clients see a real stream.
    let mid = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(0, |(i, _)| i);
    for part in [&text[..mid], &text[mid..]] {
        out += &sse(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": part}}),
        );
    }
    out += &sse(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": 0}),
    );
    out += &sse(
        "message_delta",
        &json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": output}}),
    );
    out += &sse("message_stop", &json!({"type": "message_stop"}));
    ([(header::CONTENT_TYPE, "text/event-stream")], out).into_response()
}

async fn models() -> Json<Value> {
    Json(
        json!({"data": [{"type": "model", "id": "fake-model", "display_name": "Fake"}],
                "has_more": false, "first_id": "fake-model", "last_id": "fake-model"}),
    )
}

async fn chat(State(f): S, Json(b): Json<Value>) -> Json<Value> {
    let msgs = b["messages"].as_array().cloned().unwrap_or_default();
    let system = msgs
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| text_of(&m["content"]))
        .collect::<String>();
    let prompt = msgs
        .iter()
        .filter(|m| m["role"] != "system")
        .map(|m| text_of(&m["content"]))
        .collect::<Vec<_>>()
        .join("\n");
    log_llm(&f, &system, &prompt);
    Json(
        json!({"choices": [{"message": {"role": "assistant", "content": answer(&system, &prompt)}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 20}}),
    )
}

async fn embeddings(Json(b): Json<Value>) -> Json<Value> {
    let inputs: Vec<String> = b["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut v = vec![0f32; 1536];
            for w in t
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
            {
                let h = w
                    .bytes()
                    .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                v[(h % 1536) as usize] += 1.0;
            }
            v[0] += 0.01;
            json!({"index": i, "embedding": v})
        })
        .collect();
    Json(
        json!({"data": data, "usage": {"prompt_tokens": inputs.iter().map(|t| t.len() / 4).sum::<usize>()}}),
    )
}

// ---- Stripe ----

/// `application/x-www-form-urlencoded` pairs (axum is built without its form feature here).
fn form(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn stripe_err(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"type": "invalid_request_error", "message": message}})),
    )
        .into_response()
}

fn record(f: &Fake, method: &str, path: &str, form: &HashMap<String, String>) {
    f.stripe_calls
        .lock()
        .unwrap()
        .push(json!({"method": method, "path": path, "form": form}));
}

async fn customers(State(f): S, body: Bytes) -> Json<Value> {
    let p = form(&body);
    record(&f, "POST", "/v1/customers", &p);
    let id = f.id("cus_");
    let c = json!({"id": id, "object": "customer", "email": p.get("email"),
                   "metadata": {"user_id": p.get("metadata[user_id]")}});
    f.stripe.lock().unwrap().insert(id, c.clone());
    Json(c)
}

async fn delete_customer(State(f): S, Path(id): Path<String>) -> Response {
    record(
        &f,
        "DELETE",
        &format!("/v1/customers/{id}"),
        &HashMap::new(),
    );
    if f.stripe.lock().unwrap().remove(&id).is_none() {
        return stripe_err(StatusCode::NOT_FOUND, "No such customer");
    }
    Json(json!({"id": id, "object": "customer", "deleted": true})).into_response()
}

async fn checkout(State(f): S, body: Bytes) -> Json<Value> {
    let p = form(&body);
    record(&f, "POST", "/v1/checkout/sessions", &p);
    let id = f.id("cs_");
    let s = json!({"id": id, "object": "checkout.session", "url": format!("{}/c/{id}", f.public_url),
                   "customer": p.get("customer"), "client_reference_id": p.get("client_reference_id"),
                   "success_url": p.get("success_url"), "cancel_url": p.get("cancel_url"),
                   "price": p.get("line_items[0][price]"),
                   "trial_days": p.get("subscription_data[trial_period_days]"),
                   "user_id": p.get("subscription_data[metadata][user_id]")});
    f.stripe.lock().unwrap().insert(id, s.clone());
    Json(s)
}

async fn portal(State(f): S, body: Bytes) -> Json<Value> {
    let p = form(&body);
    record(&f, "POST", "/v1/billing_portal/sessions", &p);
    let id = f.id("bps_");
    Json(json!({"id": id, "object": "billing_portal.session",
                "url": format!("{}/p/{id}", f.public_url), "return_url": p.get("return_url")}))
}

async fn subscription(State(f): S, Path(id): Path<String>) -> Response {
    record(
        &f,
        "GET",
        &format!("/v1/subscriptions/{id}"),
        &HashMap::new(),
    );
    match f.stripe.lock().unwrap().get(&id) {
        Some(s) => Json(s.clone()).into_response(),
        None => stripe_err(StatusCode::NOT_FOUND, "No such subscription"),
    }
}

async fn invoice_preview(State(f): S, body: Bytes) -> Json<Value> {
    let p = form(&body);
    record(&f, "POST", "/v1/invoices/create_preview", &p);
    Json(json!({"object": "invoice", "amount_due": 900, "currency": "usd"}))
}

/// The browser completing Checkout: a trialing subscription, the signed webhook, the redirect.
async fn complete_checkout(State(f): S, Path(id): Path<String>) -> Response {
    let Some(session) = f.stripe.lock().unwrap().get(&id).cloned() else {
        return (StatusCode::NOT_FOUND, "no such checkout session").into_response();
    };
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let trial_days: i64 = session["trial_days"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    let trial_end = now + 86_400 * trial_days;
    let end = if trial_days > 0 {
        trial_end
    } else {
        now + 86_400 * 30
    };
    let sub_id = f.id("sub_");
    let sub = json!({"id": sub_id, "object": "subscription", "customer": session["customer"],
        "status": if trial_days > 0 { "trialing" } else { "active" },
        "trial_end": if trial_days > 0 { json!(trial_end) } else { Value::Null },
        "current_period_end": end, "cancel_at_period_end": false,
        "metadata": {"user_id": session["user_id"]},
        "items": {"data": [{"id": f.id("si_"), "price": {"id": session["price"]}, "current_period_end": end}]}});
    f.stripe.lock().unwrap().insert(sub_id.clone(), sub);
    if let (Some(url), Some(secret)) = (&f.webhook_url, &f.webhook_secret) {
        let event = json!({"id": f.id("evt_"), "type": "checkout.session.completed",
            "data": {"object": {"id": id, "object": "checkout.session", "subscription": sub_id,
                                "customer": session["customer"], "client_reference_id": session["client_reference_id"]}}});
        let body = event.to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
        mac.update(format!("{now}.{body}").as_bytes());
        let sig = format!("t={now},v1={}", hex::encode(mac.finalize().into_bytes()));
        let sent = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .header("stripe-signature", sig)
            .body(body)
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("webhook refused: {status} {text}"),
                )
                    .into_response();
            }
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, format!("webhook failed: {e}")).into_response();
            }
        }
    }
    match session["success_url"].as_str() {
        Some(u) => Redirect::to(u).into_response(),
        None => "paid".into_response(),
    }
}

// ---- inspection ----

async fn fake_calls(State(f): S) -> Json<Value> {
    Json(
        json!({"composio": *f.calls.lock().unwrap(), "stripe": *f.stripe_calls.lock().unwrap(),
                "llm": *f.llm_calls.lock().unwrap()}),
    )
}

async fn fake_reset(State(f): S) -> StatusCode {
    f.reset();
    StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() {
    let bind = std::env::var("FAKE_PROVIDERS_BIND").unwrap_or_else(|_| "127.0.0.1:9100".into());
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let fake = Fake {
        nonce: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        public_url: env("FAKE_PUBLIC_URL")
            .unwrap_or_else(|| format!("http://{bind}"))
            .trim_end_matches('/')
            .to_owned(),
        fetch_delay: std::time::Duration::from_millis(
            env("FAKE_COMPOSIO_DELAY_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        ),
        webhook_url: env("FAKE_STRIPE_WEBHOOK_URL"),
        webhook_secret: env("FAKE_STRIPE_WEBHOOK_SECRET"),
        ..Fake::default()
    };
    let app = Router::new()
        .route("/toolkits", get(toolkits))
        .route("/connected_accounts/link", post(link))
        .route("/connected_accounts", get(accounts))
        .route("/connected_accounts/{id}", delete(delete_account))
        .route(
            "/trigger_instances/{slug}/upsert",
            post(|| async { Json(json!({"trigger_id": "ti_fake"})) }),
        )
        .route("/tools/execute/{tool}", post(execute))
        .route(
            "/connect/{id}",
            get(|| async { "Connected. You can close this tab." }),
        )
        .route("/v1/messages", post(messages))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/customers", post(customers))
        .route("/v1/customers/{id}", delete(delete_customer))
        .route("/v1/checkout/sessions", post(checkout))
        .route("/v1/billing_portal/sessions", post(portal))
        .route("/v1/subscriptions/{id}", get(subscription))
        .route("/v1/invoices/create_preview", post(invoice_preview))
        .route("/c/{id}", get(complete_checkout))
        .route(
            "/p/{id}",
            get(|| async { Body::from("Fake Stripe customer portal") }),
        )
        .route("/__fake/calls", get(fake_calls))
        .route("/__fake/reset", post(fake_reset))
        .with_state(Arc::new(fake));
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("fake providers on {bind}");
    axum::serve(listener, app).await.expect("serve");
}
