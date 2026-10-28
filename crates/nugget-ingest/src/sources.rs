//! `<config>/sources.toml`: the persisted list of configured sources.
//!
//! ```toml
//! version = 1
//!
//! [[sources]]
//! id = "files-1"
//! kind = "files"
//! name = "Documents"
//! enabled = true
//! secret_accounts = []          # keychain account *names* only, never values
//!
//! [sources.settings]
//! roots = ["/home/me/Documents"]
//! ```
//!
//! Written atomically (temp file + fsync + rename) with mode 0600 on Unix. Settings must not
//! contain secrets: well-known secret field names are rejected.

use std::io::Write;
use std::path::{Path, PathBuf};

use nugget_core::ids::SourceId;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const FILE_NAME: &str = "sources.toml";
const FORMAT_VERSION: u32 = 1;

/// One configured source as persisted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SourceEntry {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    /// JSON object.
    pub settings: Value,
    /// Keychain account names written for this source (for cleanup on removal).
    pub secret_accounts: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileRepr {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    sources: Vec<EntryRepr>,
}

fn default_version() -> u32 {
    FORMAT_VERSION
}

#[derive(Debug, Serialize, Deserialize)]
struct EntryRepr {
    id: String,
    kind: String,
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    secret_accounts: Vec<String>,
    #[serde(default)]
    settings: toml::Table,
}

fn default_true() -> bool {
    true
}

pub(crate) fn path(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE_NAME)
}

/// Load the file; a missing file is an empty list.
pub(crate) fn load(file: &Path) -> Result<Vec<SourceEntry>> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let repr: FileRepr =
        toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", file.display())))?;
    if repr.version > FORMAT_VERSION {
        return Err(Error::Invalid(format!(
            "{}: format version {} is newer than this build supports ({FORMAT_VERSION})",
            file.display(),
            repr.version
        )));
    }
    let mut out = Vec::with_capacity(repr.sources.len());
    for e in repr.sources {
        let id = SourceId::new(e.id);
        validate_id(&id)?;
        if out.iter().any(|o: &SourceEntry| o.id == id) {
            return Err(Error::Invalid(format!(
                "{}: duplicate source id `{id}`",
                file.display()
            )));
        }
        out.push(SourceEntry {
            id,
            kind: e.kind,
            name: e.name,
            enabled: e.enabled,
            settings: serde_json::to_value(toml::Value::Table(e.settings))?,
            secret_accounts: e.secret_accounts,
        });
    }
    Ok(out)
}

/// Atomically replace the file with `entries`.
pub(crate) fn save(file: &Path, entries: &[SourceEntry]) -> Result<()> {
    let mut repr = FileRepr {
        version: FORMAT_VERSION,
        sources: Vec::with_capacity(entries.len()),
    };
    for e in entries {
        let settings = match json_to_toml(&e.settings)? {
            Some(toml::Value::Table(t)) => t,
            None => toml::Table::new(),
            Some(_) => return Err(Error::Invalid("source settings must be an object".into())),
        };
        repr.sources.push(EntryRepr {
            id: e.id.as_str().to_owned(),
            kind: e.kind.clone(),
            name: e.name.clone(),
            enabled: e.enabled,
            secret_accounts: e.secret_accounts.clone(),
            settings,
        });
    }
    let text = toml::to_string(&repr).map_err(|e| Error::Other(format!("sources.toml: {e}")))?;
    let header =
        "# Nugget sources. Managed by nuggetd; secrets live in the OS keychain, never here.\n";
    write_atomic(file, format!("{header}{text}").as_bytes())
}

/// Write `bytes` to `file` via a temp file in the same directory, fsync, then rename.
pub(crate) fn write_atomic(file: &Path, bytes: &[u8]) -> Result<()> {
    let dir = file
        .parent()
        .ok_or_else(|| Error::Invalid(format!("{} has no parent", file.display())))?;
    std::fs::create_dir_all(dir)?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sources.toml");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            // `mode` only applies on creation; a stale temp file could have other bits.
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, file)?;
        #[cfg(unix)]
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// JSON → TOML. Object members that are `null` are dropped (TOML has no null); `null` inside an
/// array is rejected.
fn json_to_toml(v: &Value) -> Result<Option<toml::Value>> {
    Ok(Some(match v {
        Value::Null => return Ok(None),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                return Err(Error::Invalid(format!("number {n} does not fit in TOML")));
            }
        }
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for x in a {
                out.push(json_to_toml(x)?.ok_or_else(|| {
                    Error::Invalid("source settings: null inside an array".into())
                })?);
            }
            toml::Value::Array(out)
        }
        Value::Object(o) => {
            let mut t = toml::Table::new();
            for (k, x) in o {
                if let Some(x) = json_to_toml(x)? {
                    t.insert(k.clone(), x);
                }
            }
            toml::Value::Table(t)
        }
    }))
}

/// Source ids: 1–64 chars of `[A-Za-z0-9._-]`, starting with an alphanumeric. No `:` so keychain
/// account names (`password:<id>`) stay unambiguous.
pub(crate) fn validate_id(id: &SourceId) -> Result<()> {
    let s = id.as_str();
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "source id `{s}`: use 1-64 characters from [A-Za-z0-9._-], starting with a letter or digit"
        )))
    }
}

/// Field names that must never appear in settings (credentials go to the keychain).
const SECRET_FIELDS: &[&str] = &[
    "password",
    "passwd",
    "app_password",
    "secret",
    "token",
    "access_token",
    "refresh_token",
    "api_key",
    "apikey",
];

/// Normalize and check settings: `null` → `{}`, must be an object, no secret-looking fields, and
/// must be representable in TOML.
pub(crate) fn normalize_settings(settings: Value) -> Result<Value> {
    let settings = match settings {
        Value::Null => Value::Object(Default::default()),
        v @ Value::Object(_) => v,
        _ => {
            return Err(Error::Invalid(
                "source settings must be a JSON object".into(),
            ));
        }
    };
    if let Some(field) = find_secret_field(&settings) {
        return Err(Error::Invalid(format!(
            "source settings must not contain secrets (field `{field}`); set credentials with sources.secret.set"
        )));
    }
    json_to_toml(&settings)?;
    Ok(settings)
}

fn find_secret_field(v: &Value) -> Option<String> {
    match v {
        Value::Object(o) => o.iter().find_map(|(k, x)| {
            let lower = k.to_ascii_lowercase();
            if SECRET_FIELDS.contains(&lower.as_str()) && !x.is_null() {
                Some(k.clone())
            } else {
                find_secret_field(x)
            }
        }),
        Value::Array(a) => a.iter().find_map(find_secret_field),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, settings: Value) -> SourceEntry {
        SourceEntry {
            id: SourceId::new(id),
            kind: "files".into(),
            name: "Docs".into(),
            enabled: true,
            settings,
            secret_accounts: vec![],
        }
    }

    #[test]
    fn round_trip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        assert!(load(&file).unwrap().is_empty());
        let mut e = entry(
            "files-1",
            json!({"roots": ["/a", "/b"], "max_mb": 5, "follow": false, "nested": {"x": 1.5}}),
        );
        e.secret_accounts = vec!["password:files-1".into()];
        save(&file, &[e.clone(), entry("files-2", json!({}))]).unwrap();
        let back = load(&file).unwrap();
        assert_eq!(back, vec![e, entry("files-2", json!({}))]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn settings_validation() {
        assert_eq!(normalize_settings(Value::Null).unwrap(), json!({}));
        assert!(normalize_settings(json!([1])).is_err());
        assert!(normalize_settings(json!({"password": "hunter2"})).is_err());
        assert!(normalize_settings(json!({"imap": {"Token": "x"}})).is_err());
        assert!(normalize_settings(json!({"list": [null]})).is_err());
        assert!(normalize_settings(json!({"password": null, "user": "me"})).is_ok());
        assert!(normalize_settings(json!({"client_id": "abc"})).is_ok());
    }

    #[test]
    fn id_validation() {
        assert!(validate_id(&SourceId::new("files-1")).is_ok());
        assert!(validate_id(&SourceId::new("Gmail_me.2")).is_ok());
        for bad in ["", "-x", "a:b", "a/b", "a b", &"x".repeat(65)] {
            assert!(validate_id(&SourceId::new(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn corrupt_or_duplicate_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        std::fs::write(&file, "not toml [[[").unwrap();
        assert!(load(&file).is_err());
        std::fs::write(
            &file,
            "[[sources]]\nid='a'\nkind='files'\nname='x'\n[[sources]]\nid='a'\nkind='files'\nname='y'\n",
        )
        .unwrap();
        assert!(load(&file).is_err());
    }
}
