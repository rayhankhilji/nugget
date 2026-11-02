//! Stand-ins for Composio, Anthropic and OpenAI, for self-hosting smoke tests without real keys.
//! Point the server at it with `COMPOSIO_BASE_URL`, `NUGGET_ANTHROPIC_BASE_URL` and
//! `NUGGET_OPENAI_BASE_URL`. Never use it in production: it accepts any key.
//!
//! ```sh
//! FAKE_PROVIDERS_BIND=0.0.0.0:9100 cargo run -p nugget-cloud --example fake_providers
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

#[derive(Default)]
struct Fake {
    /// user → (account id, toolkit)
    accounts: Mutex<HashMap<String, Vec<(String, String)>>>,
    next: Mutex<u32>,
}

type S = State<Arc<Fake>>;

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
    let id = {
        let mut n = f.next.lock().unwrap();
        *n += 1;
        format!("ca_fake{n}")
    };
    f.accounts
        .lock()
        .unwrap()
        .entry(user)
        .or_default()
        .push((id.clone(), toolkit.into()));
    Json(
        json!({"connected_account_id": id, "redirect_url": format!("https://connect.fake.invalid/{id}"),
                "link_token": "lt", "expires_at": "2099-01-01T00:00:00Z"}),
    )
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

async fn execute(Path(tool): Path<String>) -> Json<Value> {
    let data = match tool.as_str() {
        "GMAIL_FETCH_EMAILS" => json!({"messages": [
            {"messageId": "m1", "threadId": "t1", "messageTimestamp": "2026-10-30T09:15:00Z",
             "sender": "Landlord <ll@example.com>", "subject": "Lease renewal",
             "messageText": "Hi, your lease renews on 1 December. The rent stays the same."},
            {"messageId": "m2", "threadId": "t2", "messageTimestamp": "2026-10-29T10:00:00Z",
             "sender": "Shop", "subject": "Your order shipped", "messageText": "Parcel arrives Friday."}
        ]}),
        "GOOGLECALENDAR_EVENTS_LIST" => json!({"items": [
            {"id": "e1", "summary": "Dentist", "start": {"dateTime": "2026-11-05T10:00:00Z"},
             "end": {"dateTime": "2026-11-05T10:30:00Z"}}
        ]}),
        _ => json!({}),
    };
    Json(json!({"successful": true, "error": null, "data": data}))
}

fn answer(prompt: &str) -> String {
    if prompt.contains("<source n=\"1\"") {
        "Your lease renews on 1 December [1].".into()
    } else {
        "I couldn't find that in your mail or calendar.".into()
    }
}

async fn messages(Json(b): Json<Value>) -> Json<Value> {
    let prompt = b["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    Json(
        json!({"content": [{"type": "text", "text": answer(prompt)}], "usage": {"input_tokens": 1000, "output_tokens": 20}}),
    )
}

async fn chat(Json(b): Json<Value>) -> Json<Value> {
    let prompt = b["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    Json(
        json!({"choices": [{"message": {"content": answer(prompt)}}], "usage": {"prompt_tokens": 1000, "completion_tokens": 20}}),
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

#[tokio::main]
async fn main() {
    let bind = std::env::var("FAKE_PROVIDERS_BIND").unwrap_or_else(|_| "127.0.0.1:9100".into());
    let app = Router::new()
        .route("/toolkits", get(toolkits))
        .route("/connected_accounts/link", post(link))
        .route("/connected_accounts", get(accounts))
        .route("/connected_accounts/{id}", delete(delete_account))
        .route(
            "/trigger_instances/{slug}/upsert",
            post(|| async { Json(json!({})) }),
        )
        .route("/tools/execute/{tool}", post(execute))
        .route("/v1/messages", post(messages))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .with_state(Arc::new(Fake::default()));
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("fake providers on {bind}");
    axum::serve(listener, app).await.expect("serve");
}
