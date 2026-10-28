//! OAuth 2.0 for Nugget connectors: the authorization-code flow with PKCE (S256) for installed
//! apps (RFC 8252), using a loopback redirect, plus token storage and automatic refresh.
//!
//! - [`OAuthClient::authorize`] binds `127.0.0.1` on a random port, builds the authorization URL
//!   (with `state` and a PKCE challenge) and returns an [`Authorization`]; its
//!   [`finish`](Authorization::finish) waits for the browser redirect (one callback, 5 minute
//!   timeout by default), validates `state` and exchanges the code.
//! - [`OAuthClient::begin`] wraps that into a [`nugget_connector::AuthFlow`] that stores the
//!   tokens as JSON in the [`SecretStore`](nugget_connector::SecretStore) when done.
//! - [`TokenSource::access_token`] loads the tokens, refreshes them shortly before expiry and
//!   writes the refreshed set back. A rejected refresh (`invalid_grant`) is `Error::Unauthorized`.
//!
//! All token-endpoint traffic goes through [`MeteredHttp`](nugget_connector::http::MeteredHttp),
//! so it lands in the egress ledger and never follows redirects. Client secrets and tokens are
//! redacted from `Debug` output and never logged.

mod client;
pub mod google;
mod loopback;
mod pkce;
mod token;

pub use client::{Authorization, AuthorizeOptions, OAuthClient};
pub use pkce::Pkce;
pub use token::{TokenSet, TokenSource};

use serde::{Deserialize, Serialize};

/// Default time to wait for the browser redirect.
pub const DEFAULT_AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// An OAuth provider: endpoints, scopes and extra authorization parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Short name, used in messages (e.g. `google`).
    pub name: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    /// Extra query parameters on the authorization URL (e.g. Google's `access_type=offline`).
    #[serde(default)]
    pub extra_auth_params: Vec<(String, String)>,
}

/// The user's own OAuth client. For installed apps the secret (when the provider issues one, as
/// Google does) is not confidential, but it is still never logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCredentials {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl ClientCredentials {
    pub fn new(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.filter(|s| !s.is_empty()),
        }
    }
}

impl std::fmt::Debug for ClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCredentials")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests;
