//! One naming scheme for OS keychain entries, shared by every crate.

/// Keychain service name for all Heed secrets.
pub const KEYCHAIN_SERVICE: &str = "app.heed.Heed";
/// The service before the rename; entries move from it on first read
/// ([`crate::migrate::keychain_get`]).
pub const LEGACY_KEYCHAIN_SERVICE: &str = "dev.nugget.Nugget";
/// The cloud session's service before the rename (its entries now live under
/// [`KEYCHAIN_SERVICE`] too).
pub const LEGACY_CLOUD_KEYCHAIN_SERVICE: &str = "nugget";

/// Account name for a provider API key referenced as `keychain:NAME` in config.
pub fn provider_account(name: &str) -> String {
    format!("provider:{name}")
}

/// Account name for a source's OAuth token set (JSON), e.g. a Gmail connection.
pub fn oauth_account(source: &str) -> String {
    format!("oauth:{source}")
}

/// Account name for a source's password or app password (IMAP, CalDAV).
pub fn password_account(source: &str) -> String {
    format!("password:{source}")
}
