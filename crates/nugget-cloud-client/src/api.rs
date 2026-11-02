//! Typed client for the cloud service's `/v1/*` endpoints. Every call carries the session's
//! access token; errors come back as `{error: "<code>", message}`.

use std::sync::Arc;

use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::session::Session;
use crate::{CloudError, Result};

mod intel;
pub use intel::{
    Brief, Card, CardAction, CardEvidence, Cards, DismissReason, FacetCount, Facets, SearchDates,
    SearchRequest, SearchResponse, SearchResult, Timeline, TimelineBucket,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub month_cost_micros: i64,
    pub quota_micros: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user_id: String,
    pub email: Option<String>,
    pub plan: String,
    pub embeddings: bool,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub toolkit: String,
    pub status: String,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableApp {
    pub slug: String,
    pub name: String,
    pub logo: Option<String>,
    pub connectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connections {
    pub connections: Vec<Connection>,
    pub available: Vec<AvailableApp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectStart {
    pub redirect_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResult {
    pub toolkit: String,
    pub added: u64,
    pub updated: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub synced: Vec<SyncResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub n: u32,
    pub item_id: String,
    pub title: Option<String>,
    pub source: String,
    pub url: Option<String>,
    pub occurred_at: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// An answer with `[n]` markers that refer to `citations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub conversation_id: String,
    pub answer: String,
    pub citations: Vec<Citation>,
    pub usage: AskUsage,
}

#[derive(Clone)]
pub struct Api {
    session: Arc<Session>,
}

impl Api {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let token = self.session.access_token().await?;
        let url = format!("{}/v1/{path}", self.session.config().api_base);
        let mut req = self.session.http().request(method, url).bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(CloudError::network)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
        let code = text("error").unwrap_or_else(|| format!("http_{}", status.as_u16()));
        let message = text("message").unwrap_or_else(|| status.to_string());
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Revoked or expired early: the next call refreshes (or reports signed out).
            self.session.invalidate_access().await;
            return Err(CloudError::NotSignedIn);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS && code == "quota_exceeded" {
            return Err(CloudError::QuotaExceeded(message));
        }
        Err(CloudError::Api {
            status: status.as_u16(),
            code,
            message,
        })
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T> {
        self.send(method, path, body)
            .await?
            .json()
            .await
            .map_err(|e| CloudError::Protocol(format!("{path}: {}", e.without_url())))
    }

    /// `GET /v1/me`.
    pub async fn me(&self) -> Result<Me> {
        self.json(Method::GET, "me", None).await
    }

    /// `GET /v1/connections`: the user's connected accounts and the apps they may connect.
    pub async fn connections(&self) -> Result<Connections> {
        self.json(Method::GET, "connections", None).await
    }

    /// `POST /v1/connections`: the Composio consent page to open in the browser.
    pub async fn connect(&self, toolkit: &str) -> Result<ConnectStart> {
        self.json(
            Method::POST,
            "connections",
            Some(json!({ "toolkit": toolkit })),
        )
        .await
    }

    /// `DELETE /v1/connections/{id}`.
    pub async fn disconnect(&self, id: &str) -> Result<()> {
        let id = path_segment(id)?;
        self.send(Method::DELETE, &format!("connections/{id}"), None)
            .await
            .map(drop)
    }

    /// `POST /v1/sync`: sync every connection now.
    pub async fn sync(&self) -> Result<SyncReport> {
        self.json(Method::POST, "sync", Some(json!({}))).await
    }

    /// `POST /v1/ask`.
    pub async fn ask(&self, request: &AskRequest) -> Result<Answer> {
        let body = serde_json::to_value(request).map_err(|e| CloudError::Invalid(e.to_string()))?;
        self.json(Method::POST, "ask", Some(body)).await
    }

    /// `GET /v1/export`: all of the user's data as JSON.
    pub async fn export(&self) -> Result<Value> {
        self.json(Method::GET, "export", None).await
    }

    /// `DELETE /v1/me`: delete the account; the local session is forgotten too.
    pub async fn delete_account(&self) -> Result<()> {
        self.send(Method::DELETE, "me", None).await?;
        self.session.forget().await;
        Ok(())
    }
}

/// Ids go into a path: only characters that need no escaping.
fn path_segment(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && id != "."
        && id != "..";
    if ok {
        Ok(id)
    } else {
        Err(CloudError::Invalid("not a valid connection id".into()))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::auth::tests::token_body;
    use crate::session::tests::{FakeClock, session};
    use crate::session::{MemoryStore, StoredSession};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(super) async fn signed_in(server: &MockServer) -> Api {
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("AT", "rt2", 10_000)))
            .mount(server)
            .await;
        let store = Arc::new(MemoryStore::with(StoredSession {
            refresh_token: "rt".into(),
            email: None,
        }));
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        Api::new(Arc::new(session(&server.uri(), store, clock)))
    }

    fn err(status: u16, code: &str, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({ "error": code, "message": message }))
    }

    #[tokio::test]
    async fn calls_carry_the_bearer_token_and_parse() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/ask"))
            .and(header("authorization", "Bearer AT"))
            .and(body_json(json!({ "question": "When is my flight?" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversation_id": "c1",
                "answer": "Tuesday at 9 [1].",
                "citations": [{ "n": 1, "item_id": "i1", "title": "Booking", "source": "gmail",
                                "url": null, "occurred_at": "2026-10-30T09:00:00Z", "snippet": "Flight BA1" }],
                "usage": { "input_tokens": 10, "output_tokens": 5 }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/connections"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "connections": [{ "id": "k1", "toolkit": "gmail", "status": "ACTIVE",
                                  "last_synced_at": null, "last_error": null }],
                "available": [{ "slug": "gmail", "name": "Gmail", "logo": null, "connectable": true },
                              { "slug": "slack", "name": "Slack", "logo": null, "connectable": false }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/connections"))
            .and(body_json(json!({ "toolkit": "googlecalendar" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "redirect_url": "https://connect.example/x" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/connections/k1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/sync"))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "synced": [{ "toolkit": "gmail", "added": 3, "updated": 1, "error": null }]
            })))
            .mount(&server)
            .await;

        let a = api
            .ask(&AskRequest {
                question: "When is my flight?".into(),
                conversation_id: None,
            })
            .await
            .unwrap();
        assert_eq!(a.citations[0].n, 1);
        assert_eq!(a.usage.output_tokens, 5);
        let c = api.connections().await.unwrap();
        assert_eq!(c.connections[0].toolkit, "gmail");
        assert!(!c.available[1].connectable);
        assert_eq!(
            api.connect("googlecalendar").await.unwrap().redirect_url,
            "https://connect.example/x"
        );
        api.disconnect("k1").await.unwrap();
        assert!(matches!(
            api.disconnect("../me").await,
            Err(CloudError::Invalid(_))
        ));
        assert_eq!(api.sync().await.unwrap().synced[0].added, 3);
    }

    #[tokio::test]
    async fn errors_are_mapped() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .respond_with(err(401, "unauthorized", "bad token"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/ask"))
            .respond_with(err(
                429,
                "quota_exceeded",
                "You've used this month's AI allowance.",
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/sync"))
            .respond_with(err(429, "rate_limited", "slow down"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/export"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;
        assert!(matches!(api.me().await, Err(CloudError::NotSignedIn)));
        let q = api
            .ask(&AskRequest {
                question: "q".into(),
                conversation_id: Some("c1".into()),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&q, CloudError::QuotaExceeded(m) if m.contains("allowance")),
            "{q}"
        );
        assert_eq!(q.code(), "quota_exceeded");
        match api.sync().await {
            Err(CloudError::Api {
                status: 429, code, ..
            }) => assert_eq!(code, "rate_limited"),
            other => panic!("{other:?}"),
        }
        match api.export().await {
            Err(e @ CloudError::Api { status: 500, .. }) => assert_eq!(e.code(), "http_500"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn signed_out_calls_never_reach_the_server() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let api = Api::new(Arc::new(session(
            &server.uri(),
            Default::default(),
            Default::default(),
        )));
        assert!(matches!(
            api.connections().await,
            Err(CloudError::NotSignedIn)
        ));
    }
}
