//! One naming scheme for OS keychain entries, shared by every crate.

/// Keychain service name for all Nugget secrets.
pub const KEYCHAIN_SERVICE: &str = "dev.nugget.Nugget";

/// Account name for a provider API key referenced as `keychain:NAME` in config.
pub fn provider_account(name: &str) -> String {
    format!("provider:{name}")
}
