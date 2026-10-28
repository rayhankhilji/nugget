//! Source settings for `outlook`, `outlook-calendar` and `onedrive`.

use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::oauth::{DEFAULT_TENANT, validate_tenant};

pub(crate) const MAX_WINDOW_DAYS: u32 = 3650;
pub(crate) const DEFAULT_FOLDERS: &[&str] = &["inbox", "sentitems"];
pub(crate) const DEFAULT_MAX_FILE_MB: u32 = 25;
pub(crate) const MAX_FILE_MB: u32 = 500;

fn default_window() -> u32 {
    365
}

fn default_tenant() -> String {
    DEFAULT_TENANT.into()
}

fn default_max_file_mb() -> u32 {
    DEFAULT_MAX_FILE_MB
}

/// The OAuth part shared by every kind.
#[derive(Clone)]
pub(crate) struct Common {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub tenant: String,
    pub account_hint: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlookSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// Mail folders: well-known names (`inbox`, `sentitems`, `archive`, ...) or folder ids.
    #[serde(default)]
    pub folders: Option<Vec<String>>,
    /// Backfill window on first sync (and on resync), in days.
    #[serde(default = "default_window")]
    pub window_days: u32,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    #[serde(default)]
    pub account_hint: Option<String>,
    #[serde(default = "default_window")]
    pub past_days: u32,
    #[serde(default = "default_window")]
    pub future_days: u32,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OneDriveSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// Folder paths (`/Documents/Work`) to index. Empty: the whole drive.
    #[serde(default)]
    pub include_folders: Vec<String>,
    /// Folder paths never indexed (wins over `include_folders`).
    #[serde(default)]
    pub exclude_folders: Vec<String>,
    #[serde(default = "default_max_file_mb")]
    pub max_file_mb: u32,
}

fn parse<T: for<'de> Deserialize<'de>>(settings: &Value) -> Result<T> {
    let v = if settings.is_null() {
        json!({})
    } else {
        settings.clone()
    };
    serde_json::from_value(v).map_err(|e| Error::Invalid(format!("settings: {e}")))
}

fn check_common(client_id: &str, tenant: &str, account_hint: Option<&str>) -> Result<()> {
    let id = client_id.trim();
    if id.is_empty() {
        return Err(Error::Invalid("settings: client_id is required".into()));
    }
    if id.len() > 512 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Invalid("settings: client_id is malformed".into()));
    }
    validate_tenant(tenant.trim())?;
    if account_hint.is_some_and(|h| h.len() > 320 || h.chars().any(char::is_control)) {
        return Err(Error::Invalid("settings: account_hint is malformed".into()));
    }
    Ok(())
}

fn check_days(name: &str, v: u32, min: u32) -> Result<()> {
    if v < min || v > MAX_WINDOW_DAYS {
        return Err(Error::Invalid(format!(
            "settings: {name} must be between {min} and {MAX_WINDOW_DAYS}"
        )));
    }
    Ok(())
}

/// Mail folder names and ids are used as a path segment and as the key prefix.
pub(crate) fn valid_folder_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        // Dot-segments: the URL builder drops them, so `..` would address another endpoint.
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'=' | b'+' | b'.'))
}

/// Normalise a OneDrive folder path: leading `/`, no trailing `/`, no empty or dot segments,
/// lower-cased (OneDrive paths are case-insensitive). `/` is the whole drive.
pub(crate) fn normalize_path(p: &str) -> Result<String> {
    let bad = || {
        Error::Invalid(format!(
            "settings: folder path `{}` is malformed",
            p.chars().take(80).collect::<String>()
        ))
    };
    if p.len() > 2048 || p.chars().any(char::is_control) {
        return Err(bad());
    }
    let mut out = String::new();
    for seg in p.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." {
            return Err(bad());
        }
        out.push('/');
        out.push_str(&seg.to_lowercase());
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

impl OutlookSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings)?;
        check_common(&s.client_id, &s.tenant, s.account_hint.as_deref())?;
        check_days("window_days", s.window_days, 1)?;
        if let Some(f) = &s.folders {
            if f.len() > 100 {
                return Err(Error::Invalid("settings: too many folders".into()));
            }
            if f.iter().any(|f| !valid_folder_id(f.trim())) {
                return Err(Error::Invalid(
                    "settings: folders contains a bad folder name or id".into(),
                ));
            }
        }
        Ok(s)
    }

    pub(crate) fn common(&self) -> Common {
        common(
            &self.client_id,
            &self.client_secret,
            &self.tenant,
            &self.account_hint,
        )
    }

    /// Configured folders (deduplicated, in order), or Inbox and Sent Items.
    pub(crate) fn folder_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let list: Vec<String> = match self.folders.as_ref().filter(|f| !f.is_empty()) {
            Some(f) => f.iter().map(|s| s.trim().to_string()).collect(),
            None => DEFAULT_FOLDERS.iter().map(|s| (*s).to_string()).collect(),
        };
        for f in list {
            if !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["folders"] = json!({
            "type": "array", "items": {"type": "string", "minLength": 1},
            "default": DEFAULT_FOLDERS,
            "title": "Mail folders",
            "description": "Well-known folder names (inbox, sentitems, archive, ...) or folder ids. Default: Inbox and Sent Items."
        });
        props["window_days"] = json!({
            "type": "integer", "minimum": 1, "maximum": MAX_WINDOW_DAYS, "default": 365,
            "title": "Backfill window (days)",
            "description": "How far back to index mail on first sync."
        });
        schema
    }
}

impl CalendarSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings)?;
        check_common(&s.client_id, &s.tenant, s.account_hint.as_deref())?;
        check_days("past_days", s.past_days, 0)?;
        check_days("future_days", s.future_days, 0)?;
        if s.past_days + s.future_days == 0 {
            return Err(Error::Invalid(
                "settings: past_days and future_days are both 0".into(),
            ));
        }
        Ok(s)
    }

    pub(crate) fn common(&self) -> Common {
        common(
            &self.client_id,
            &self.client_secret,
            &self.tenant,
            &self.account_hint,
        )
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["past_days"] = json!({
            "type": "integer", "minimum": 0, "maximum": MAX_WINDOW_DAYS, "default": 365,
            "title": "Days of past events"
        });
        props["future_days"] = json!({
            "type": "integer", "minimum": 0, "maximum": MAX_WINDOW_DAYS, "default": 365,
            "title": "Days of future events"
        });
        schema
    }
}

impl OneDriveSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let mut s: Self = parse(settings)?;
        check_common(&s.client_id, &s.tenant, s.account_hint.as_deref())?;
        if s.max_file_mb == 0 || s.max_file_mb > MAX_FILE_MB {
            return Err(Error::Invalid(format!(
                "settings: max_file_mb must be between 1 and {MAX_FILE_MB}"
            )));
        }
        if s.include_folders.len() > 200 || s.exclude_folders.len() > 200 {
            return Err(Error::Invalid("settings: too many folders".into()));
        }
        s.include_folders = s
            .include_folders
            .iter()
            .map(|p| normalize_path(p))
            .collect::<Result<_>>()?;
        s.exclude_folders = s
            .exclude_folders
            .iter()
            .map(|p| normalize_path(p))
            .collect::<Result<_>>()?;
        Ok(s)
    }

    pub(crate) fn common(&self) -> Common {
        common(
            &self.client_id,
            &self.client_secret,
            &self.tenant,
            &self.account_hint,
        )
    }

    pub(crate) fn max_bytes(&self) -> u64 {
        u64::from(self.max_file_mb) * 1024 * 1024
    }

    /// Whether a file at `path` (normalised, lower-case) is in scope.
    pub(crate) fn in_scope(&self, path: &str) -> bool {
        let under =
            |root: &String| root == "/" || path == root || path.starts_with(&format!("{root}/"));
        (self.include_folders.is_empty() || self.include_folders.iter().any(under))
            && !self.exclude_folders.iter().any(under)
    }

    /// Whether any folder filter is configured (paths then decide what is indexed).
    pub(crate) fn has_filters(&self) -> bool {
        !self.include_folders.is_empty() || !self.exclude_folders.is_empty()
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["include_folders"] = json!({
            "type": "array", "items": {"type": "string", "minLength": 1},
            "title": "Folders to index",
            "description": "Paths from the drive root, e.g. /Documents/Work. Leave empty for the whole drive."
        });
        props["exclude_folders"] = json!({
            "type": "array", "items": {"type": "string", "minLength": 1},
            "title": "Folders to skip",
            "description": "Paths from the drive root never indexed, e.g. /Pictures."
        });
        props["max_file_mb"] = json!({
            "type": "integer", "minimum": 1, "maximum": MAX_FILE_MB, "default": DEFAULT_MAX_FILE_MB,
            "title": "Largest file (MB)",
            "description": "Larger files are skipped."
        });
        schema
    }
}

fn common(
    client_id: &str,
    client_secret: &Option<String>,
    tenant: &str,
    account_hint: &Option<String>,
) -> Common {
    Common {
        client_id: client_id.trim().to_string(),
        client_secret: client_secret.clone().filter(|s| !s.is_empty()),
        tenant: tenant.trim().to_string(),
        account_hint: account_hint.clone(),
    }
}

fn common_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["client_id"],
        "properties": {
            "client_id": {
                "type": "string", "minLength": 1, "title": "Application (client) ID",
                "description": "From your own Microsoft Entra app registration (Mobile and desktop applications platform, redirect URI http://localhost)."
            },
            "client_secret": {
                "type": "string", "title": "Client secret",
                "description": "Optional: public clients need none. Never logged."
            },
            "tenant": {
                "type": "string", "default": DEFAULT_TENANT, "title": "Tenant",
                "description": "common (work, school and personal accounts), organizations, consumers, or your tenant id or domain."
            },
            "account_hint": {
                "type": "string", "title": "Microsoft account",
                "description": "Email address to pre-select on the sign-in screen."
            }
        }
    })
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn paths_normalise_and_scope() {
        assert_eq!(
            normalize_path("Documents/Work/").unwrap(),
            "/documents/work"
        );
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("\\A\\\\b").unwrap(), "/a/b");
        assert!(normalize_path("/a/../b").is_err());
        assert!(normalize_path("/a\u{0}").is_err());
        let s = OneDriveSettings::parse(&json!({
            "client_id": "x",
            "include_folders": ["/Documents"],
            "exclude_folders": ["/Documents/Private"]
        }))
        .unwrap();
        assert!(s.in_scope("/documents/a.txt"));
        assert!(s.in_scope("/documents/work/a.txt"));
        assert!(!s.in_scope("/documentsx/a.txt"));
        assert!(!s.in_scope("/documents/private/a.txt"));
        assert!(!s.in_scope("/a.txt"));
    }

    /// `true` when `T: Debug` (inherent method wins), `false` otherwise (trait fallback).
    struct Probe<T>(std::marker::PhantomData<T>);
    trait NotDebug {
        fn is_debug(&self) -> bool {
            false
        }
    }
    impl<T> NotDebug for Probe<T> {}
    impl<T: std::fmt::Debug> Probe<T> {
        #[allow(dead_code)]
        fn is_debug(&self) -> bool {
            true
        }
    }

    /// Settings carry the OAuth `client_secret`: they must not be `Debug`, so no `{:?}` or
    /// `tracing` field can print it.
    #[test]
    fn settings_holding_secrets_are_not_debug() {
        use std::marker::PhantomData;
        assert!(!Probe::<Common>(PhantomData).is_debug());
        assert!(!Probe::<OutlookSettings>(PhantomData).is_debug());
        assert!(!Probe::<CalendarSettings>(PhantomData).is_debug());
        assert!(!Probe::<OneDriveSettings>(PhantomData).is_debug());
        assert!(Probe::<String>(PhantomData).is_debug());
    }
}
