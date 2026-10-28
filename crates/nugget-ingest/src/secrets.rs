//! Credential storage for sources.
//!
//! - [`KeyringSecrets`]: the OS keychain (service [`KEYCHAIN_SERVICE`]), keyed by the full
//!   account name, e.g. `password:imap-1` or `oauth:gmail-1` (see [`nugget_core::secrets`]).
//! - [`MemorySecrets`]: an in-memory map for tests and headless runs.
//! - `ScopedSecrets` (internal): what a connector receives. It only lets a source touch accounts
//!   named `<prefix>:<source id>` or `<prefix>:<source id>:<suffix>` (never `provider:*`), and
//!   remembers which accounts were written so removing the source can delete them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nugget_connector::SecretStore;
use nugget_core::ids::SourceId;
use nugget_core::secrets::KEYCHAIN_SERVICE;
use nugget_core::{Error, Result};

/// Secrets in the OS keychain under service [`KEYCHAIN_SERVICE`].
///
/// Calls block (D-Bus round trips on Linux); call from a blocking context where possible.
#[derive(Debug, Clone)]
pub struct KeyringSecrets {
    service: String,
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringSecrets {
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
        }
    }

    /// A different service name (tests against a real keychain).
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
))]
mod os {
    use super::*;

    fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(service, account).map_err(map_err)
    }

    fn map_err(e: keyring::Error) -> Error {
        // Never include the secret; keyring errors do not carry it.
        Error::Storage(format!("keychain: {e}"))
    }

    impl SecretStore for KeyringSecrets {
        fn get(&self, account: &str) -> Result<Option<String>> {
            match entry(&self.service, account)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map_err(e)),
            }
        }

        fn set(&self, account: &str, value: &str) -> Result<()> {
            entry(&self.service, account)?
                .set_password(value)
                .map_err(map_err)
        }

        fn delete(&self, account: &str) -> Result<()> {
            match entry(&self.service, account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(map_err(e)),
            }
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
)))]
impl SecretStore for KeyringSecrets {
    fn get(&self, _account: &str) -> Result<Option<String>> {
        Err(Error::Unsupported("no OS keychain on this platform".into()))
    }
    fn set(&self, _account: &str, _value: &str) -> Result<()> {
        Err(Error::Unsupported("no OS keychain on this platform".into()))
    }
    fn delete(&self, _account: &str) -> Result<()> {
        Ok(())
    }
}

/// In-memory secrets (tests). Cloning shares the map.
#[derive(Clone, Default)]
pub struct MemorySecrets {
    map: Arc<Mutex<HashMap<String, String>>>,
}

impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.map.lock().map(|m| m.len()).unwrap_or_default();
        f.debug_struct("MemorySecrets")
            .field("entries", &n)
            .finish()
    }
}

impl MemorySecrets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Account names currently stored (values are never exposed for listing).
    pub fn accounts(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account);
        Ok(())
    }
}

/// Whether `account` belongs to `source`: `<prefix>:<source>` or `<prefix>:<source>:<rest>`,
/// with a non-empty prefix free of `:` and not `provider`.
pub fn account_belongs_to(account: &str, source: &SourceId) -> bool {
    let Some((prefix, rest)) = account.split_once(':') else {
        return false;
    };
    if prefix.is_empty() || prefix == "provider" {
        return false;
    }
    let id = source.as_str();
    rest == id
        || rest
            .strip_prefix(id)
            .is_some_and(|tail| tail.starts_with(':') && tail.len() > 1)
}

/// Default accounts every source may own (deleted on removal even if never recorded).
pub(crate) fn default_accounts(source: &SourceId) -> [String; 2] {
    [
        nugget_core::secrets::oauth_account(source.as_str()),
        nugget_core::secrets::password_account(source.as_str()),
    ]
}

/// Called with an account name the first time a source writes it.
pub(crate) type OnAccountWritten = Arc<dyn Fn(&SourceId, &str) + Send + Sync>;

/// The [`SecretStore`] handed to one source's connector.
pub(crate) struct ScopedSecrets {
    pub(crate) source: SourceId,
    pub(crate) inner: Arc<dyn SecretStore>,
    pub(crate) on_written: OnAccountWritten,
}

impl ScopedSecrets {
    fn check(&self, account: &str) -> Result<()> {
        if account_belongs_to(account, &self.source) {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "source {} may not access keychain account `{account}`",
                self.source
            )))
        }
    }
}

impl SecretStore for ScopedSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        self.check(account)?;
        self.inner.get(account)
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.check(account)?;
        self.inner.set(account, value)?;
        (self.on_written)(&self.source, account);
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.check(account)?;
        self.inner.delete(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoping_rules() {
        let s = SourceId::new("imap-1");
        assert!(account_belongs_to("password:imap-1", &s));
        assert!(account_belongs_to("oauth:imap-1", &s));
        assert!(account_belongs_to("oauth:imap-1:client", &s));
        assert!(!account_belongs_to("oauth:imap-10", &s));
        assert!(!account_belongs_to("oauth:imap-1:", &s));
        assert!(!account_belongs_to("password:imap-2", &s));
        assert!(!account_belongs_to("provider:imap-1", &s));
        assert!(!account_belongs_to("imap-1", &s));
        assert!(!account_belongs_to(":imap-1", &s));
    }

    #[test]
    fn scoped_store_enforces_scope_and_reports_writes() {
        let mem = MemorySecrets::new();
        let written = Arc::new(Mutex::new(Vec::<String>::new()));
        let w = written.clone();
        let scoped = ScopedSecrets {
            source: SourceId::new("gmail-1"),
            inner: Arc::new(mem.clone()),
            on_written: Arc::new(move |_, a| w.lock().unwrap().push(a.to_owned())),
        };
        scoped.set("oauth:gmail-1", "tok").unwrap();
        assert_eq!(scoped.get("oauth:gmail-1").unwrap().as_deref(), Some("tok"));
        assert!(scoped.get("provider:anthropic").is_err());
        assert!(scoped.set("oauth:gmail-2", "x").is_err());
        assert_eq!(*written.lock().unwrap(), vec!["oauth:gmail-1".to_owned()]);
        scoped.delete("oauth:gmail-1").unwrap();
        assert!(mem.accounts().is_empty());
    }
}
