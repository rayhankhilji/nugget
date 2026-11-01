//! Settings of the three kinds, their JSON Schemas and validation.

use std::path::{Path, PathBuf};

use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Shown (verbatim) when macOS privacy protection blocks reading the Messages database.
pub const FULL_DISK_ACCESS_MESSAGE: &str = "Nugget needs Full Disk Access to read your Messages. \
Open System Settings → Privacy & Security → Full Disk Access and turn on Nugget, then try again.";

fn default_true() -> bool {
    true
}

/// Settings every messaging kind shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Common {
    /// Mark every document `sensitive` (kept away from cloud models and cloud embeddings).
    pub private: bool,
    /// Only messages newer than this many days.
    pub since_days: Option<u32>,
    pub include_groups: bool,
}

macro_rules! common {
    ($t:ty) => {
        impl $t {
            pub fn common(&self) -> Common {
                Common {
                    private: self.private,
                    since_days: self.since_days,
                    include_groups: self.include_groups,
                }
            }
        }
    };
}

impl Common {
    fn check(&self) -> Result<()> {
        if self.since_days == Some(0) {
            return Err(Error::Invalid("`since_days` must be at least 1".into()));
        }
        Ok(())
    }
}

/// `imessage` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IMessageSettings {
    #[serde(default = "default_true")]
    pub private: bool,
    #[serde(default)]
    pub since_days: Option<u32>,
    #[serde(default = "default_true")]
    pub include_groups: bool,
    #[serde(default)]
    pub db_path: Option<String>,
    #[serde(default = "default_true")]
    pub contacts: bool,
    /// Advanced: where to look for `AddressBook-v22.abcddb` files instead of the macOS default
    /// (`~/Library/Application Support/AddressBook`). Also enables contacts off macOS.
    #[serde(default)]
    pub contacts_dir: Option<String>,
}

/// `whatsapp` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhatsAppSettings {
    #[serde(default = "default_true")]
    pub private: bool,
    #[serde(default)]
    pub since_days: Option<u32>,
    #[serde(default = "default_true")]
    pub include_groups: bool,
    pub folder: String,
    #[serde(default)]
    pub me: Option<String>,
}

/// `telegram` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSettings {
    #[serde(default = "default_true")]
    pub private: bool,
    #[serde(default)]
    pub since_days: Option<u32>,
    #[serde(default = "default_true")]
    pub include_groups: bool,
    pub path: String,
}

fn parse<T: for<'de> Deserialize<'de>>(settings: &Value, kind: &str) -> Result<T> {
    let v = if settings.is_null() {
        Value::Object(Map::new())
    } else {
        settings.clone()
    };
    serde_json::from_value(v).map_err(|e| Error::Invalid(format!("{kind} settings: {e}")))
}

/// `~` / `~/x` → the home directory.
pub fn expand_home(p: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if p == "~" {
        if let Some(h) = home() {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
        && let Some(h) = home()
    {
        return PathBuf::from(h).join(rest);
    }
    PathBuf::from(p)
}

fn require_nonempty(s: &str, field: &str) -> Result<()> {
    if s.trim().is_empty() {
        return Err(Error::Invalid(format!("`{field}` is required")));
    }
    Ok(())
}

common!(IMessageSettings);
common!(WhatsAppSettings);
common!(TelegramSettings);

impl IMessageSettings {
    pub fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings, "iMessage")?;
        s.common().check()?;
        Ok(s)
    }

    /// The chat.db to read: `db_path`, else `~/Library/Messages/chat.db` on macOS.
    pub fn db_path(&self) -> Result<PathBuf> {
        if let Some(p) = self.db_path.as_deref().filter(|p| !p.trim().is_empty()) {
            return Ok(expand_home(p));
        }
        if cfg!(target_os = "macos") {
            Ok(expand_home("~/Library/Messages/chat.db"))
        } else {
            Err(Error::Unsupported(
                "iMessage is only available on macOS".into(),
            ))
        }
    }

    /// Folder searched for address book databases, when contacts are on.
    pub fn contacts_dir(&self) -> Option<PathBuf> {
        if !self.contacts {
            return None;
        }
        if let Some(d) = self
            .contacts_dir
            .as_deref()
            .filter(|p| !p.trim().is_empty())
        {
            return Some(expand_home(d));
        }
        cfg!(target_os = "macos").then(|| expand_home("~/Library/Application Support/AddressBook"))
    }
}

impl WhatsAppSettings {
    pub fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings, "WhatsApp")?;
        s.common().check()?;
        require_nonempty(&s.folder, "folder")?;
        Ok(s)
    }

    pub fn folder(&self) -> PathBuf {
        expand_home(self.folder.trim())
    }
}

impl TelegramSettings {
    pub fn parse(settings: &Value) -> Result<Self> {
        let s: Self = parse(settings, "Telegram")?;
        s.common().check()?;
        require_nonempty(&s.path, "path")?;
        Ok(s)
    }

    pub fn path(&self) -> PathBuf {
        expand_home(self.path.trim())
    }
}

/// Map an I/O error on the Messages database: permission problems are the macOS privacy
/// protection (Full Disk Access), a missing file names the path.
pub fn map_db_error(e: std::io::Error, path: &Path) -> Error {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            Error::Unauthorized(FULL_DISK_ACCESS_MESSAGE.into())
        }
        std::io::ErrorKind::NotFound => {
            Error::NotFound(format!("Messages database not found at {}", path.display()))
        }
        _ => Error::Io(e),
    }
}

/// Check that the Messages database can be opened for reading.
pub fn check_db_readable(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path).map_err(|e| map_db_error(e, path))?;
    if !meta.is_file() {
        return Err(Error::Invalid(format!(
            "{} is not a Messages database file",
            path.display()
        )));
    }
    std::fs::File::open(path).map_err(|e| map_db_error(e, path))?;
    Ok(())
}

pub fn check_folder(path: &Path, what: &str) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(Error::Invalid(format!(
            "{what} {} is not a folder",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!(
            "{what} {} does not exist",
            path.display()
        ))),
        Err(e) => Err(Error::Invalid(format!(
            "cannot open {what} {}: {e}",
            path.display()
        ))),
    }
}

pub fn check_exists(path: &Path, what: &str) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!(
            "{what} {} does not exist",
            path.display()
        ))),
        Err(e) => Err(Error::Invalid(format!(
            "cannot open {what} {}: {e}",
            path.display()
        ))),
    }
}

fn common_props() -> Map<String, Value> {
    let v = json!({
        "private": {
            "type": "boolean", "default": true,
            "title": "Keep these chats on this device only (never sent to cloud models)",
            "description": "Every imported conversation is marked sensitive: it stays out of cloud embeddings and is never sent to cloud models."
        },
        "since_days": {
            "type": "integer", "minimum": 1,
            "title": "Only messages from the last N days",
            "description": "Leave empty to import the whole history."
        },
        "include_groups": {
            "type": "boolean", "default": true,
            "title": "Include group chats"
        }
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn schema(title: &str, required: &[&str], extra: Value) -> Value {
    let mut props = common_props();
    if let Value::Object(m) = extra {
        props.extend(m);
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": props,
    })
}

/// JSON Schema of [`IMessageSettings`].
pub fn imessage_schema() -> Value {
    schema(
        "iMessage",
        &[],
        json!({
            "db_path": {
                "type": "string",
                "title": "Messages database",
                "description": "Path of chat.db. Defaults to ~/Library/Messages/chat.db."
            },
            "contacts": {
                "type": "boolean", "default": true,
                "title": "Show contact names",
                "description": "Resolve phone numbers and e-mail addresses to names through the macOS address book."
            },
            "contacts_dir": {
                "type": "string",
                "title": "Address book folder (advanced)",
                "description": "Folder searched for AddressBook-v22.abcddb files. Defaults to ~/Library/Application Support/AddressBook."
            }
        }),
    )
}

/// JSON Schema of [`WhatsAppSettings`].
pub fn whatsapp_schema() -> Value {
    schema(
        "WhatsApp exports",
        &["folder"],
        json!({
            "folder": {
                "type": "string",
                "title": "Exports folder",
                "description": "Folder where you save WhatsApp \"Export chat\" files (.zip or .txt, also in subfolders one level deep). New exports are picked up automatically."
            },
            "me": {
                "type": "string",
                "title": "Your name in the chats",
                "description": "Your WhatsApp profile name as it appears in the exports, so your own messages are shown as yours."
            }
        }),
    )
}

/// JSON Schema of [`TelegramSettings`].
pub fn telegram_schema() -> Value {
    schema(
        "Telegram export",
        &["path"],
        json!({
            "path": {
                "type": "string",
                "title": "Export folder or result.json",
                "description": "A Telegram Desktop export (Settings → Advanced → Export Telegram data, format \"Machine-readable JSON\"): the folder containing result.json, or the file itself."
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        let s = WhatsAppSettings::parse(&json!({"folder": "/x", "me": "Tanuj"})).unwrap();
        assert!(s.private && s.include_groups);
        assert!(WhatsAppSettings::parse(&json!({})).is_err());
        assert!(WhatsAppSettings::parse(&json!({"folder": " "})).is_err());
        assert!(WhatsAppSettings::parse(&json!({"folder": "/x", "since_days": 0})).is_err());
        assert!(WhatsAppSettings::parse(&json!({"folder": "/x", "bogus": 1})).is_err());
        let t = TelegramSettings::parse(&json!({"path": "/x", "private": false})).unwrap();
        assert!(!t.common().private);
        let i = IMessageSettings::parse(&Value::Null).unwrap();
        assert!(i.contacts);
        if !cfg!(target_os = "macos") {
            assert!(matches!(i.db_path(), Err(Error::Unsupported(_))));
            assert!(i.contacts_dir().is_none());
        }
    }

    #[test]
    fn schemas_list_every_setting() {
        for (schema, keys) in [
            (
                imessage_schema(),
                &[
                    "private",
                    "since_days",
                    "include_groups",
                    "db_path",
                    "contacts",
                ][..],
            ),
            (whatsapp_schema(), &["folder", "me"][..]),
            (telegram_schema(), &["path"][..]),
        ] {
            for k in keys {
                assert!(schema["properties"].get(*k).is_some(), "{k}");
            }
        }
    }
}
