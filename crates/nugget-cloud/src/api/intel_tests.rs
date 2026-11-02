//! M2/M3 over the router with a real Postgres and test doubles (auth, LLM, embeddings): knowledge
//! extraction into entities and claims, cards with accept/dismiss/snooze, the cached brief, search
//! with facets, the LLM cache, the sensitive-item gate and per-user isolation.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::composio::Composio;
use crate::db::NewItem;
use crate::gateway::{Embedder, GatewayError, Llm, LlmRequest, LlmResponse};
use crate::ingest::{chunk, content_hash, indexed_text};
use crate::plans::Catalog;
use crate::testkit::{FakeEmbedder, add_user, fakes, subscriber, test_db, test_prices};

fn catalog() -> Arc<Catalog> {
    Arc::new(Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap())
}

/// Answers extraction prompts with a fixed graph for Dana's mail, brief prompts with prose
/// citing entry 1, and records every prompt.
#[derive(Default)]
struct ScriptLlm {
    calls: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Llm for ScriptLlm {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError> {
        let prompt = req.messages.last().map(|m| m.1.clone()).unwrap_or_default();
        self.calls
            .lock()
            .unwrap()
            .push((req.system.clone(), prompt.clone()));
        let text = if req.system.contains("knowledge graph") {
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
        } else if req.system.starts_with("You draft a short email reply") {
            "Hi Dana, the signed contract is attached. Best, Me".into()
        } else if req.system.starts_with("You write the user's morning brief") {
            "Dana is waiting on the signed contract [1].".into()
        } else {
            "ok".into()
        };
        Ok(LlmResponse {
            text,
            input_tokens: 100,
            output_tokens: 20,
        })
    }
}

impl ScriptLlm {
    fn count(&self, needle: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s.contains(needle))
            .count()
    }
    fn saw(&self, text: &str) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|(s, p)| s.contains(text) || p.contains(text))
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Uuid,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer tok-{user}"));
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn rfc(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn mail(
    id: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    at: OffsetDateTime,
    labels: &[&str],
) -> NewItem {
    NewItem {
        source: "gmail".into(),
        external_id: id.into(),
        kind: "email",
        title: Some(subject.into()),
        author: Some(from.into()),
        url: Some(format!("https://mail.google.com/mail/u/0/#all/{id}")),
        occurred_at: Some(at),
        body: body.into(),
        metadata: json!({"thread_id": format!("t-{id}"), "to": to, "labels": labels}),
    }
}

async fn seed(db: &crate::db::Db, user: Uuid, now: OffsetDateTime) -> Vec<NewItem> {
    let tomorrow = now + Duration::days(1);
    let start = rfc(tomorrow);
    let end = rfc(tomorrow + Duration::hours(1));
    let items = vec![
        mail(
            "s1",
            "Me <me@me.example>",
            "pat@other.example",
            "Hello",
            "Thanks Pat, talk soon.",
            now - Duration::days(10),
            &["SENT"],
        ),
        mail(
            "d1",
            "Dana Okoro <dana@harbourline.example>",
            "me@me.example",
            "Signed contract",
            "Hi, I'm Dana Okoro and I work at Harbourline. Could you send the signed contract by Friday?",
            now - Duration::days(3),
            &["INBOX"],
        ),
        mail(
            "c1",
            "Reception <reception@northside-clinic.example>",
            "me@me.example",
            "Your therapy appointment",
            "Your therapy session is confirmed. Can you confirm by reply?",
            now - Duration::days(2),
            &["INBOX"],
        ),
        NewItem {
            source: "googlecalendar".into(),
            external_id: "e1".into(),
            kind: "event",
            title: Some("Harbourline kickoff".into()),
            author: Some("me@me.example".into()),
            url: None,
            occurred_at: Some(tomorrow),
            body: "Kickoff with Dana".into(),
            metadata: json!({
                "event_id": "e1", "status": "confirmed",
                "start": {"date_time": start}, "end": {"date_time": end},
                "organizer": {"email": "me@me.example"},
                "attendees": [{"email": "me@me.example", "response": "accepted"},
                              {"email": "dana@harbourline.example", "name": "Dana Okoro", "response": "accepted"}],
            }),
        },
    ];
    for it in &items {
        let texts = chunk(&indexed_text(it), 1200, 200);
        let emb = FakeEmbedder.embed(&texts).await.unwrap().0;
        let chunks: Vec<_> = texts.into_iter().zip(emb.into_iter().map(Some)).collect();
        db.upsert_item(user, None, it, &content_hash(it), &chunks)
            .await
            .unwrap();
    }
    items
}

/// A zone where it is between 07:00 and 23:59 now, so the brief is due.
fn zone_after_seven(now: OffsetDateTime) -> &'static str {
    [
        "UTC",
        "Asia/Tokyo",
        "Pacific/Kiritimati",
        "America/New_York",
        "Pacific/Honolulu",
    ]
    .into_iter()
    .find(|z| now.to_offset(crate::intel::utc_offset(Some(z), now)).hour() >= 7)
    .unwrap()
}

#[tokio::test]
async fn knowledge_cards_brief_and_search() {
    let Some((db, pool)) = test_db().await else {
        return;
    };
    let (auth, _composio, _) = fakes();
    let llm = Arc::new(ScriptLlm::default());
    let gateway = Gateway::new(
        Some(llm.clone() as Arc<dyn Llm>),
        Some(Arc::new(FakeEmbedder) as Arc<dyn Embedder>),
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let state = AppState::new(db.clone(), auth, gateway, catalog());
    let app = router(Arc::new(state));
    let (a, b) = (
        subscriber(&pool, "heed").await,
        subscriber(&pool, "heed").await,
    );
    let now = OffsetDateTime::now_utc();
    seed(&db, a, now).await;

    // Without a live subscription every intelligence endpoint answers 402.
    let lapsed = add_user(&pool).await;
    for (method, uri, body) in [
        ("GET", "/v1/cards", None),
        ("POST", "/v1/cards/refresh", Some(json!({}))),
        ("POST", "/v1/cards/seen", Some(json!({"ids": []}))),
        ("POST", "/v1/cards/abc/accept", None),
        ("GET", "/v1/brief", None),
        ("POST", "/v1/search", Some(json!({"q": "contract"}))),
    ] {
        let (s, v) = call(&app, method, uri, lapsed, body).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{method} {uri}: {v}");
        assert_eq!(v["error"], "subscription_required");
    }

    // Before any run: an empty inbox; a bad zone is refused.
    let (s, v) = call(&app, "GET", "/v1/cards?tz=Europe/London", a, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["cards"], json!([]));
    let (s, _) = call(&app, "GET", "/v1/cards?tz=Mars/Base", a, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Knowledge and cards.
    let (s, v) = call(&app, "POST", "/v1/cards/refresh", a, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["report"]["items"], 4, "{v}");
    let extractions = llm.count("knowledge graph");
    assert!(extractions >= 1, "extraction ran on the gateway");
    assert!(
        !llm.saw("therapy"),
        "a confirmed-sensitive item never reaches the model"
    );
    let rows = pool
        .query(
            "select e.kind, e.name, e.canonical from public.entities e where e.user_id = $1 order by e.name",
            &[&a],
        )
        .await
        .unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.get(1)).collect();
    assert!(names.iter().any(|n| n == "Dana Okoro"), "{names:?}");
    assert!(names.iter().any(|n| n == "Harbourline"), "{names:?}");
    let works_at = pool
        .query_one(
            "select c.evidence, i.external_id from public.claims c
             join public.entities s on s.id = c.subject_id join public.items i on i.id = c.item_id
             where c.user_id = $1 and c.predicate = 'works_at' and s.name = 'Dana Okoro'",
            &[&a],
        )
        .await
        .unwrap();
    assert_eq!(works_at.get::<_, String>(1), "d1", "claims cite their item");
    assert!(
        works_at
            .get::<_, Value>(0)
            .to_string()
            .contains("I work at Harbourline"),
        "evidence keeps the quote"
    );
    let dana_id: Uuid = pool
        .query_one(
            "select id from public.entities where user_id = $1 and name = 'Dana Okoro'",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);

    let cards = v["cards"].as_array().unwrap().clone();
    let kinds: Vec<&str> = cards.iter().filter_map(|c| c["kind"].as_str()).collect();
    assert!(kinds.contains(&"unanswered_thread"), "{v}");
    assert!(kinds.contains(&"meeting_prep"), "{v}");
    let unanswered = cards
        .iter()
        .find(|c| {
            c["kind"] == "unanswered_thread" && c["title"].as_str().unwrap_or("").contains("Dana")
        })
        .unwrap_or_else(|| panic!("Dana's card: {v}"));
    assert!(
        unanswered["title_text"]["key"].is_string(),
        "LocalText for the desktop: {unanswered}"
    );
    let sensitive: Vec<&Value> = cards.iter().filter(|c| c["sensitive"] == true).collect();
    assert!(
        !sensitive.is_empty(),
        "the clinic mail is a sensitive card: {v}"
    );
    for c in sensitive {
        assert!(
            c["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["quote"].is_null()),
            "sensitive cards are never quoted: {c}"
        );
    }
    let projected: i64 = pool
        .query_one(
            "select count(*) from public.cards where user_id = $1",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(projected as usize, cards.len().max(projected as usize));
    assert!(projected >= 2);

    // A second run re-reads every item but pays for none of them twice; entity ids are stable.
    let (s, v2) = call(&app, "POST", "/v1/cards/refresh", a, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v2}");
    assert_eq!(
        llm.count("knowledge graph"),
        extractions,
        "cached extractions"
    );
    assert!(v2["report"]["llm"]["cached"].as_u64().unwrap() >= 1, "{v2}");
    let dana_again: Uuid = pool
        .query_one(
            "select id from public.entities where user_id = $1 and name = 'Dana Okoro'",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(dana_id, dana_again);

    // The brief: generated once per local day, prose with citations, cached.
    let tz = zone_after_seven(now);
    let (s, v) = call(&app, "GET", &format!("/v1/brief?tz={tz}"), a, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let brief = &v["brief"];
    assert_eq!(brief["timezone"], tz);
    assert!(brief["prose"].as_str().unwrap().contains("[1]"), "{v}");
    assert_eq!(brief["citations"][0]["n"], 1, "{v}");
    assert!(brief["brief"]["top"].is_array());
    let briefs_written = llm.count("morning brief");
    assert_eq!(briefs_written, 1);
    let (_, again) = call(&app, "GET", "/v1/brief", a, None).await;
    assert_eq!(again["brief"]["created_at"], brief["created_at"]);
    assert_eq!(llm.count("morning brief"), 1, "generated once");
    assert!(!llm.saw("therapy"));

    // Accept, snooze and dismiss (with a reason), then the inbox no longer shows them.
    let id_of = |kind: &str| {
        cards
            .iter()
            .find(|c| c["kind"] == kind)
            .and_then(|c| c["id"].as_str())
            .unwrap()
            .to_owned()
    };
    let (unans, meeting) = (id_of("unanswered_thread"), id_of("meeting_prep"));
    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/cards/{unans}/dismiss"),
        a,
        Some(json!({"reason": "bogus"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/cards/{unans}/dismiss"),
        a,
        Some(json!({"reason": "already_handled"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["card"]["state"], "dismissed");
    let until = rfc(now + Duration::days(2));
    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/cards/{meeting}/snooze"),
        a,
        Some(json!({"until": until})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["card"]["state"], "snoozed");
    let (s, _) = call(&app, "POST", "/v1/cards/nope/accept", a, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, v) = call(&app, "GET", "/v1/cards", a, None).await;
    let open: Vec<&str> = v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert!(
        !open.contains(&unans.as_str()) && !open.contains(&meeting.as_str()),
        "{v}"
    );
    let status: String = pool
        .query_one(
            "select status from public.cards where user_id = $1 and card_key = $2",
            &[&a, &unans],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "dismissed");
    // A refresh keeps the dismissal (the card is a tombstone) and the learned feedback.
    let (_, v) = call(&app, "POST", "/v1/cards/refresh", a, Some(json!({}))).await;
    assert!(
        !v["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == unans.as_str()),
        "{v}"
    );
    let state: Value = pool
        .query_one(
            "select state from public.proactive_state where user_id = $1",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        state["feedback_log"].as_array().unwrap().len(),
        2,
        "dismiss and snooze teach"
    );
    assert_eq!(state["feedback_log"][0]["reason"], "already_handled");

    // Search: hybrid with facets and a timeline; filters; browse.
    let (s, v) = call(
        &app,
        "POST",
        "/v1/search",
        a,
        Some(json!({"q": "signed contract"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mode"], "hybrid");
    assert_eq!(v["results"][0]["title"], "Signed contract", "{v}");
    assert_eq!(
        v["facets"]["sources"][0],
        json!({"value": "gmail", "count": v["total"]})
    );
    assert!(!v["timeline"]["points"].as_array().unwrap().is_empty());
    let (_, v) = call(
        &app,
        "POST",
        "/v1/search",
        a,
        Some(json!({"q": "", "sources": ["googlecalendar"]})),
    )
    .await;
    assert_eq!(v["mode"], "browse");
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["results"][0]["title"], "Harbourline kickoff");
    let (_, v) = call(
        &app,
        "POST",
        "/v1/search",
        a,
        Some(json!({"q": "", "person": "dana"})),
    )
    .await;
    let titles: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"Signed contract") && !titles.contains(&"Hello"),
        "{v}"
    );
    let (_, v) = call(
        &app,
        "POST",
        "/v1/search",
        a,
        Some(json!({"q": "contract", "from": rfc(now - Duration::days(1)), "to": rfc(now)})),
    )
    .await;
    assert_eq!(v["total"], 0, "date filter: {v}");
    assert_eq!(v["timeline"]["bucket"], "day");
    let (s, _) = call(
        &app,
        "POST",
        "/v1/search",
        a,
        Some(json!({"q": "x", "from": rfc(now), "to": rfc(now - Duration::days(1))})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Another user sees none of it.
    let (_, v) = call(&app, "GET", "/v1/cards", b, None).await;
    assert_eq!(v["cards"], json!([]));
    let (_, v) = call(
        &app,
        "POST",
        "/v1/search",
        b,
        Some(json!({"q": "contract"})),
    )
    .await;
    assert_eq!(v["results"], json!([]));
    assert_eq!(v["total"], 0);
    let (_, v) = call(&app, "GET", "/v1/brief", b, None).await;
    assert!(v["brief"].is_null() || v["brief"]["prose"].is_null(), "{v}");
    let (s, _) = call(&app, "POST", &format!("/v1/cards/{unans}/accept"), b, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Export carries the brief and cards.
    let (_, v) = call(&app, "GET", "/v1/export", a, None).await;
    assert_eq!(v["briefs"].as_array().unwrap().len(), 1);
    for k in [
        "proactive_state",
        "intel_runs",
        "llm_cache",
        "entities",
        "claims",
    ] {
        assert!(!v[k].as_array().unwrap().is_empty(), "export has {k}");
    }
    let foreign: i64 = pool
        .query_one(
            "select count(*) from public.cards c, unnest(c.item_ids) as x(id)
             where c.user_id = $1 and not exists (select 1 from public.items i where i.id = x.id and i.user_id = $1)",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(foreign, 0, "cards cite only the user's own items");
    assert!(!v["cards"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn scheduler_runs_changed_users_and_skips_quota_spent_extraction() {
    let Some((db, pool)) = test_db().await else {
        return;
    };
    let llm = Arc::new(ScriptLlm::default());
    let gateway = Gateway::new(
        Some(llm.clone() as Arc<dyn Llm>),
        None,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let intel = crate::intel::Intel {
        db: db.clone(),
        gateway,
        plans: catalog(),
    };
    let u = subscriber(&pool, "heed").await;
    let lapsed = add_user(&pool).await;
    let now = OffsetDateTime::now_utc();
    seed(&db, u, now).await;
    seed(&db, lapsed, now).await;
    // Past the plan's hard cap: extraction is skipped, deterministic knowledge and cards still run.
    db.record_usage(u, crate::db::UsageKind::Llm, "x", 0, 0, 100_000_000)
        .await
        .unwrap();
    intel.tick().await;
    assert_eq!(llm.count("knowledge graph"), 0);
    let lapsed_runs: i64 = pool
        .query_one(
            "select count(*) from public.intel_runs where user_id = $1",
            &[&lapsed],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(lapsed_runs, 0, "no jobs for a user without a subscription");
    let ran: i64 = pool
        .query_one(
            "select count(*) from public.intel_runs where user_id = $1 and last_error is null",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(ran, 1);
    let people: i64 = pool
        .query_one(
            "select count(*) from public.entities where user_id = $1 and kind = 'person'",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert!(people >= 2, "people from mail headers without a model");
    // Nothing changed: the next tick leaves the user alone.
    let before: OffsetDateTime = pool
        .query_one(
            "select ran_at from public.intel_runs where user_id = $1",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    intel.tick().await;
    let after: OffsetDateTime = pool
        .query_one(
            "select ran_at from public.intel_runs where user_id = $1",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
}

/// "Awaiting your reply" on a Gmail-connected user: the card offers its reply, accepting it
/// proposes a `reply_mail` through the actions path (drafted by the fast model), the preview
/// shows it, and only an approval with the preview's digest sends it, once.
#[tokio::test]
async fn awaiting_reply_card_proposes_previews_and_sends_only_on_approval() {
    let Some((db, pool)) = test_db().await else {
        return;
    };
    let (auth, composio, _) = fakes();
    let llm = Arc::new(ScriptLlm::default());
    let gateway = Gateway::new(
        Some(llm.clone() as Arc<dyn Llm>),
        None,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let mut state = AppState::new(db.clone(), auth, gateway, catalog());
    state.composio = Some(composio.clone() as Arc<dyn Composio>);
    let app = router(Arc::new(state));
    let u = subscriber(&pool, "heed").await;
    let now = OffsetDateTime::now_utc();
    seed(&db, u, now).await;
    let (_, v) = call(&app, "POST", "/v1/cards/refresh", u, Some(json!({}))).await;
    let card = v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["kind"] == "unanswered_thread" && c["title"].as_str().unwrap_or("").contains("Dana")
        })
        .unwrap_or_else(|| panic!("Dana's card: {v}"))
        .clone();
    let id = card["id"].as_str().unwrap().to_owned();
    assert_eq!(card["actions"][0]["kind"], "draft_reply", "{card}");
    assert_eq!(
        card["actions"][0]["supported"], false,
        "Gmail is not connected yet"
    );

    // Not connected: accepting asks to connect, and the card is left as it was.
    let (s, e) = call(
        &app,
        "POST",
        &format!("/v1/cards/{id}/accept"),
        u,
        Some(json!({"action": 0})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("not_connected")),
        "{e}"
    );

    let account = format!("ca_{}_gmail", &u.to_string()[..8]);
    db.upsert_connection(u, "gmail", &account, "active")
        .await
        .unwrap();
    let (_, v) = call(&app, "GET", "/v1/cards", u, None).await;
    let listed = v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["actions"][0]["supported"], true, "{listed}");

    // Accept: the card is accepted and its reply proposed, pending approval.
    let (s, v) = call(&app, "POST", &format!("/v1/cards/{id}/accept"), u, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["card"]["state"], "accepted");
    let action = &v["action"];
    assert_eq!(action["kind"], "reply_mail");
    assert_eq!(action["toolkit"], "gmail");
    assert_eq!(action["status"], "pending");
    assert_eq!(action["decision"], "confirm");
    assert_eq!(
        llm.count("You draft a short email reply"),
        1,
        "drafted by the fast model"
    );
    let aid = action["id"].as_str().unwrap().to_owned();
    let digest = action["digest"].as_str().unwrap().to_owned();
    let sent = |tool: &str| -> Vec<Value> {
        composio
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.1 == tool)
            .map(|c| c.2.clone())
            .collect()
    };
    assert!(
        sent("GMAIL_REPLY_TO_THREAD").is_empty(),
        "nothing sent on accept"
    );

    // Accepting again returns the same pending action (idempotent per card and action).
    let (_, again) = call(&app, "POST", &format!("/v1/cards/{id}/accept"), u, None).await;
    assert_eq!(again["action"]["id"], aid.as_str());

    // Preview: the dry run the user reads, same digest, still nothing sent.
    let (s, p) = call(&app, "GET", &format!("/v1/actions/{aid}/preview"), u, None).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["digest"], digest.as_str());
    assert!(
        p["preview"]
            .to_string()
            .contains("the signed contract is attached"),
        "{p}"
    );
    assert!(sent("GMAIL_REPLY_TO_THREAD").is_empty());

    // A wrong digest is refused; the preview's digest sends it once.
    let (s, e) = call(
        &app,
        "POST",
        &format!("/v1/actions/{aid}/approve"),
        u,
        Some(json!({"digest": "0".repeat(64)})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::PRECONDITION_FAILED, Some("digest_mismatch"))
    );
    let (s, done) = call(
        &app,
        "POST",
        &format!("/v1/actions/{aid}/approve"),
        u,
        Some(json!({"digest": digest})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "succeeded");
    let calls = sent("GMAIL_REPLY_TO_THREAD");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["thread_id"], "t-d1", "{}", calls[0]);
    assert!(
        calls[0].to_string().contains("dana@harbourline.example"),
        "{}",
        calls[0]
    );

    // The sensitive clinic card is never drafted by the model: the user writes the text.
    let (_, v) = call(&app, "GET", "/v1/cards", u, None).await;
    if let Some(c) = v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["sensitive"] == true && c["kind"] == "unanswered_thread")
    {
        let cid = c["id"].as_str().unwrap();
        let (s, e) = call(&app, "POST", &format!("/v1/cards/{cid}/accept"), u, None).await;
        assert_eq!(
            (s, e["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("body_required")),
            "{e}"
        );
        let (s, ok) = call(
            &app,
            "POST",
            &format!("/v1/cards/{cid}/accept"),
            u,
            Some(json!({"body": "Confirmed, thanks."})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{ok}");
        assert_eq!(ok["action"]["status"], "pending");
    }
    assert!(!llm.saw("therapy"));
    assert_eq!(llm.count("You draft a short email reply"), 1);
}
