//! Google preset: endpoints and scopes for Google's "Desktop app" OAuth clients.

use crate::Provider;

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const SCOPE_GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";

/// Google with the given scopes. Asks for offline access and forces the consent screen so a
/// refresh token is always issued (Google only returns one on first consent otherwise).
pub fn provider(scopes: &[&str]) -> Provider {
    provider_with_endpoints(AUTH_URL, TOKEN_URL, scopes)
}

/// Same as [`provider`] with other endpoints (tests, proxies).
pub fn provider_with_endpoints(auth_url: &str, token_url: &str, scopes: &[&str]) -> Provider {
    Provider {
        name: "google".into(),
        auth_url: auth_url.into(),
        token_url: token_url.into(),
        scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        extra_auth_params: vec![
            ("access_type".into(), "offline".into()),
            ("prompt".into(), "consent".into()),
        ],
    }
}
