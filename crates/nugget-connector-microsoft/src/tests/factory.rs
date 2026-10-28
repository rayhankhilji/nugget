use std::time::Duration;

use nugget_connector::{AuthKind, ConnectorFactory, Registry};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::oauth;
use crate::{OneDriveFactory, OutlookCalendarFactory, OutlookFactory};

#[test]
fn registers_three_kinds_with_oauth() {
    let mut r = Registry::new();
    crate::register(&mut r);
    let kinds = r.kinds();
    let names: Vec<_> = kinds.iter().map(|k| k.kind).collect();
    assert_eq!(names, vec!["onedrive", "outlook", "outlook-calendar"]);
    assert!(kinds.iter().all(|k| k.auth == AuthKind::OAuth));
    for k in &kinds {
        let s = &k.settings_schema;
        assert_eq!(s["required"], json!(["client_id"]));
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(s["properties"]["tenant"]["default"], "common");
        assert!(s["properties"]["client_secret"].is_object());
    }
    assert_eq!(
        kinds[1].settings_schema["properties"]["folders"]["default"],
        json!(["inbox", "sentitems"])
    );
    assert!(kinds[1].settings_schema["properties"]["window_days"].is_object());
    assert!(kinds[2].settings_schema["properties"]["past_days"].is_object());
    assert!(kinds[0].settings_schema["properties"]["include_folders"].is_object());
    assert!(kinds[0].settings_schema["properties"]["max_file_mb"].is_object());
}

#[test]
fn validates_settings() {
    let o = OutlookFactory::new();
    assert!(
        o.validate(&json!({"client_id": "00000000-0000-0000-0000-000000000001"}))
            .is_ok()
    );
    assert!(
        o.validate(&json!({
            "client_id": "x", "client_secret": "s", "tenant": "contoso.onmicrosoft.com",
            "account_hint": "a@b.c", "folders": ["inbox", "archive", "AAMkAGI2=="], "window_days": 30
        }))
        .is_ok()
    );
    assert!(o.validate(&json!({})).is_err());
    assert!(o.validate(&Value::Null).is_err());
    assert!(o.validate(&json!({"client_id": "  "})).is_err());
    assert!(o.validate(&json!({"client_id": "a b"})).is_err());
    assert!(
        o.validate(&json!({"client_id": "x", "window_days": 0}))
            .is_err()
    );
    assert!(
        o.validate(&json!({"client_id": "x", "folders": ["a/b"]}))
            .is_err()
    );
    assert!(
        o.validate(&json!({"client_id": "x", "folders": [""]}))
            .is_err()
    );
    assert!(o.validate(&json!({"client_id": "x", "bogus": 1})).is_err());
    // The tenant ends up in the endpoint URL: anything but a name or id is refused.
    for bad in [
        "",
        "a/b",
        "common?x=1",
        "evil.com/..",
        "a b",
        "..",
        "-x",
        "x.",
    ] {
        assert!(
            o.validate(&json!({"client_id": "x", "tenant": bad}))
                .is_err(),
            "{bad}"
        );
    }
    for good in [
        "common",
        "organizations",
        "consumers",
        "72f988bf-86f1-41af-91ab-2d7cd011db47",
    ] {
        assert!(
            o.validate(&json!({"client_id": "x", "tenant": good}))
                .is_ok()
        );
    }

    let c = OutlookCalendarFactory::new();
    assert!(
        c.validate(&json!({"client_id": "x", "past_days": 0}))
            .is_ok()
    );
    assert!(
        c.validate(&json!({"client_id": "x", "past_days": 0, "future_days": 0}))
            .is_err()
    );
    assert!(
        c.validate(&json!({"client_id": "x", "folders": ["inbox"]}))
            .is_err()
    );

    let d = OneDriveFactory::new();
    assert!(
        d.validate(&json!({"client_id": "x", "include_folders": ["/Documents"], "exclude_folders": ["Documents/Private/"], "max_file_mb": 100}))
            .is_ok()
    );
    assert!(
        d.validate(&json!({"client_id": "x", "include_folders": ["/a/../b"]}))
            .is_err()
    );
    assert!(
        d.validate(&json!({"client_id": "x", "max_file_mb": 0}))
            .is_err()
    );
    assert!(
        d.validate(&json!({"client_id": "x", "max_file_mb": 100000}))
            .is_err()
    );
}

#[test]
fn microsoft_oauth_preset_urls() {
    let p = oauth::provider("common", oauth::ALL_SCOPES).unwrap();
    assert_eq!(p.name, "microsoft");
    assert_eq!(
        p.auth_url,
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
    );
    assert_eq!(
        p.token_url,
        "https://login.microsoftonline.com/common/oauth2/v2.0/token"
    );
    assert_eq!(
        p.scopes.join(" "),
        "offline_access Mail.Read Calendars.Read Files.Read User.Read"
    );
    assert!(
        p.extra_auth_params
            .contains(&("response_mode".to_string(), "query".to_string()))
    );
    let t = oauth::provider("contoso.onmicrosoft.com", &[oauth::SCOPE_MAIL_READ]).unwrap();
    assert_eq!(
        t.token_url,
        "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/token"
    );
    let gov = oauth::provider_with_login("https://login.microsoftonline.us/", "organizations", &[])
        .unwrap();
    assert_eq!(
        gov.auth_url,
        "https://login.microsoftonline.us/organizations/oauth2/v2.0/authorize"
    );
    assert!(oauth::provider("evil.com/x?", &[]).is_err());
}

#[tokio::test]
async fn kinds_and_poll_intervals() {
    let h = Harness::new().await;
    let mail = h.build(&OutlookFactory::new(), json!({"client_id": "x"}));
    let cal = h.build(&OutlookCalendarFactory::new(), json!({"client_id": "x"}));
    let drive = h.build(&OneDriveFactory::new(), json!({"client_id": "x"}));
    assert_eq!(mail.kind(), "outlook");
    assert_eq!(cal.kind(), "outlook-calendar");
    assert_eq!(drive.kind(), "onedrive");
    assert_eq!(mail.poll_interval(), Duration::from_secs(60));
    assert_eq!(cal.poll_interval(), Duration::from_secs(300));
    assert_eq!(drive.poll_interval(), Duration::from_secs(300));
    assert!(!mail.supports_watch() && !cal.supports_watch() && !drive.supports_watch());
}

#[tokio::test]
async fn begin_auth_returns_microsoft_consent_url_with_scopes() {
    let h = Harness::new().await;
    for (factory, scope) in [
        (
            Box::new(OutlookFactory::new()) as Box<dyn ConnectorFactory>,
            "Mail.Read",
        ),
        (Box::new(OutlookCalendarFactory::new()), "Calendars.Read"),
        (Box::new(OneDriveFactory::new()), "Files.Read"),
    ] {
        let conn = h.build(
            factory.as_ref(),
            json!({"client_id": "cid", "client_secret": "sec", "tenant": "organizations", "account_hint": "me@contoso.com"}),
        );
        let flow = conn.begin_auth().await.unwrap();
        assert!(
            flow.url.starts_with(
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?"
            ),
            "{}",
            flow.url
        );
        let q: HashMap<String, String> = url::Url::parse(&flow.url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(q["scope"], format!("offline_access User.Read {scope}"));
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["login_hint"], "me@contoso.com");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["prompt"], "select_account");
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
        assert!(!q.contains_key("client_secret"));
        drop(flow);
    }
}

#[tokio::test]
async fn auth_flow_as_public_client_then_sync_uses_new_tokens() {
    let h = Harness::new().await;
    // Public client: no client_secret in the token request.
    Mock::given(method("POST"))
        .and(path("/contoso.onmicrosoft.com/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("client_id=cid"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "scope": "Calendars.Read User.Read",
            "access_token": "fresh", "refresh_token": "r", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/calendarView/delta")))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [], "@odata.deltaLink": format!("{}{API}/me/calendarView/delta?$deltatoken=d1", h.uri())
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = h.build(
        &OutlookCalendarFactory::new().with_endpoints(h.endpoints()),
        json!({"client_id": "cid", "tenant": "contoso.onmicrosoft.com"}),
    );
    let sink = RecSink::default();
    assert!(is_unauthorized(&run(&conn, None, &sink).await));

    let flow = conn.begin_auth().await.unwrap();
    assert!(flow.url.starts_with(&format!(
        "{}/contoso.onmicrosoft.com/oauth2/v2.0/authorize?",
        h.uri()
    )));
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
    let token_req = &h
        .requests_to("/contoso.onmicrosoft.com/oauth2/v2.0/token")
        .await[0];
    assert!(!String::from_utf8_lossy(&token_req.body).contains("client_secret"));

    let cursor = run(&conn, None, &sink).await.unwrap();
    assert!(
        cursor["delta_link"]
            .as_str()
            .unwrap()
            .ends_with("$deltatoken=d1")
    );
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn expired_token_is_refreshed_with_client_secret_when_given() {
    let h = Harness::new().await;
    h.seed_tokens("old", true);
    Mock::given(method("POST"))
        .and(path("/common/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("client_secret=sec"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new", "refresh_token": "rt2", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/calendarView/delta")))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [], "@odata.deltaLink": format!("{}{API}/me/calendarView/delta?$deltatoken=d", h.uri())
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = h.build(
        &OutlookCalendarFactory::new().with_endpoints(h.endpoints()),
        json!({"client_id": "cid", "client_secret": "sec"}),
    );
    run(&conn, None, &RecSink::default()).await.unwrap();
    // The rotated refresh token is stored.
    let stored = h.secrets.get(&oauth_account(SOURCE)).unwrap().unwrap();
    assert!(stored.contains("rt2"));
}
