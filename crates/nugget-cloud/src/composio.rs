//! Composio: managed OAuth, connected accounts and tool calls. The Composio `user_id` is the
//! Supabase user id. REST v3.1, `x-api-key` header.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum ComposioError {
    #[error("Composio is not configured (set COMPOSIO_API_KEY)")]
    NotConfigured,
    #[error("Composio request failed: {0}")]
    Http(String),
    #[error("Composio tool {tool} failed: {message}")]
    Tool { tool: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Toolkit {
    pub slug: String,
    pub name: String,
    pub logo: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub connected_account_id: String,
    pub redirect_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: String,
    pub toolkit: String,
    /// Composio's status, e.g. `ACTIVE`, `INITIATED`, `FAILED`, `EXPIRED`.
    pub status: String,
}

#[async_trait]
pub trait Composio: Send + Sync {
    /// Catalogue of toolkits (first page).
    async fn toolkits(&self) -> Result<Vec<Toolkit>, ComposioError>;
    /// Starts an OAuth connection; the user opens `redirect_url`.
    async fn create_link(
        &self,
        user: &str,
        auth_config: &str,
        callback: &str,
    ) -> Result<Link, ComposioError>;
    /// The user's connected accounts.
    async fn accounts(&self, user: &str) -> Result<Vec<Account>, ComposioError>;
    async fn delete_account(&self, id: &str) -> Result<(), ComposioError>;
    /// Runs a tool as the user through one connected account and returns its `data`.
    async fn execute(
        &self,
        user: &str,
        account: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, ComposioError>;
}

/// Our status for a Composio status.
pub fn map_status(s: &str) -> &'static str {
    match s.to_ascii_uppercase().as_str() {
        "ACTIVE" => "active",
        "INITIATED" | "INITIALIZING" => "initiated",
        "INACTIVE" | "DISABLED" => "disabled",
        _ => "failed",
    }
}

pub struct HttpComposio {
    pub http: reqwest::Client,
    pub base: String,
    pub key: String,
}

fn http_err(e: impl std::fmt::Display) -> ComposioError {
    ComposioError::Http(e.to_string())
}

impl HttpComposio {
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, ComposioError> {
        let res = req
            .header("x-api-key", &self.key)
            .send()
            .await
            .map_err(http_err)?;
        let status = res.status();
        let text = res.text().await.map_err(http_err)?;
        let v: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(http_err)?
        };
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            return Err(ComposioError::Http(format!("{status}: {msg}")));
        }
        Ok(v)
    }
}

fn s(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(Value::as_str).map(str::to_owned)
}

#[async_trait]
impl Composio for HttpComposio {
    async fn toolkits(&self) -> Result<Vec<Toolkit>, ComposioError> {
        let v = self
            .send(self.http.get(format!("{}/toolkits", self.base)))
            .await?;
        let items = v["items"]
            .as_array()
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|t| {
                Some(Toolkit {
                    slug: s(t, "/slug")?.to_ascii_lowercase(),
                    name: s(t, "/name").unwrap_or_default(),
                    logo: s(t, "/meta/logo"),
                })
            })
            .collect())
    }

    async fn create_link(
        &self,
        user: &str,
        auth_config: &str,
        callback: &str,
    ) -> Result<Link, ComposioError> {
        let body =
            json!({"auth_config_id": auth_config, "user_id": user, "callback_url": callback});
        let v = self
            .send(
                self.http
                    .post(format!("{}/connected_accounts/link", self.base))
                    .json(&body),
            )
            .await?;
        Ok(Link {
            connected_account_id: s(&v, "/connected_account_id")
                .ok_or_else(|| http_err("no connected_account_id"))?,
            redirect_url: s(&v, "/redirect_url").ok_or_else(|| http_err("no redirect_url"))?,
        })
    }

    async fn accounts(&self, user: &str) -> Result<Vec<Account>, ComposioError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let mut q = vec![("user_ids", user.to_owned()), ("limit", "100".into())];
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            let v = self
                .send(
                    self.http
                        .get(format!("{}/connected_accounts", self.base))
                        .query(&q),
                )
                .await?;
            for a in v["items"].as_array().into_iter().flatten() {
                if let (Some(id), Some(toolkit)) = (s(a, "/id"), s(a, "/toolkit/slug")) {
                    let status = s(a, "/status").unwrap_or_default();
                    out.push(Account {
                        id,
                        toolkit: toolkit.to_ascii_lowercase(),
                        status,
                    });
                }
            }
            cursor = s(&v, "/next_cursor");
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn delete_account(&self, id: &str) -> Result<(), ComposioError> {
        let id = url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>();
        self.send(
            self.http
                .delete(format!("{}/connected_accounts/{id}", self.base)),
        )
        .await?;
        Ok(())
    }

    async fn execute(
        &self,
        user: &str,
        account: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, ComposioError> {
        let body = json!({"user_id": user, "connected_account_id": account, "arguments": args});
        let v = self
            .send(
                self.http
                    .post(format!("{}/tools/execute/{tool}", self.base))
                    .json(&body),
            )
            .await?;
        if v["successful"] != Value::Bool(true) {
            let message = s(&v, "/error").unwrap_or_else(|| "unsuccessful".into());
            return Err(ComposioError::Tool {
                tool: tool.into(),
                message,
            });
        }
        // `data` is documented as a string on some tools; accept JSON text or an object.
        Ok(match &v["data"] {
            Value::String(t) => serde_json::from_str(t).unwrap_or(Value::String(t.clone())),
            other => other.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> HttpComposio {
        HttpComposio {
            http: reqwest::Client::new(),
            base: server.uri(),
            key: "ck".into(),
        }
    }

    #[tokio::test]
    async fn create_link_sends_user_and_auth_config() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/connected_accounts/link"))
            .and(header("x-api-key", "ck"))
            .and(body_json(json!({"auth_config_id": "ac_1", "user_id": "u1", "callback_url": "heed://connections/callback"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "link_token": "t", "redirect_url": "https://connect/x", "expires_at": "2026-11-02T00:00:00Z",
                "connected_account_id": "ca_1"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let l = client(&server)
            .create_link("u1", "ac_1", "heed://connections/callback")
            .await
            .unwrap();
        assert_eq!(
            l,
            Link {
                connected_account_id: "ca_1".into(),
                redirect_url: "https://connect/x".into()
            }
        );
    }

    #[tokio::test]
    async fn accounts_page_through() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/connected_accounts"))
            .and(query_param("user_ids", "u1"))
            .and(query_param("cursor", "c2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "ca_2", "status": "INITIATED", "toolkit": {"slug": "googlecalendar"}}],
                "next_cursor": null
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/connected_accounts"))
            .and(query_param("user_ids", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "ca_1", "status": "ACTIVE", "toolkit": {"slug": "GMAIL"}}],
                "next_cursor": "c2"
            })))
            .mount(&server)
            .await;
        let a = client(&server).accounts("u1").await.unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(
            a[0],
            Account {
                id: "ca_1".into(),
                toolkit: "gmail".into(),
                status: "ACTIVE".into()
            }
        );
        assert_eq!(map_status(&a[1].status), "initiated");
    }

    #[tokio::test]
    async fn execute_unwraps_data_and_reports_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tools/execute/GMAIL_FETCH_EMAILS"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": "{\"messages\": []}", "error": null, "successful": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tools/execute/BAD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {}, "error": "token expired", "successful": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tools/execute/DOWN"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(json!({"error": {"message": "busy"}})),
            )
            .mount(&server)
            .await;
        let c = client(&server);
        assert_eq!(
            c.execute("u", "ca", "GMAIL_FETCH_EMAILS", json!({}))
                .await
                .unwrap(),
            json!({"messages": []})
        );
        let e = c.execute("u", "ca", "BAD", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("token expired"));
        let e = c.execute("u", "ca", "DOWN", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("busy"));
    }

    #[tokio::test]
    async fn toolkits_parse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/toolkits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"slug": "gmail", "name": "Gmail", "meta": {"logo": "https://l/g.png"}}, {"name": "no slug"}]
            })))
            .mount(&server)
            .await;
        let t = client(&server).toolkits().await.unwrap();
        assert_eq!(
            t,
            vec![Toolkit {
                slug: "gmail".into(),
                name: "Gmail".into(),
                logo: Some("https://l/g.png".into())
            }]
        );
    }
}
