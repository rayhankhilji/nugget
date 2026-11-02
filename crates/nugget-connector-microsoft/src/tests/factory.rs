use nugget_connector::ConnectorFactory;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::oauth;
use crate::{OneDriveFactory, OutlookCalendarFactory, OutlookFactory};

#[test]
fn validates_settings() {
    // No built-in client (as in a source build): `client_id` is then required.
    let ep = || MicrosoftEndpoints::with_base("http://127.0.0.1:9");
    let (o, c, d) = (
        OutlookFactory::new().with_endpoints(ep()),
        OutlookCalendarFactory::new().with_endpoints(ep()),
        OneDriveFactory::new().with_endpoints(ep()),
    );
    let fs: [&dyn ConnectorFactory; 3] = [&o, &c, &d];
    let ok = [
        (
            0,
            json!({"client_id": "x", "client_secret": "s", "tenant": "contoso.onmicrosoft.com",
            "account_hint": "a@b.c", "folders": ["inbox", "AAMkAGI2=="], "window_days": 30}),
        ),
        (1, json!({"client_id": "x", "past_days": 0})),
        (
            2,
            json!({"client_id": "x", "include_folders": ["/Documents"], "max_file_mb": 100}),
        ),
    ];
    for (i, v) in ok {
        assert!(fs[i].validate(&v).is_ok(), "{v}");
    }
    let bad = [
        (0, json!({})),
        (0, Value::Null),
        (0, json!({"client_id": "a b"})),
        (0, json!({"client_id": "x", "window_days": 0})),
        (0, json!({"client_id": "x", "folders": ["a/b"]})),
        (0, json!({"client_id": "x", "bogus": 1})),
        (
            1,
            json!({"client_id": "x", "past_days": 0, "future_days": 0}),
        ),
        (1, json!({"client_id": "x", "folders": ["inbox"]})),
        (2, json!({"client_id": "x", "include_folders": ["/a/../b"]})),
        (2, json!({"client_id": "x", "max_file_mb": 0})),
    ];
    for (i, v) in bad {
        assert!(fs[i].validate(&v).is_err(), "{v}");
    }
    // The tenant ends up in the endpoint URL: anything but a name or id is refused.
    for bad in ["", "a/b", "common?x=1", "evil.com/..", "..", "-x", "x."] {
        assert!(
            o.validate(&json!({"client_id": "x", "tenant": bad}))
                .is_err(),
            "{bad}"
        );
        assert!(oauth::provider(bad, &[]).is_err(), "{bad}");
    }
    for good in ["organizations", "72f988bf-86f1-41af-91ab-2d7cd011db47"] {
        assert!(
            o.validate(&json!({"client_id": "x", "tenant": good}))
                .is_ok()
        );
    }
}

#[tokio::test]
async fn builtin_client_connects_without_settings_and_sign_out_forgets_tokens() {
    let h = Harness::new().await;
    let mut ep = h.endpoints();
    ep.builtin_client = Some(nugget_oauth::ClientCredentials::new("nugget-app", None));
    let f = OutlookFactory::new().with_endpoints(ep);
    assert!(f.validate(&json!({})).is_ok());
    let conn = h.build(&f, json!({}));
    let flow = conn.begin_auth().await.unwrap();
    assert!(flow.url.contains("client_id=nugget-app"), "{}", flow.url);
    drop(flow);
    let conn = h.build(&f, json!({"client_id": "mine"}));
    assert!(
        conn.begin_auth()
            .await
            .unwrap()
            .url
            .contains("client_id=mine")
    );
    h.seed_tokens("at", false);
    conn.sign_out().await.unwrap();
    assert!(
        h.ctx()
            .secrets
            .get(&nugget_core::secrets::oauth_account(
                h.ctx().source.as_str()
            ))
            .unwrap()
            .is_none()
    );
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
    assert_eq!(q["scope"], "offline_access User.Read Calendars.Read");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["prompt"], "select_account");
    assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(!q.contains_key("client_secret"));
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
