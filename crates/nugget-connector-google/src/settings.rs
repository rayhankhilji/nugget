//! Source settings for `gmail`, `gcal` and `gdrive`.

use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const MAX_WINDOW_DAYS: u32 = 3650;

fn default_window() -> u32 {
    365
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GmailSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// Backfill window on first sync (and on resync), in days.
    #[serde(default = "default_window")]
    pub window_days: u32,
    /// Also index Spam and Trash.
    #[serde(default)]
    pub include_spam_trash: bool,
    /// Ask for `gmail.modify` (instead of `gmail.readonly`) so suggested actions may archive
    /// threads after the user confirms. Takes effect after reconnecting the account.
    #[serde(default)]
    pub allow_modify: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// Calendar ids to sync. Absent or empty: every calendar selected in Google Calendar.
    #[serde(default)]
    pub calendars: Option<Vec<String>>,
    #[serde(default = "default_window")]
    pub past_days: u32,
    #[serde(default = "default_window")]
    pub future_days: u32,
}

/// Default and largest per-file download for `gdrive`, in MiB.
pub(crate) const DEFAULT_DRIVE_FILE_MB: u32 = 25;
pub(crate) const MAX_DRIVE_FILE_MB: u32 = 200;

fn default_drive_file_mb() -> u32 {
    DEFAULT_DRIVE_FILE_MB
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DriveSettings {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub account_hint: Option<String>,
    /// Also index files other people shared with you (not in a shared drive).
    #[serde(default)]
    pub include_shared_with_me: bool,
    /// Also index files in shared drives you are a member of.
    #[serde(default)]
    pub include_shared_drives: bool,
    /// Largest file downloaded or exported, in MiB; bigger files get a "not indexed" stub.
    #[serde(default = "default_drive_file_mb")]
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

fn check_client(client_id: &str, account_hint: Option<&str>) -> Result<()> {
    let id = client_id.trim();
    if id.is_empty() {
        return Err(Error::Invalid("settings: client_id is required".into()));
    }
    if id.len() > 512 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Invalid("settings: client_id is malformed".into()));
    }
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

impl GmailSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings)?;
        check_client(&s.client_id, s.account_hint.as_deref())?;
        check_days("window_days", s.window_days, 1)?;
        Ok(s)
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["window_days"] = json!({
            "type": "integer", "minimum": 1, "maximum": MAX_WINDOW_DAYS, "default": 365,
            "title": "Backfill window (days)",
            "description": "How far back to index mail on first sync."
        });
        props["include_spam_trash"] = json!({
            "type": "boolean", "default": false, "title": "Include Spam and Trash"
        });
        props["allow_modify"] = json!({
            "type": "boolean", "default": false, "title": "Allow archiving",
            "description": "Lets suggested actions archive threads after you confirm (asks Google for permission to change labels; reconnect the account after turning it on). Nothing is ever sent or deleted."
        });
        schema
    }
}

impl CalendarSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings)?;
        check_client(&s.client_id, s.account_hint.as_deref())?;
        check_days("past_days", s.past_days, 0)?;
        check_days("future_days", s.future_days, 0)?;
        if let Some(cals) = &s.calendars {
            if cals.len() > 500 {
                return Err(Error::Invalid("settings: too many calendars".into()));
            }
            if cals
                .iter()
                .any(|c| c.trim().is_empty() || c.len() > 1024 || c.contains('/'))
            {
                return Err(Error::Invalid(
                    "settings: calendars contains a bad id".into(),
                ));
            }
        }
        Ok(s)
    }

    /// Configured calendars, if any.
    pub(crate) fn calendar_ids(&self) -> Option<Vec<String>> {
        self.calendars
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| c.iter().map(|s| s.trim().to_string()).collect())
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["calendars"] = json!({
            "type": "array", "items": {"type": "string", "minLength": 1},
            "title": "Calendars",
            "description": "Calendar ids (e.g. `primary`). Leave empty for every calendar selected in Google Calendar."
        });
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

impl DriveSettings {
    pub(crate) fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings)?;
        check_client(&s.client_id, s.account_hint.as_deref())?;
        if s.max_file_mb < 1 || s.max_file_mb > MAX_DRIVE_FILE_MB {
            return Err(Error::Invalid(format!(
                "settings: max_file_mb must be between 1 and {MAX_DRIVE_FILE_MB}"
            )));
        }
        Ok(s)
    }

    /// The per-file cap in bytes.
    pub(crate) fn max_bytes(&self) -> u64 {
        u64::from(self.max_file_mb) * 1024 * 1024
    }

    pub(crate) fn schema() -> Value {
        let mut schema = common_schema();
        let props = &mut schema["properties"];
        props["include_shared_with_me"] = json!({
            "type": "boolean", "default": false, "title": "Include files shared with me",
            "description": "Also index files other people shared with you."
        });
        props["include_shared_drives"] = json!({
            "type": "boolean", "default": false, "title": "Include shared drives",
            "description": "Also index files in shared drives you are a member of."
        });
        props["max_file_mb"] = json!({
            "type": "integer", "minimum": 1, "maximum": MAX_DRIVE_FILE_MB,
            "default": DEFAULT_DRIVE_FILE_MB, "title": "Largest file (MB)",
            "description": "Bigger files are listed by name with the reason they were not indexed."
        });
        schema
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
                "type": "string", "minLength": 1, "title": "OAuth client ID",
                "description": "From your own Google Cloud project: an OAuth client of type \"Desktop app\"."
            },
            "client_secret": {
                "type": "string", "title": "OAuth client secret",
                "description": "Shown next to the client ID. Not confidential for desktop apps, but never logged."
            },
            "account_hint": {
                "type": "string", "title": "Google account",
                "description": "Email address to pre-select on the consent screen."
            }
        }
    })
}
