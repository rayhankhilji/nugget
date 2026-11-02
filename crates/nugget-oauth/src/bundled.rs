//! Nugget's own public OAuth clients ("Connect" with no setup), baked in at build time.
//!
//! Release builds set `NUGGET_OAUTH_<PROVIDER>_CLIENT_ID` (and, for Google only, the
//! `..._CLIENT_SECRET` that Google issues to desktop clients; it is not confidential for
//! installed apps). Builds without them (source builds, tests) have no built-in client and the
//! user brings their own. A client the user configured always wins: see [`resolve`].

use crate::ClientCredentials;

/// Providers that can carry a built-in client.
pub const PROVIDERS: &[&str] = &["google", "microsoft", "github", "linear", "slack"];

fn non_empty(v: Option<&'static str>) -> Option<&'static str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// The built-in client for `provider` (`google`, `microsoft`, `github`, `linear`, `slack`),
/// if this build has one.
pub fn client(provider: &str) -> Option<ClientCredentials> {
    let (id, secret) = match provider {
        "google" => (
            option_env!("NUGGET_OAUTH_GOOGLE_CLIENT_ID"),
            option_env!("NUGGET_OAUTH_GOOGLE_CLIENT_SECRET"),
        ),
        "microsoft" => (option_env!("NUGGET_OAUTH_MICROSOFT_CLIENT_ID"), None),
        "github" => (option_env!("NUGGET_OAUTH_GITHUB_CLIENT_ID"), None),
        "linear" => (option_env!("NUGGET_OAUTH_LINEAR_CLIENT_ID"), None),
        "slack" => (option_env!("NUGGET_OAUTH_SLACK_CLIENT_ID"), None),
        _ => (None, None),
    };
    let id = non_empty(id)?;
    Some(ClientCredentials::new(
        id,
        non_empty(secret).map(str::to_string),
    ))
}

/// Providers with a built-in client in this build.
pub fn available() -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .copied()
        .filter(|p| client(p).is_some())
        .collect()
}

/// The client to use: the user's own (`user_id` with `user_secret`) when set, else `builtin`.
/// The built-in secret never goes with the user's id, nor the user's secret with the built-in id.
pub fn resolve(
    user_id: Option<&str>,
    user_secret: Option<String>,
    builtin: Option<ClientCredentials>,
) -> Option<ClientCredentials> {
    match user_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => Some(ClientCredentials::new(id, user_secret)),
        None => builtin,
    }
}
