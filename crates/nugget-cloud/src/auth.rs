//! Supabase Auth: checks access tokens by asking Supabase (`GET /auth/v1/user`), so it works
//! whatever JWT signing the project uses. Results are cached briefly by token hash.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("not signed in")]
    Invalid,
    #[error("auth service unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn verify(&self, access_token: &str) -> Result<AuthUser, AuthError>;
    /// Deletes the auth user (cascades to all their rows).
    async fn delete_user(&self, id: Uuid) -> Result<(), AuthError>;
}

const TTL: Duration = Duration::from_secs(60);
const MAX_CACHE: usize = 10_000;

pub struct SupabaseAuth {
    pub http: reqwest::Client,
    pub url: String,
    pub anon_key: String,
    pub service_role_key: Option<String>,
    cache: Mutex<HashMap<[u8; 32], (AuthUser, Instant)>>,
}

impl SupabaseAuth {
    pub fn new(
        http: reqwest::Client,
        url: String,
        anon_key: String,
        service_role_key: Option<String>,
    ) -> Self {
        Self {
            http,
            url,
            anon_key,
            service_role_key,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

fn unavailable(e: impl std::fmt::Display) -> AuthError {
    AuthError::Unavailable(e.to_string())
}

#[async_trait]
impl AuthProvider for SupabaseAuth {
    async fn verify(&self, token: &str) -> Result<AuthUser, AuthError> {
        if token.is_empty() || token.len() > 8192 {
            return Err(AuthError::Invalid);
        }
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if let Some((u, at)) = self
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            && at.elapsed() < TTL
        {
            return Ok(u.clone());
        }
        let res = self
            .http
            .get(format!("{}/auth/v1/user", self.url))
            .header("apikey", &self.anon_key)
            .bearer_auth(token)
            .send()
            .await
            .map_err(unavailable)?;
        match res.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AuthError::Invalid),
            s => return Err(AuthError::Unavailable(format!("status {s}"))),
        }
        let v: Value = res.json().await.map_err(unavailable)?;
        let id = v["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(AuthError::Invalid)?;
        let user = AuthUser {
            id,
            email: v["email"].as_str().map(str::to_owned),
        };
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= MAX_CACHE {
            cache.clear();
        }
        cache.insert(key, (user.clone(), Instant::now()));
        Ok(user)
    }

    async fn delete_user(&self, id: Uuid) -> Result<(), AuthError> {
        let key = self
            .service_role_key
            .as_ref()
            .ok_or_else(|| unavailable("SUPABASE_SERVICE_ROLE_KEY is not set"))?;
        let res = self
            .http
            .delete(format!("{}/auth/v1/admin/users/{id}", self.url))
            .header("apikey", key)
            .bearer_auth(key)
            .send()
            .await
            .map_err(unavailable)?;
        if !res.status().is_success() {
            return Err(AuthError::Unavailable(format!(
                "delete user: status {}",
                res.status()
            )));
        }
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, (u, _)| u.id != id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn verifies_and_caches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/auth/v1/user"))
            .and(header("apikey", "anon"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "00000000-0000-0000-0000-00000000000a", "email": "a@example.com"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/auth/v1/user"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let a = SupabaseAuth::new(reqwest::Client::new(), server.uri(), "anon".into(), None);
        let u = a.verify("good").await.unwrap();
        assert_eq!(u.email.as_deref(), Some("a@example.com"));
        assert_eq!(a.verify("good").await.unwrap(), u, "second call from cache");
        assert!(matches!(a.verify("bad").await, Err(AuthError::Invalid)));
        assert!(matches!(a.verify("").await, Err(AuthError::Invalid)));
        assert!(
            a.delete_user(u.id).await.is_err(),
            "no service key, no deletion"
        );
    }

    #[tokio::test]
    async fn delete_uses_admin_api() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(
                "/auth/v1/admin/users/00000000-0000-0000-0000-00000000000a",
            ))
            .and(header("authorization", "Bearer srk"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let a = SupabaseAuth::new(
            reqwest::Client::new(),
            server.uri(),
            "anon".into(),
            Some("srk".into()),
        );
        a.delete_user("00000000-0000-0000-0000-00000000000a".parse().unwrap())
            .await
            .unwrap();
    }
}
