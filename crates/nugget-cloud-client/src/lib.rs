//! Desktop client for Nugget Cloud (docs/architecture-cloud.md, sections 1 and 4).
//!
//! - [`CloudConfig`]: where the cloud is (Supabase project and the `/v1` API base).
//! - [`pkce`]: the PKCE verifier and S256 challenge.
//! - [`auth`]: Supabase Auth endpoints (authorize URL for Google, magic link, token exchange, refresh,
//!   logout) and the strict parser for the `nugget://auth/callback` deep link.
//! - [`session`]: the signed-in session. The verifier of a sign-in in progress lives only in
//!   memory for 10 minutes and is single use; the refresh token is kept in a [`SessionStore`]
//!   (the OS keychain in the app); the access token lives only in memory and is refreshed
//!   60 seconds before it expires.
//! - [`api`]: the typed client for the cloud service's `/v1/*` endpoints.

pub mod api;
pub mod auth;
pub mod pkce;
pub mod session;

pub use api::{
    Answer, Api, AskRequest, AskUsage, AvailableApp, BillingLink, Brief, Card, CardAction,
    CardEvidence, Cards, Citation, ConnectStart, Connection, Connections, DEVICE_HEADER,
    DismissReason, Entitlements, FacetCount, Facets, Interval, Me, ModelTier, PlanOffer, Plans,
    SearchDates, SearchRequest, SearchResponse, SearchResult, Subscription, SyncReport, SyncResult,
    Timeline, TimelineBucket, Usage,
};
pub use auth::{Callback, Provider, REDIRECT_URL, parse_callback};
pub use session::{
    Clock, KeychainStore, MemoryStore, Session, SessionStatus, SessionStore, StoredSession,
    SystemClock,
};

/// Environment variables (read at run time, else baked in at build time).
pub const ENV_SUPABASE_URL: &str = "NUGGET_SUPABASE_URL";
pub const ENV_SUPABASE_ANON_KEY: &str = "NUGGET_SUPABASE_ANON_KEY";
pub const ENV_CLOUD_URL: &str = "NUGGET_CLOUD_URL";

pub type Result<T, E = CloudError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("Nugget Cloud isn't configured in this build ({0} is not set)")]
    NotConfigured(String),
    #[error("not signed in to Nugget Cloud")]
    NotSignedIn,
    #[error("no sign-in is in progress; start it again from Nugget")]
    NoPendingSignIn,
    #[error("the sign-in took too long; please start it again")]
    SignInExpired,
    #[error("sign-in failed: {0}")]
    SignInFailed(String),
    #[error("{0}")]
    QuotaExceeded(String),
    /// 402: no active or trialing subscription.
    #[error("{0}")]
    SubscriptionRequired(String),
    /// 403: this account is signed in on as many devices as its plan allows.
    #[error("{0}")]
    DeviceLimitReached(String),
    /// 403: the plan's limit of connected apps is reached.
    #[error("{0}")]
    ConnectionLimitReached(String),
    /// 409: the billing portal needs a subscription first.
    #[error("{0}")]
    NoCustomer(String),
    #[error("{message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("couldn't reach Nugget Cloud: {0}")]
    Network(String),
    #[error("unexpected response from Nugget Cloud: {0}")]
    Protocol(String),
    #[error("couldn't use the keychain: {0}")]
    Keychain(String),
    #[error("{0}")]
    Invalid(String),
}

impl CloudError {
    /// A stable machine-readable code for the UI.
    pub fn code(&self) -> &str {
        match self {
            Self::NotConfigured(_) => "not_configured",
            Self::NotSignedIn => "not_signed_in",
            Self::NoPendingSignIn => "no_pending_sign_in",
            Self::SignInExpired => "sign_in_expired",
            Self::SignInFailed(_) => "sign_in_failed",
            Self::QuotaExceeded(_) => "quota_exceeded",
            Self::SubscriptionRequired(_) => "subscription_required",
            Self::DeviceLimitReached(_) => "device_limit_reached",
            Self::ConnectionLimitReached(_) => "connection_limit_reached",
            Self::NoCustomer(_) => "no_customer",
            Self::Api { code, .. } => code,
            Self::Network(_) => "network",
            Self::Protocol(_) => "protocol",
            Self::Keychain(_) => "keychain",
            Self::Invalid(_) => "invalid",
        }
    }

    pub(crate) fn network(e: reqwest::Error) -> Self {
        // Without the URL: it may carry a query (never a secret here, but keep logs short).
        Self::Network(e.without_url().to_string())
    }
}

/// Where Nugget Cloud is. The anon key is public by design (protected by RLS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudConfig {
    /// `https://<project>.supabase.co`, without a trailing slash.
    pub supabase_url: String,
    pub supabase_anon_key: String,
    /// The cloud service, e.g. `https://api.nugget.example`, without a trailing slash.
    pub api_base: String,
}

impl CloudConfig {
    /// From the environment, falling back to the values baked in at build time.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| built_in(name))
        })
    }

    /// From any lookup of the three variables (tests, or another settings source).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let need = |name: &str| {
            get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| CloudError::NotConfigured(name.to_owned()))
        };
        let supabase_url = base_url(ENV_SUPABASE_URL, &need(ENV_SUPABASE_URL)?)?;
        let supabase_anon_key = need(ENV_SUPABASE_ANON_KEY)?;
        let api_base = base_url(ENV_CLOUD_URL, &need(ENV_CLOUD_URL)?)?;
        Ok(Self {
            supabase_url,
            supabase_anon_key,
            api_base,
        })
    }
}

fn built_in(name: &str) -> Option<String> {
    let v = match name {
        ENV_SUPABASE_URL => option_env!("NUGGET_SUPABASE_URL"),
        ENV_SUPABASE_ANON_KEY => option_env!("NUGGET_SUPABASE_ANON_KEY"),
        ENV_CLOUD_URL => option_env!("NUGGET_CLOUD_URL"),
        _ => None,
    };
    v.map(str::to_owned).filter(|v| !v.trim().is_empty())
}

/// An `https:` base URL (plain `http:` only on loopback, for local development), no trailing `/`.
fn base_url(name: &str, raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| CloudError::NotConfigured(format!("{name} (not a URL: {e})")))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let ok = match url.scheme() {
        "https" => url.host_str().is_some_and(|h| !h.is_empty()),
        "http" => loopback,
        _ => false,
    };
    if !ok || url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(CloudError::NotConfigured(format!(
            "{name} (must be an https URL)"
        )));
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

/// The HTTP client every request uses, with timeouts. rustls takes the process-default crypto
/// provider, so the caller must install one first (the desktop installs `ring`).
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(CloudError::network)
}

/// A client for tests, with `ring` installed as the process default.
#[cfg(test)]
pub(crate) fn test_http() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    http_client().expect("test http client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn config_reads_all_three_and_trims_slashes() {
        let cfg = CloudConfig::from_lookup(lookup(&[
            (ENV_SUPABASE_URL, "https://abc.supabase.co/"),
            (ENV_SUPABASE_ANON_KEY, "anon"),
            (ENV_CLOUD_URL, "https://api.example.com"),
        ]))
        .unwrap();
        assert_eq!(cfg.supabase_url, "https://abc.supabase.co");
        assert_eq!(cfg.api_base, "https://api.example.com");
        assert_eq!(cfg.supabase_anon_key, "anon");
    }

    #[test]
    fn missing_or_bad_config_is_not_configured() {
        let err = CloudConfig::from_lookup(lookup(&[
            (ENV_SUPABASE_URL, "https://abc.supabase.co"),
            (ENV_SUPABASE_ANON_KEY, "  "),
        ]))
        .unwrap_err();
        assert!(
            matches!(&err, CloudError::NotConfigured(v) if v == ENV_SUPABASE_ANON_KEY),
            "{err}"
        );
        assert_eq!(err.code(), "not_configured");
        for bad in [
            "http://abc.supabase.co",
            "ftp://x",
            "https://x?y=1",
            "nonsense",
        ] {
            let r = CloudConfig::from_lookup(lookup(&[
                (ENV_SUPABASE_URL, bad),
                (ENV_SUPABASE_ANON_KEY, "anon"),
                (ENV_CLOUD_URL, "https://api.example.com"),
            ]));
            assert!(matches!(r, Err(CloudError::NotConfigured(_))), "{bad}");
        }
        // Local development.
        assert!(
            CloudConfig::from_lookup(lookup(&[
                (ENV_SUPABASE_URL, "http://127.0.0.1:54321"),
                (ENV_SUPABASE_ANON_KEY, "anon"),
                (ENV_CLOUD_URL, "http://localhost:8080"),
            ]))
            .is_ok()
        );
    }
}
