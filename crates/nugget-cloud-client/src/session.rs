//! The signed-in session: a sign-in in progress (its PKCE verifier, in memory only, for 10
//! minutes, single use), the refresh token in a [`SessionStore`] and the access token in memory,
//! refreshed 60 seconds before it expires.

use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::auth::{Callback, Provider, SupabaseAuth, TokenResponse, authorize_url};
use crate::pkce::Pkce;
use crate::{CloudConfig, CloudError, Result};

/// How long a sign-in started in Nugget may take to come back.
pub const PENDING_TTL_SECS: i64 = 10 * 60;
/// The access token is refreshed this long before it expires.
pub const REFRESH_MARGIN_SECS: i64 = 60;

const KEYCHAIN_SERVICE: &str = "nugget";
const KEYCHAIN_ENTRY: &str = "cloud-session";

/// What persists across launches. The email is kept so the app can say who is signed in
/// without a network call.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSession {
    pub refresh_token: String,
    #[serde(default)]
    pub email: Option<String>,
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Where the refresh token lives between launches.
pub trait SessionStore: Send + Sync {
    fn load(&self) -> Result<Option<StoredSession>>;
    fn save(&self, session: &StoredSession) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// The OS keychain: service `nugget`, entry `cloud-session`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

impl KeychainStore {
    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ENTRY)
            .map_err(|e| CloudError::Keychain(e.to_string()))
    }
}

impl SessionStore for KeychainStore {
    fn load(&self) -> Result<Option<StoredSession>> {
        match Self::entry()?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|_| CloudError::Keychain("the saved session is unreadable".into())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CloudError::Keychain(e.to_string())),
        }
    }

    fn save(&self, session: &StoredSession) -> Result<()> {
        let json =
            serde_json::to_string(session).map_err(|e| CloudError::Keychain(e.to_string()))?;
        Self::entry()?
            .set_password(&json)
            .map_err(|e| CloudError::Keychain(e.to_string()))
    }

    fn clear(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CloudError::Keychain(e.to_string())),
        }
    }
}

/// In memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Option<StoredSession>>);

impl MemoryStore {
    pub fn with(session: StoredSession) -> Self {
        Self(Mutex::new(Some(session)))
    }

    pub fn get(&self) -> Option<StoredSession> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SessionStore for MemoryStore {
    fn load(&self) -> Result<Option<StoredSession>> {
        Ok(self.get())
    }
    fn save(&self, session: &StoredSession) -> Result<()> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

/// Unix seconds; injectable for tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }
}

/// Who is signed in, for the UI. Never carries a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStatus {
    pub signed_in: bool,
    pub email: Option<String>,
}

struct Pending {
    verifier: String,
    started: i64,
}

struct Access {
    token: String,
    expires_at: i64,
}

#[derive(Default)]
struct Inner {
    pending: Option<Pending>,
    access: Option<Access>,
    stored: Option<StoredSession>,
    loaded: bool,
}

pub struct Session {
    auth: SupabaseAuth,
    http: reqwest::Client,
    store: Arc<dyn SessionStore>,
    clock: Arc<dyn Clock>,
    inner: tokio::sync::Mutex<Inner>,
}

impl Session {
    pub fn new(
        cfg: CloudConfig,
        http: reqwest::Client,
        store: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            auth: SupabaseAuth::new(cfg, http.clone()),
            http,
            store,
            clock,
            inner: tokio::sync::Mutex::new(Inner::default()),
        }
    }

    pub fn config(&self) -> &CloudConfig {
        self.auth.config()
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn ensure_loaded(&self, inner: &mut Inner) {
        if inner.loaded {
            return;
        }
        inner.loaded = true;
        match self.store.load() {
            Ok(s) => inner.stored = s,
            Err(e) => tracing::warn!(error = %e, "couldn't read the saved cloud session"),
        }
    }

    fn start_pending(&self, inner: &mut Inner) -> Result<Pkce> {
        let pkce = Pkce::generate()?;
        // Newest wins: a second click replaces the first sign-in.
        inner.pending = Some(Pending {
            verifier: pkce.verifier.clone(),
            started: self.clock.now(),
        });
        Ok(pkce)
    }

    /// Start a Google sign-in: the URL to open in the system browser.
    pub async fn begin_oauth(&self, provider: Provider) -> Result<String> {
        let mut inner = self.inner.lock().await;
        let pkce = self.start_pending(&mut inner)?;
        authorize_url(self.config(), provider, &pkce.challenge)
    }

    /// Start an email sign-in: Supabase emails a link that comes back to the deep link.
    pub async fn begin_magic_link(&self, email: &str) -> Result<()> {
        let email = valid_email(email)?;
        let mut inner = self.inner.lock().await;
        let pkce = self.start_pending(&mut inner)?;
        if let Err(e) = self.auth.send_magic_link(&email, &pkce.challenge).await {
            inner.pending = None;
            return Err(e);
        }
        Ok(())
    }

    /// Finish a sign-in from the deep link. Refused unless a sign-in started in this process in
    /// the last 10 minutes. The pending sign-in is used up by success, expiry or an error
    /// callback, but NOT by a code Supabase refuses: any web page can open the deep link, and a
    /// forged code must not cancel the real sign-in that is still on its way (PKCE already
    /// stops a forged code from signing anyone in).
    pub async fn complete(&self, callback: Callback) -> Result<SessionStatus> {
        let mut inner = self.inner.lock().await;
        let pending = inner.pending.as_ref().ok_or(CloudError::NoPendingSignIn)?;
        if self.clock.now() - pending.started > PENDING_TTL_SECS {
            inner.pending = None;
            return Err(CloudError::SignInExpired);
        }
        let verifier = pending.verifier.clone();
        let code = match callback {
            Callback::Code(code) => code,
            err @ Callback::Error { .. } => {
                inner.pending = None;
                return Err(CloudError::SignInFailed(
                    err.error_message().unwrap_or_default(),
                ));
            }
        };
        let tokens = self.auth.exchange(&code, &verifier).await?;
        inner.pending = None;
        inner.loaded = true;
        self.apply(&mut inner, tokens);
        Ok(status_of(&inner))
    }

    fn apply(&self, inner: &mut Inner, t: TokenResponse) {
        let now = self.clock.now();
        inner.access = Some(Access {
            token: t.access_token,
            expires_at: t.expires_at.unwrap_or(now + t.expires_in),
        });
        let stored = StoredSession {
            refresh_token: t.refresh_token,
            email: t
                .user
                .email
                .or_else(|| inner.stored.as_ref().and_then(|s| s.email.clone())),
        };
        // Still signed in for this launch if the keychain refuses; the next launch asks again.
        if let Err(e) = self.store.save(&stored) {
            tracing::warn!(error = %e, "couldn't save the cloud session");
        }
        inner.stored = Some(stored);
    }

    pub async fn status(&self) -> SessionStatus {
        let mut inner = self.inner.lock().await;
        self.ensure_loaded(&mut inner);
        status_of(&inner)
    }

    /// A valid access token, refreshing it when it expires within a minute.
    pub async fn access_token(&self) -> Result<String> {
        let mut inner = self.inner.lock().await;
        self.ensure_loaded(&mut inner);
        let now = self.clock.now();
        if let Some(a) = &inner.access
            && a.expires_at - REFRESH_MARGIN_SECS > now
        {
            return Ok(a.token.clone());
        }
        let refresh = inner
            .stored
            .as_ref()
            .map(|s| s.refresh_token.clone())
            .ok_or(CloudError::NotSignedIn)?;
        match self.auth.refresh(&refresh).await {
            Ok(tokens) => {
                self.apply(&mut inner, tokens);
                Ok(inner
                    .access
                    .as_ref()
                    .map(|a| a.token.clone())
                    .unwrap_or_default())
            }
            Err(CloudError::NotSignedIn) => {
                self.forget_locked(&mut inner);
                Err(CloudError::NotSignedIn)
            }
            Err(e) => Err(e),
        }
    }

    /// The server refused the access token: drop it, so the next call refreshes.
    pub async fn invalidate_access(&self) {
        self.inner.lock().await.access = None;
    }

    /// Sign out: revoke the session at Supabase (best effort) and forget it here. Returns whether
    /// the revocation reached Supabase.
    pub async fn sign_out(&self) -> Result<bool> {
        let token = self.access_token().await.ok();
        let revoked = match token {
            Some(t) => match self.auth.logout(&t).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't revoke the cloud session");
                    false
                }
            },
            None => false,
        };
        let mut inner = self.inner.lock().await;
        self.forget_locked(&mut inner);
        inner.pending = None;
        Ok(revoked)
    }

    /// Forget the session locally (after the account was deleted).
    pub async fn forget(&self) {
        let mut inner = self.inner.lock().await;
        self.forget_locked(&mut inner);
    }

    fn forget_locked(&self, inner: &mut Inner) {
        inner.access = None;
        inner.stored = None;
        inner.loaded = true;
        if let Err(e) = self.store.clear() {
            tracing::warn!(error = %e, "couldn't delete the saved cloud session");
        }
    }
}

fn status_of(inner: &Inner) -> SessionStatus {
    SessionStatus {
        signed_in: inner.stored.is_some() || inner.access.is_some(),
        email: inner.stored.as_ref().and_then(|s| s.email.clone()),
    }
}

/// A plausible address (Supabase does the real check): one `@`, no spaces or control characters.
fn valid_email(raw: &str) -> Result<String> {
    let email = raw.trim();
    let ok = (3..=254).contains(&email.len())
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        });
    if ok {
        Ok(email.to_owned())
    } else {
        Err(CloudError::Invalid("enter a valid email address".into()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::auth::tests::{cfg, token_body};
    use std::sync::atomic::{AtomicI64, Ordering};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    pub struct FakeClock(pub AtomicI64);
    impl Clock for FakeClock {
        fn now(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    impl FakeClock {
        pub fn set(&self, t: i64) {
            self.0.store(t, Ordering::SeqCst);
        }
    }

    pub fn session(base: &str, store: Arc<MemoryStore>, clock: Arc<FakeClock>) -> Session {
        Session::new(cfg(base), crate::test_http(), store, clock)
    }

    async fn mount_pkce(server: &MockServer, expires_at: i64) {
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "pkce"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(token_body("at", "rt", expires_at)),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn callback_without_a_pending_sign_in_is_refused() {
        let server = MockServer::start().await;
        let s = session(&server.uri(), Default::default(), Default::default());
        assert!(matches!(
            s.complete(Callback::Code("c".into())).await,
            Err(CloudError::NoPendingSignIn)
        ));
        assert!(!s.status().await.signed_in);
    }

    #[tokio::test]
    async fn sign_in_is_single_use_and_saves_the_refresh_token() {
        let server = MockServer::start().await;
        mount_pkce(&server, 10_000).await;
        let store = Arc::new(MemoryStore::default());
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        let s = session(&server.uri(), store.clone(), clock.clone());
        let url = s.begin_oauth(Provider::Google).await.unwrap();
        assert!(url.contains("code_challenge="), "{url}");
        let st = s.complete(Callback::Code("c".into())).await.unwrap();
        assert_eq!(
            st,
            SessionStatus {
                signed_in: true,
                email: Some("me@example.com".into())
            }
        );
        assert_eq!(store.get().unwrap().refresh_token, "rt");
        assert_eq!(s.access_token().await.unwrap(), "at");
        // Replaying the link does nothing.
        assert!(matches!(
            s.complete(Callback::Code("c".into())).await,
            Err(CloudError::NoPendingSignIn)
        ));
    }

    #[tokio::test]
    async fn a_forged_code_does_not_cancel_the_real_sign_in() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"auth_code": "forged"}),
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "code": 404, "error_code": "flow_state_not_found", "msg": "invalid flow state"
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        mount_pkce(&server, 10_000).await;
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        let s = session(&server.uri(), Default::default(), clock);
        s.begin_oauth(Provider::Google).await.unwrap();
        // A web page opens heed://auth/callback?code=forged before the real redirect lands.
        assert!(s.complete(Callback::Code("forged".into())).await.is_err());
        assert!(!s.status().await.signed_in);
        let st = s.complete(Callback::Code("c".into())).await.unwrap();
        assert!(st.signed_in, "the real callback still completes");
    }

    #[tokio::test]
    async fn a_pending_sign_in_expires_after_ten_minutes() {
        let server = MockServer::start().await;
        mount_pkce(&server, 10_000).await;
        let clock = Arc::new(FakeClock::default());
        let s = session(&server.uri(), Default::default(), clock.clone());
        s.begin_oauth(Provider::Google).await.unwrap();
        clock.set(PENDING_TTL_SECS + 1);
        assert!(matches!(
            s.complete(Callback::Code("c".into())).await,
            Err(CloudError::SignInExpired)
        ));
        // An error callback uses up the pending sign-in and reports Supabase's reason.
        s.begin_oauth(Provider::Google).await.unwrap();
        let err = s
            .complete(Callback::Error {
                error: "access_denied".into(),
                description: Some("User cancelled".into()),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CloudError::SignInFailed(m) if m == "User cancelled"),
            "{err}"
        );
        assert!(matches!(
            s.complete(Callback::Code("c".into())).await,
            Err(CloudError::NoPendingSignIn)
        ));
    }

    #[tokio::test]
    async fn access_token_refreshes_sixty_seconds_before_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "refresh_token"))
            .and(body_json(serde_json::json!({ "refresh_token": "rt0" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("at1", "rt1", 5_000)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "refresh_token"))
            .and(body_json(serde_json::json!({ "refresh_token": "rt1" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("at2", "rt2", 9_000)))
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::with(StoredSession {
            refresh_token: "rt0".into(),
            email: Some("me@example.com".into()),
        }));
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        let s = session(&server.uri(), store.clone(), clock.clone());
        assert!(s.status().await.signed_in);
        // No access token in memory yet: refresh.
        assert_eq!(s.access_token().await.unwrap(), "at1");
        assert_eq!(store.get().unwrap().refresh_token, "rt1");
        // 61 s before expiry: still good.
        clock.set(5_000 - 61);
        assert_eq!(s.access_token().await.unwrap(), "at1");
        // Within the minute: refreshed.
        clock.set(5_000 - 59);
        assert_eq!(s.access_token().await.unwrap(), "at2");
        assert_eq!(store.get().unwrap().refresh_token, "rt2");
    }

    #[tokio::test]
    async fn a_refused_refresh_token_signs_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": "Invalid Refresh Token"
            })))
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::with(StoredSession {
            refresh_token: "old".into(),
            email: None,
        }));
        let s = session(&server.uri(), store.clone(), Default::default());
        assert!(matches!(
            s.access_token().await,
            Err(CloudError::NotSignedIn)
        ));
        assert!(store.get().is_none());
        assert!(!s.status().await.signed_in);
    }

    #[tokio::test]
    async fn sign_out_revokes_and_forgets() {
        let server = MockServer::start().await;
        mount_pkce(&server, 10_000).await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/logout"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::default());
        let s = session(&server.uri(), store.clone(), Default::default());
        s.begin_oauth(Provider::Google).await.unwrap();
        s.complete(Callback::Code("c".into())).await.unwrap();
        assert!(s.sign_out().await.unwrap());
        assert!(store.get().is_none());
        assert!(!s.status().await.signed_in);
        assert!(matches!(
            s.access_token().await,
            Err(CloudError::NotSignedIn)
        ));
    }

    #[tokio::test]
    async fn magic_link_needs_an_email_and_starts_a_sign_in() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/otp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        mount_pkce(&server, 10_000).await;
        let s = session(&server.uri(), Default::default(), Default::default());
        for bad in ["", "me", "me@", "@x.com", "a b@x.com", "me@localhost"] {
            assert!(
                matches!(s.begin_magic_link(bad).await, Err(CloudError::Invalid(_))),
                "{bad}"
            );
        }
        s.begin_magic_link(" me@example.com ").await.unwrap();
        assert!(
            s.complete(Callback::Code("c".into()))
                .await
                .unwrap()
                .signed_in
        );
    }
}
