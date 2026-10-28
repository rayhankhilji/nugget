//! Microsoft identity platform (Entra ID, v2.0 endpoints) preset for [`nugget_oauth`].
//!
//! Works with the user's own app registration as a public client ("Mobile and desktop
//! applications" platform, redirect `http://localhost`): PKCE, no client secret. A secret is
//! sent only when the user supplied one (a "Web" platform registration).

use nugget_core::{Error, Result};
use nugget_oauth::Provider;

pub const LOGIN_BASE: &str = "https://login.microsoftonline.com";

/// Needed for a refresh token.
pub const SCOPE_OFFLINE_ACCESS: &str = "offline_access";
/// Sign-in and basic profile; lets the consent screen name the account.
pub const SCOPE_USER_READ: &str = "User.Read";
pub const SCOPE_MAIL_READ: &str = "Mail.Read";
pub const SCOPE_CALENDARS_READ: &str = "Calendars.Read";
pub const SCOPE_FILES_READ: &str = "Files.Read";

/// Every scope the three connectors use. Each connector asks only for its own plus
/// `offline_access` and `User.Read`; an app registration should allow all of these.
pub const ALL_SCOPES: &[&str] = &[
    SCOPE_OFFLINE_ACCESS,
    SCOPE_MAIL_READ,
    SCOPE_CALENDARS_READ,
    SCOPE_FILES_READ,
    SCOPE_USER_READ,
];

/// The default tenant: work, school and personal Microsoft accounts.
pub const DEFAULT_TENANT: &str = "common";

/// `common`, `organizations`, `consumers`, a tenant id (GUID) or a verified domain
/// (`contoso.onmicrosoft.com`). Anything else could alter the endpoint URL and is refused.
pub fn validate_tenant(tenant: &str) -> Result<()> {
    let ok = !tenant.is_empty()
        && tenant.len() <= 253
        && !tenant.starts_with(['.', '-'])
        && !tenant.ends_with(['.', '-'])
        && !tenant.contains("..")
        && tenant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "settings: tenant `{}` is not `common`, `organizations`, `consumers`, a tenant id or a domain",
            tenant.chars().take(64).collect::<String>()
        )))
    }
}

/// `<login>/<tenant>/oauth2/v2.0/authorize`.
pub fn auth_url(login: &str, tenant: &str) -> String {
    format!(
        "{}/{tenant}/oauth2/v2.0/authorize",
        login.trim_end_matches('/')
    )
}

/// `<login>/<tenant>/oauth2/v2.0/token`.
pub fn token_url(login: &str, tenant: &str) -> String {
    format!("{}/{tenant}/oauth2/v2.0/token", login.trim_end_matches('/'))
}

/// The Microsoft identity platform for `tenant` with the given scopes.
pub fn provider(tenant: &str, scopes: &[&str]) -> Result<Provider> {
    provider_with_login(LOGIN_BASE, tenant, scopes)
}

/// Same as [`provider`] with another login root (tests, sovereign clouds such as
/// `https://login.microsoftonline.us`).
pub fn provider_with_login(login: &str, tenant: &str, scopes: &[&str]) -> Result<Provider> {
    validate_tenant(tenant)?;
    Ok(Provider {
        name: "microsoft".into(),
        auth_url: auth_url(login, tenant),
        token_url: token_url(login, tenant),
        scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        // The code comes back in the query string (what the loopback listener reads), and the
        // account picker is shown so a second account can be connected.
        extra_auth_params: vec![
            ("response_mode".into(), "query".into()),
            ("prompt".into(), "select_account".into()),
        ],
    })
}
