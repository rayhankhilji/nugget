use std::time::Duration;

use nugget_connector::{AuthKind, ConnectorFactory, Registry};
use serde_json::json;

use super::*;
use crate::{CalendarFactory, GmailFactory};

#[test]
fn registers_every_kind_with_oauth() {
    let mut r = Registry::new();
    crate::register(&mut r);
    let kinds = r.kinds();
    let names: Vec<_> = kinds.iter().map(|k| k.kind).collect();
    assert_eq!(names, vec!["gcal", "gdrive", "gmail"]);
    assert!(kinds.iter().all(|k| k.auth == AuthKind::OAuth));
    for k in &kinds {
        assert_eq!(k.settings_schema["required"], json!(["client_id"]));
        assert_eq!(k.settings_schema["additionalProperties"], json!(false));
    }
    assert!(kinds[2].settings_schema["properties"]["window_days"].is_object());
    assert!(kinds[1].settings_schema["properties"]["include_shared_drives"].is_object());
    assert!(kinds[0].settings_schema["properties"]["calendars"].is_object());
}

#[test]
fn validates_settings() {
    let g = GmailFactory::new();
    assert!(
        g.validate(&json!({"client_id": "x.apps.googleusercontent.com"}))
            .is_ok()
    );
    assert!(
        g.validate(&json!({"client_id": "x", "client_secret": "s", "account_hint": "a@b.c", "window_days": 30, "include_spam_trash": true}))
            .is_ok()
    );
    assert!(g.validate(&json!({})).is_err());
    assert!(g.validate(&Value::Null).is_err());
    assert!(g.validate(&json!({"client_id": "  "})).is_err());
    assert!(g.validate(&json!({"client_id": "a b"})).is_err());
    assert!(
        g.validate(&json!({"client_id": "x", "window_days": 0}))
            .is_err()
    );
    assert!(
        g.validate(&json!({"client_id": "x", "window_days": 99999}))
            .is_err()
    );
    assert!(g.validate(&json!({"client_id": "x", "bogus": 1})).is_err());
    assert!(
        g.validate(&json!({"client_id": "x", "calendars": ["a"]}))
            .is_err()
    );

    let c = CalendarFactory::new();
    assert!(
        c.validate(&json!({"client_id": "x", "calendars": ["primary"], "past_days": 0}))
            .is_ok()
    );
    assert!(
        c.validate(&json!({"client_id": "x", "calendars": ["a/b"]}))
            .is_err()
    );
    assert!(
        c.validate(&json!({"client_id": "x", "calendars": [""]}))
            .is_err()
    );
    assert!(
        c.validate(&json!({"client_id": "x", "window_days": 3}))
            .is_err()
    );
}

#[tokio::test]
async fn kinds_and_poll_intervals() {
    let h = Harness::new().await;
    let gmail = h.build(&GmailFactory::new(), json!({"client_id": "x"}));
    let gcal = h.build(&CalendarFactory::new(), json!({"client_id": "x"}));
    assert_eq!(gmail.kind(), "gmail");
    assert_eq!(gcal.kind(), "gcal");
    assert_eq!(gmail.poll_interval(), Duration::from_secs(60));
    assert_eq!(gcal.poll_interval(), Duration::from_secs(300));
    assert!(!gmail.supports_watch() && !gcal.supports_watch());
}

#[tokio::test]
async fn begin_auth_returns_google_consent_url_with_scope() {
    let h = Harness::new().await;
    for (factory, scope) in [
        (
            Box::new(GmailFactory::new()) as Box<dyn ConnectorFactory>,
            nugget_oauth::google::SCOPE_GMAIL_READONLY,
        ),
        (
            Box::new(CalendarFactory::new()),
            nugget_oauth::google::SCOPE_CALENDAR_READONLY,
        ),
    ] {
        let conn = h.build(
            factory.as_ref(),
            json!({"client_id": "cid", "client_secret": "sec", "account_hint": "me@example.com"}),
        );
        let flow = conn.begin_auth().await.unwrap();
        assert!(flow.url.starts_with(nugget_oauth::google::AUTH_URL));
        let q: HashMap<String, String> = url::Url::parse(&flow.url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(q["scope"], scope);
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["login_hint"], "me@example.com");
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(!q.contains_key("client_secret"));
        drop(flow);
    }
}

#[tokio::test]
async fn begin_auth_flow_completes_and_sync_uses_new_tokens() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, ResponseTemplate};
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh", "refresh_token": "r", "expires_in": 3600
        })))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/calendars/primary/events"))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "nextSyncToken": "st"
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = h.build(
        &CalendarFactory::new().with_endpoints(h.endpoints()),
        json!({"client_id": "cid", "calendars": ["primary"]}),
    );
    // Not connected yet.
    let sink = RecSink::default();
    assert!(is_unauthorized(&run(&conn, None, &sink).await));

    let flow = conn.begin_auth().await.unwrap();
    let q: HashMap<String, String> = url::Url::parse(&flow.url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let done = tokio::spawn(flow.done);
    let resp = reqwest::get(format!("{}?state={}&code=c", q["redirect_uri"], q["state"]))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    done.await.unwrap().unwrap();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(cursor["calendars"]["primary"]["sync_token"], "st");
    h.assert_egress("sync:gcal").await;
}
