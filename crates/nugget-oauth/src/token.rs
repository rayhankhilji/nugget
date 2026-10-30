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

    /// Whether the access token has already expired (by the local clock).
    fn expired(&self) -> bool {
        self.expires_within(Duration::ZERO)
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
    cache: Mutex<Cached>,
}

/// The in-memory copy. `unsaved` is set when a refresh succeeded but the keychain write failed:
/// with refresh-token rotation the old stored refresh token is already dead, so the cached set
/// is the only good copy and must win over the keychain until it is written back.
#[derive(Default)]
struct Cached {
    tokens: Option<TokenSet>,
    unsaved: bool,
    /// With `unsaved`: the access token the keychain held when the refresh happened. If the
    /// keychain holds anything else later, a new authorization was stored and it wins.
    base: Option<String>,
    /// After a failed early refresh: do not try again before this (the current token is used).
    retry_at: Option<tokio::time::Instant>,
}

/// Wait before retrying a failed early refresh when the provider gave no hint.
const EARLY_REFRESH_BACKOFF: Duration = Duration::from_secs(30);

impl Cached {
    fn forget(&mut self) {
        self.tokens = None;
        self.unsaved = false;
        self.base = None;
        self.retry_at = None;
    }
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
            cache: Mutex::new(Cached::default()),
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
        self.retry_unsaved(&mut cache).await;
        // Taken out of the cache: a failed refresh leaves it empty, so the next call re-reads
        // the keychain (the user may have reconnected meanwhile) — unless it holds the only
        // copy of rotated tokens.
        let current = match cache.tokens.take() {
            Some(t) => t,
            None => self.load().await?,
        };
        let backing_off = cache
            .retry_at
            .is_some_and(|at| tokio::time::Instant::now() < at);
        if !current.expires_within(self.skew) || (backing_off && !current.expired()) {
            let token = current.access_token.clone();
            cache.tokens = Some(current);
            return Ok(token);
        }
        match self.refresh_from(&current, &mut cache).await {
            Ok(token) => Ok(token),
            // A rejected refresh: these tokens are dead. Never keep them (a reconnect must not
            // be undone by writing them back).
            Err(e @ Error::Unauthorized(_)) => {
                cache.forget();
                Err(e)
            }
            // Refreshing early (inside the skew) failed for a transient reason: the current token
            // still works, so keep using it, and do not ask again before the server's hint (or
            // a short backoff) has passed.
            Err(e) if !current.expired() => {
                let wait = match &e {
                    Error::RateLimited {
                        retry_after_ms: Some(ms),
                    } => Duration::from_millis(*ms),
                    _ => EARLY_REFRESH_BACKOFF,
                };
                tracing::warn!(account = %self.account, error = %e, ?wait, "early OAuth refresh failed; using the current token");
                cache.retry_at = Some(tokio::time::Instant::now() + wait);
                let token = current.access_token.clone();
                cache.tokens = Some(current);
                Ok(token)
            }
            Err(e) => {
                if cache.unsaved {
                    cache.tokens = Some(current);
                }
                Err(e)
            }
        }
    }

    /// Call after the API rejected `rejected` (HTTP 401): reloads the keychain (the user may have
    /// reconnected) and refreshes unless a different token is already available.
    pub async fn force_refresh(&self, rejected: &str) -> Result<String> {
        let mut cache = self.cache.lock().await;
        self.retry_unsaved(&mut cache).await;
        let stored = match cache.tokens.clone() {
            Some(t) if cache.unsaved => t,
            _ => self.load().await?,
        };
        if stored.access_token != rejected && !stored.expires_within(self.skew) {
            let token = stored.access_token.clone();
            cache.tokens = Some(stored);
            return Ok(token);
        }
        let r = self.refresh_from(&stored, &mut cache).await;
        if matches!(r, Err(Error::Unauthorized(_))) {
            cache.forget();
        }
        r
    }

    /// Forget cached tokens (e.g. after a new authorization stored fresh ones).
    pub async fn clear_cache(&self) {
        // A new authorization always wins over tokens a failed write left in memory.
        self.cache.lock().await.forget();
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

    /// Write back tokens a previous refresh could not store, unless the keychain changed since
    /// (the user reconnected: the new grant wins and the in-memory set is dropped).
    async fn retry_unsaved(&self, cache: &mut Cached) {
        if !cache.unsaved {
            return;
        }
        match load_tokens(self.secrets.clone(), self.account.clone()).await {
            Ok(stored) if stored.as_ref().map(|t| &t.access_token) != cache.base.as_ref() => {
                tracing::info!(account = %self.account, "keychain holds new OAuth tokens; dropping unsaved ones");
                cache.forget();
                return;
            }
            Ok(_) => {}
            Err(_) => return, // keychain unreadable: keep the in-memory set
        }
        if let Some(t) = &cache.tokens
            && store_tokens(self.secrets.clone(), self.account.clone(), t)
                .await
                .is_ok()
        {
            cache.unsaved = false;
            cache.base = None;
        }
    }

    /// Refresh, cache and store the new set; returns the access token.
    async fn refresh_from(&self, current: &TokenSet, cache: &mut Cached) -> Result<String> {
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
        let token = fresh.access_token.clone();
        // The provider may have rotated the refresh token, invalidating the stored one: keep the
        // new set in memory even if the keychain write fails, and retry the write later.
        let stored = store_tokens(self.secrets.clone(), self.account.clone(), &fresh).await;
        if stored.is_err() && !cache.unsaved {
            // What the keychain still holds (unless an earlier failed write already set it).
            cache.base = Some(current.access_token.clone());
        }
        if stored.is_ok() {
            cache.base = None;
        }
        cache.tokens = Some(fresh);
        cache.unsaved = stored.is_err();
        cache.retry_at = None;
        if let Err(e) = stored {
            tracing::warn!(account = %self.account, error = %e, "could not store refreshed OAuth tokens; will retry");
        }
        tracing::debug!(account = %self.account, "refreshed OAuth access token");
        Ok(token)
    }
}
