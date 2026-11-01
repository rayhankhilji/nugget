//! Sources: the connector registry, settings reconciliation and credential routing.
//!
//! `sources.secret.set` stores, by [`AuthKind`]: `Password` kinds' password or API token under
//! `password:<id>`; `OAuth` kinds' **client secret** under `oauth_client_secret:<id>`, never in
//! `sources.toml` ([`client_secret_resolver`] injects it into the settings at build time, and
//! `client_secret` in `sources.add`/`update` settings is refused). OAuth tokens live under
//! `oauth:<id>`. Accounts are written through [`Ingest::set_secret`] so removing the source deletes
//! them. Secret values never reach logs, audit entries, IPC responses or notifications.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use nugget_connector::{AuthKind, Registry, SecretStore};
use nugget_core::ids::SourceId;
use nugget_core::ipc::SourceInfo;
use nugget_core::{Error, Result};
use nugget_ingest::{Ingest, SettingsResolver};
use serde_json::Value;

/// Settings key the Google connectors read the client secret from (injected, never stored).
pub(crate) const CLIENT_SECRET_FIELD: &str = "client_secret";

/// Keychain account of a source's OAuth client secret.
pub(crate) fn client_secret_account(id: &SourceId) -> String {
    format!("oauth_client_secret:{id}")
}

/// Kinds whose optional settings fields may arrive blank from a form (see
/// [`reconcile_settings`]).
const FORM_NORMALISED_KINDS: &[&str] = &[
    "outlook",
    "outlook-calendar",
    "onedrive",
    nugget_connector_saas::SLACK_KIND,
    nugget_connector_saas::NOTION_KIND,
    nugget_connector_saas::GITHUB_KIND,
    nugget_connector_saas::LINEAR_KIND,
];

/// Every connector kind shipped.
pub(crate) fn registry() -> Registry {
    let mut r = Registry::new();
    nugget_connector_files::register(&mut r);
    nugget_connector_google::register(&mut r);
    r.register(Arc::new(nugget_connector_imap::ImapFactory::new()));
    r.register(Arc::new(nugget_connector_caldav::CalDavFactory::new()));
    nugget_connector_microsoft::register(&mut r);
    nugget_connector_saas::register(&mut r);
    nugget_connector_messaging::register(&mut r);
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
/// - Microsoft 365 and SaaS kinds: top-level fields that are `null`, blank strings or empty
///   arrays are dropped, so blank form fields fall back to the connector's default (their
///   schemas reject an empty `client_id`, `tenant` or `token_env` and an empty Linear
///   `include`; an empty list elsewhere means the same as no list).
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
        if FORM_NORMALISED_KINDS.contains(&kind) {
            map.retain(|_, v| match v {
                Value::Null => false,
                Value::String(s) => !s.trim().is_empty(),
                Value::Array(a) => !a.is_empty(),
                _ => true,
            });
        }
    }
    Ok(settings)
}

/// The folders of a `files` source's settings, `~` expanded and made comparable (see
/// [`comparable`]). Relative paths are left out: validation refuses them with its own message.
fn files_roots(settings: &Value) -> Vec<PathBuf> {
    let Some(roots) = settings.get("roots").and_then(Value::as_array) else {
        return Vec::new();
    };
    roots
        .iter()
        .filter_map(Value::as_str)
        .filter(|r| !r.trim().is_empty())
        .map(|r| nugget_connector_files::expand_home(Path::new(r)))
        .filter(|p| p.is_absolute())
        .map(|p| comparable(&p))
        .collect()
}

/// `.` dropped and `..` applied, without touching the filesystem.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A path in a form two spellings of the same folder agree on: the deepest existing ancestor
/// canonicalized (symlinks such as macOS's `/var` → `/private/var` resolved) with the rest
/// appended, Windows `\\?\` prefixes stripped, and case folded where file systems usually
/// ignore case (Windows, macOS).
fn comparable(p: &Path) -> PathBuf {
    let p = lexical(p);
    let mut out = p.clone();
    for a in p.ancestors() {
        if let Ok(c) = std::fs::canonicalize(a) {
            out = match p.strip_prefix(a) {
                Ok(rest) if !rest.as_os_str().is_empty() => c.join(rest),
                _ => c,
            };
            break;
        }
    }
    let out = strip_verbatim(out);
    if cfg!(any(windows, target_os = "macos")) {
        PathBuf::from(out.to_string_lossy().to_lowercase())
    } else {
        out
    }
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share` → `\\server\share` (what
/// `canonicalize` returns on Windows); other paths unchanged.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else {
        return p;
    };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    p
}

/// GG6-13: refuse a `files` source whose folders equal, contain or lie inside a folder of
/// another `files` source: its items would be indexed twice. For an update, `current` is the
/// source being updated: it is not compared with itself, and only folders not already in its
/// stored settings are checked (so an existing overlap never makes a source uneditable).
pub(crate) fn check_duplicate_folders(
    kind: &str,
    settings: &Value,
    existing: &[SourceInfo],
    current: Option<&SourceInfo>,
) -> Result<()> {
    if kind != nugget_connector_files::FILES_KIND {
        return Ok(());
    }
    let before = current
        .map(|c| files_roots(&c.settings))
        .unwrap_or_default();
    let new: Vec<PathBuf> = files_roots(settings)
        .into_iter()
        .filter(|r| !before.contains(r))
        .collect();
    if new.is_empty() {
        return Ok(());
    }
    for other in existing {
        if other.kind != nugget_connector_files::FILES_KIND
            || current.is_some_and(|c| c.id == other.id)
        {
            continue;
        }
        for theirs in files_roots(&other.settings) {
            for mine in &new {
                let name = &other.name;
                let msg = if *mine == theirs {
                    format!("This folder is already added as \"{name}\".")
                } else if mine.starts_with(&theirs) {
                    format!("This folder is inside \"{name}\", which is already added.")
                } else if theirs.starts_with(mine) {
                    format!(
                        "This folder contains \"{name}\", which is already added. Remove that \
                         source first or pick a different folder."
                    )
                } else {
                    continue;
                };
                return Err(Error::Invalid(msg));
            }
        }
    }
    Ok(())
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

    fn files(id: &str, name: &str, roots: &[&Path]) -> SourceInfo {
        serde_json::from_value(json!({
            "id": id, "kind": "files", "name": name, "enabled": true, "state": "idle",
            "items": 0, "chunks": 0, "settings": { "roots": roots },
        }))
        .unwrap()
    }

    #[test]
    fn duplicate_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("Docs");
        let inner = docs.join("Notes");
        let other = dir.path().join("Other");
        for d in [&inner, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let existing = [files("files-1", "My docs", &[&docs])];
        let err = |roots: Value| match check_duplicate_folders(
            "files",
            &json!({ "roots": roots }),
            &existing,
            None,
        ) {
            Err(Error::Invalid(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert_eq!(
            err(json!([docs])),
            "This folder is already added as \"My docs\"."
        );
        // The same folder spelled differently (trailing `/.`, `..`) is still the same folder.
        assert_eq!(
            err(json!([docs.join("Notes").join("..").join(".")])),
            "This folder is already added as \"My docs\"."
        );
        assert_eq!(
            err(json!([inner])),
            "This folder is inside \"My docs\", which is already added."
        );
        assert_eq!(
            err(json!([dir.path()])),
            "This folder contains \"My docs\", which is already added. Remove that source \
             first or pick a different folder."
        );
        // A missing folder inside an existing one: its existing ancestor is canonicalized.
        assert!(err(json!([docs.join("not-yet").join("deeper")])).contains("is inside"));
        // Relative roots are left to validation's own message.
        assert!(
            check_duplicate_folders("files", &json!({ "roots": ["Docs"] }), &existing, None)
                .is_ok()
        );

        // Disjoint folders, other kinds, and the source being updated itself are fine.
        assert!(
            check_duplicate_folders("files", &json!({ "roots": [other] }), &existing, None).is_ok()
        );
        assert!(
            check_duplicate_folders("obsidian", &json!({ "vault": docs }), &existing, None).is_ok()
        );
        let me = &existing[0];
        assert!(
            check_duplicate_folders("files", &json!({ "roots": [inner] }), &existing, Some(me))
                .is_ok()
        );
        // Sources that already overlap stay editable: an update only checks folders it adds.
        let existing = [existing[0].clone(), files("files-2", "Notes", &[&inner])];
        let me = Some(&existing[1]);
        let more = json!({ "roots": [inner, other] });
        assert!(check_duplicate_folders("files", &more, &existing, me).is_ok());
        let worse = json!({ "roots": [inner, docs.join("Else")] });
        assert!(check_duplicate_folders("files", &worse, &existing, me).is_err());
    }

    /// As on macOS (`/var` → `/private/var`): a folder spelled through a symlink, or missing
    /// under a symlinked ancestor, is still a duplicate.
    #[cfg(unix)]
    #[test]
    fn duplicate_folders_through_a_symlink_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("private").join("var");
        std::fs::create_dir_all(real.join("Docs")).unwrap();
        let var = dir.path().join("var");
        std::os::unix::fs::symlink(&real, &var).unwrap();
        let existing = [files("files-1", "Docs", &[&real.join("Docs")])];
        let r = check_duplicate_folders(
            "files",
            &json!({ "roots": [var.join("Docs")] }),
            &existing,
            None,
        );
        assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("already added as")));
        let missing = var.join("Docs").join("not-yet");
        let r = check_duplicate_folders("files", &json!({ "roots": [missing] }), &existing, None);
        assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("is inside")));
        // And the other way round: stored through the link, new one under the real path.
        let existing = [files("files-1", "Docs", &[&var.join("Docs")])];
        let missing = real.join("Docs").join("not-yet");
        let r = check_duplicate_folders("files", &json!({ "roots": [missing] }), &existing, None);
        assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("is inside")));
    }

    const ENTRA_APP: &str = "00000000-1111-2222-3333-444444444444";

    /// The settings the desktop wizard sends validate against each connector, for every kind.
    #[test]
    fn desktop_wizard_settings_validate_for_every_kind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let chat_db = dir.path().join("chat.db");
        std::fs::write(&chat_db, b"").unwrap();
        let chat_db = chat_db.to_str().unwrap().to_string();
        let r = registry();
        let cases = [
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
                "gdrive",
                json!({"client_id": "123-abc.apps.googleusercontent.com"}),
            ),
            (
                "imap",
                json!({"host": "imap.example.com", "port": 993, "tls": "implicit", "username": "me@example.com", "folders": ["INBOX", "Archive"]}),
            ),
            (
                "caldav",
                json!({"url": "https://caldav.example.com/", "username": "me"}),
            ),
            // Microsoft 365: `{client_id, tenant?}` plus kind-specific optional fields; blank
            // strings and empty lists from untouched form fields are dropped.
            (
                "outlook",
                json!({"client_id": ENTRA_APP, "tenant": "", "account_hint": "", "folders": []}),
            ),
            (
                "outlook-calendar",
                json!({"client_id": ENTRA_APP, "tenant": "contoso.onmicrosoft.com", "past_days": 30, "future_days": 0}),
            ),
            (
                "onedrive",
                json!({"client_id": ENTRA_APP, "tenant": " ", "include_folders": [], "exclude_folders": []}),
            ),
            // SaaS: `{}` or optional fields; the token goes through sources.secret.set.
            (
                "slack",
                json!({"token_env": "", "conversations": [], "include_dms": true}),
            ),
            (
                "notion",
                json!({"token_env": "", "include_databases": false, "max_pages": 100}),
            ),
            (
                "github",
                json!({"client_id": "", "repos": [], "token_env": ""}),
            ),
            (
                "linear",
                json!({"client_id": "", "include": [], "teams": [], "token_env": null}),
            ),
            // The wizard sends no `db_path` (macOS default); a file stands in for chat.db here.
            (
                "imessage",
                json!({"db_path": chat_db, "contacts": true, "include_groups": true, "private": true}),
            ),
            (
                "whatsapp",
                json!({"folder": root, "include_groups": true, "private": true}),
            ),
            (
                "telegram",
                json!({"path": root, "include_groups": true, "private": true}),
            ),
        ];
        let kinds: std::collections::BTreeSet<&str> = cases.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds.len(), r.kinds().len(), "every kind has a case");
        for (kind, settings) in cases {
            let s = reconcile_settings(kind, settings.clone()).unwrap();
            r.get(kind)
                .unwrap()
                .validate(&s)
                .unwrap_or_else(|e| panic!("{kind} {settings}: {e}"));
        }
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
