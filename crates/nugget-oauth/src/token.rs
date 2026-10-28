use std::sync::Arc;
use std::time::Duration;

use nugget_connector::SecretStore;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::client::OAuthClient;

/// Tokens as stored (JSON) in the keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Expiry as Unix seconds; `None` = the provider did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl TokenSet {
    /// Whether the access token expires within `skew` from now.
    pub fn expires_within(&self, skew: Duration) -> bool {
        match self.expires_at {
            Some(at) => {
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                at <= now.saturating_add(skew.as_secs() as i64)
            }
            None => false,
        }
    }
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

pub(crate) async fn store_tokens(
    secrets: Arc<dyn SecretStore>,
    account: String,
    tokens: &TokenSet,
) -> Result<()> {
    let json = serde_json::to_string(tokens)?;
    tokio::task::spawn_blocking(move || secrets.set(&account, &json))
        .await
        .map_err(|e| Error::Other(format!("keychain task failed: {e}")))?
}

async fn load_tokens(secrets: Arc<dyn SecretStore>, account: String) -> Result<Option<TokenSet>> {
    let raw = tokio::task::spawn_blocking(move || secrets.get(&account))
        .await
        .map_err(|e| Error::Other(format!("keychain task failed: {e}")))??;
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(|_| {
            Error::Unauthorized("stored OAuth tokens are unreadable; reconnect the account".into())
        }),
    }
}

/// Hands out a valid access token for one account, refreshing it shortly before expiry and
/// writing refreshed tokens back to the keychain. Concurrent callers share one refresh.
pub struct TokenSource {
    client: OAuthClient,
    secrets: Arc<dyn SecretStore>,
    account: String,
    skew: Duration,
    cache: Mutex<Option<TokenSet>>,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSource")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl TokenSource {
    /// Refreshes when the token expires within 2 minutes.
    pub fn new(
        client: OAuthClient,
        secrets: Arc<dyn SecretStore>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            client,
            secrets,
            account: account.into(),
            skew: Duration::from_secs(120),
            cache: Mutex::new(None),
        }
    }

    pub fn with_refresh_skew(mut self, skew: Duration) -> Self {
        self.skew = skew;
        self
    }

    pub fn client(&self) -> &OAuthClient {
        &self.client
    }

    /// A valid access token. No stored tokens, or a refresh the provider rejects, is
    /// `Error::Unauthorized`.
    pub async fn access_token(&self) -> Result<String> {
        let mut cache = self.cache.lock().await;
        // Taken out of the cache: a failed refresh leaves it empty, so the next call re-reads
        // the keychain (the user may have reconnected meanwhile).
        let current = match cache.take() {
            Some(t) => t,
            None => self.load().await?,
        };
        if !current.expires_within(self.skew) {
            let token = current.access_token.clone();
            *cache = Some(current);
            return Ok(token);
        }
        let fresh = self.refresh_from(&current).await?;
        let token = fresh.access_token.clone();
        *cache = Some(fresh);
        Ok(token)
    }

    /// Call after the API rejected `rejected` (HTTP 401): reloads the keychain (the user may have
    /// reconnected) and refreshes unless a different token is already available.
    pub async fn force_refresh(&self, rejected: &str) -> Result<String> {
        let mut cache = self.cache.lock().await;
        let stored = self.load().await?;
        if stored.access_token != rejected && !stored.expires_within(self.skew) {
            let token = stored.access_token.clone();
            *cache = Some(stored);
            return Ok(token);
        }
        let fresh = self.refresh_from(&stored).await?;
        let token = fresh.access_token.clone();
        *cache = Some(fresh);
        Ok(token)
    }

    /// Forget cached tokens (e.g. after a new authorization stored fresh ones).
    pub async fn clear_cache(&self) {
        *self.cache.lock().await = None;
    }

    /// Whether tokens are stored for this account.
    pub async fn is_connected(&self) -> Result<bool> {
        Ok(load_tokens(self.secrets.clone(), self.account.clone())
            .await?
            .is_some())
    }

    async fn load(&self) -> Result<TokenSet> {
        load_tokens(self.secrets.clone(), self.account.clone())
            .await?
            .ok_or_else(|| {
                Error::Unauthorized(format!(
                    "not connected to {}; authorize the source first",
                    self.client.provider().name
                ))
            })
    }

    async fn refresh_from(&self, current: &TokenSet) -> Result<TokenSet> {
        let refresh_token = current.refresh_token.as_deref().ok_or_else(|| {
            Error::Unauthorized(
                "access token expired and no refresh token is stored; reconnect".into(),
            )
        })?;
        let mut fresh = self.client.refresh(refresh_token).await?;
        if fresh.refresh_token.is_none() {
            fresh.refresh_token = current.refresh_token.clone();
        }
        if fresh.scope.is_none() {
            fresh.scope = current.scope.clone();
        }
        store_tokens(self.secrets.clone(), self.account.clone(), &fresh).await?;
        tracing::debug!(account = %self.account, "refreshed OAuth access token");
        Ok(fresh)
    }
}
