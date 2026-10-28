//! Sources: the connector registry, settings reconciliation and credential routing.
//!
//! **Secrets.** `sources.secret.set` stores, by the kind's [`AuthKind`]:
//! - `Password` (imap, caldav): the password under `password:<id>` ([`Ingest::set_password`]);
//! - `OAuth` (gmail, gcal): the OAuth **client secret** under `oauth_client_secret:<id>`. It is
//!   never persisted in `sources.toml`: [`client_secret_resolver`] adds it to the settings handed
//!   to `ConnectorFactory::build` as `client_secret`, and the source is rebuilt
//!   ([`Ingest::reload`]) after it changes. `client_secret` in `sources.add`/`update` settings is
//!   refused.
//!
//! Every account is written through [`Ingest::set_secret`], which records the account *name* in
//! `sources.toml`, so removing the source deletes it (the daemon also deletes
//! `oauth_client_secret:<id>` explicitly). Secret values never reach logs, audit entries, IPC
//! responses or notifications.

use std::sync::Arc;

use nugget_connector::{AuthKind, Registry, SecretStore};
use nugget_core::ids::SourceId;
use nugget_core::{Error, Result};
use nugget_ingest::{Ingest, SettingsResolver};
use serde_json::Value;

/// Settings key the Google connectors read the client secret from (injected, never stored).
pub(crate) const CLIENT_SECRET_FIELD: &str = "client_secret";

/// Keychain account of a source's OAuth client secret.
pub(crate) fn client_secret_account(id: &SourceId) -> String {
    format!("oauth_client_secret:{id}")
}

/// Every connector kind shipped in M1: files, obsidian, gmail, gcal, imap, caldav.
pub(crate) fn registry() -> Registry {
    let mut r = Registry::new();
    nugget_connector_files::register(&mut r);
    nugget_connector_google::register(&mut r);
    r.register(Arc::new(nugget_connector_imap::ImapFactory::new()));
    r.register(Arc::new(nugget_connector_caldav::CalDavFactory::new()));
    r
}

/// How a kind authenticates (`None` for unknown kinds).
pub(crate) fn auth_kind(ingest: &Ingest, kind: &str) -> Option<AuthKind> {
    ingest
        .kinds()
        .into_iter()
        .find(|k| k.kind == kind)
        .map(|k| k.auth)
}

/// Bring settings sent by clients in line with the connectors' schemas, and refuse secrets.
///
/// - `files`: an empty `include` list means "the default types" (the desktop sends `[]` when
///   the field is left blank; the connector requires a non-empty list or none), so it is
///   dropped. An empty `exclude` is fine.
/// - Any kind: a non-null `client_secret` is refused (use `sources.secret.set`).
pub(crate) fn reconcile_settings(kind: &str, settings: Value) -> Result<Value> {
    let mut settings = settings;
    if let Value::Object(map) = &mut settings {
        if map.get(CLIENT_SECRET_FIELD).is_some_and(|v| !v.is_null()) {
            return Err(Error::Invalid(
                "settings must not contain `client_secret`; set it with sources.secret.set".into(),
            ));
        }
        map.remove(CLIENT_SECRET_FIELD);
        if kind == nugget_connector_files::FILES_KIND
            && map
                .get("include")
                .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty) || v.is_null())
        {
            map.remove("include");
        }
    }
    Ok(settings)
}

/// Injects `oauth_client_secret:<id>` as `client_secret` into OAuth kinds' settings at build time.
pub(crate) fn client_secret_resolver(
    secrets: Arc<dyn SecretStore>,
    oauth_kinds: Vec<String>,
) -> SettingsResolver {
    Arc::new(move |id, kind, mut settings| {
        if !oauth_kinds.iter().any(|k| k == kind) {
            return Ok(settings);
        }
        match secrets.get(&client_secret_account(id)) {
            Ok(Some(secret)) if !secret.is_empty() => {
                if let Value::Object(map) = &mut settings {
                    map.insert(CLIENT_SECRET_FIELD.into(), Value::String(secret));
                }
            }
            Ok(_) => {}
            // Build without it; the token exchange then reports what is missing.
            Err(e) => {
                tracing::warn!(source = %id, error = %e, "cannot read the OAuth client secret");
            }
        }
        Ok(settings)
    })
}

/// Kinds that authenticate with OAuth.
pub(crate) fn oauth_kinds(registry: &Registry) -> Vec<String> {
    registry
        .kinds()
        .into_iter()
        .filter(|k| k.auth == AuthKind::OAuth)
        .map(|k| k.kind.to_string())
        .collect()
}

/// Whether `NUGGET_KEYCHAIN` turns the OS keychain off (`0`, `off`, `false`, `no`).
pub(crate) fn keychain_disabled() -> bool {
    std::env::var(nugget_store::KEYCHAIN_ENV).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_ingest::MemorySecrets;
    use serde_json::json;

    #[test]
    fn registers_every_m1_kind() {
        let r = registry();
        let mut kinds: Vec<&str> = r.kinds().iter().map(|k| k.kind).collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            ["caldav", "files", "gcal", "gmail", "imap", "obsidian"]
        );
        let mut oauth = oauth_kinds(&r);
        oauth.sort();
        assert_eq!(oauth, ["gcal", "gmail"]);
    }

    /// The settings the desktop wizard sends validate against each connector.
    #[test]
    fn desktop_wizard_settings_validate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let r = registry();
        let cases = [
            (
                "files",
                json!({"roots": [root], "include": [], "exclude": []}),
            ),
            (
                "files",
                json!({"roots": [root], "include": ["**/*.md"], "exclude": ["**/tmp/**"]}),
            ),
            ("obsidian", json!({"vault": root})),
            (
                "gmail",
                json!({"client_id": "123-abc.apps.googleusercontent.com"}),
            ),
            (
                "gcal",
                json!({"client_id": "123-abc.apps.googleusercontent.com"}),
            ),
            (
                "imap",
                json!({"host": "imap.example.com", "port": 993, "tls": "implicit", "username": "me@example.com", "folders": ["INBOX", "Archive"]}),
            ),
            (
                "imap",
                json!({"host": "imap.example.com", "port": 143, "tls": "starttls", "username": "me", "folders": ["INBOX"]}),
            ),
            (
                "caldav",
                json!({"url": "https://caldav.example.com/", "username": "me"}),
            ),
        ];
        for (kind, settings) in cases {
            let s = reconcile_settings(kind, settings.clone()).unwrap();
            r.get(kind)
                .unwrap()
                .validate(&s)
                .unwrap_or_else(|e| panic!("{kind} {settings}: {e}"));
        }
    }

    #[test]
    fn client_secret_is_refused_in_settings() {
        let err = reconcile_settings("gmail", json!({"client_id": "x", "client_secret": "s"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sources.secret.set"), "{err}");
        assert!(!err.contains("\"s\""));
        assert_eq!(
            reconcile_settings("gmail", json!({"client_id": "x", "client_secret": null})).unwrap(),
            json!({"client_id": "x"})
        );
    }

    #[test]
    fn resolver_injects_only_for_oauth_kinds() {
        let mem = MemorySecrets::new();
        let id = SourceId::new("gmail-1");
        mem.set(&client_secret_account(&id), "cs").unwrap();
        let resolve = client_secret_resolver(Arc::new(mem), vec!["gmail".into()]);
        assert_eq!(
            resolve(&id, "gmail", json!({"client_id": "x"})).unwrap(),
            json!({"client_id": "x", "client_secret": "cs"})
        );
        assert_eq!(
            resolve(&id, "imap", json!({"host": "h"})).unwrap(),
            json!({"host": "h"})
        );
        let other = SourceId::new("gmail-2");
        assert_eq!(
            resolve(&other, "gmail", json!({"client_id": "x"})).unwrap(),
            json!({"client_id": "x"})
        );
        // The account belongs to the source under ingest's scoping rule, so ingest records it
        // and deletes it when the source is removed.
        assert!(nugget_ingest::account_belongs_to(
            &client_secret_account(&id),
            &id
        ));
    }
}
