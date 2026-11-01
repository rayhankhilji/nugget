//! `imap` source settings.

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How the connection is secured. There is no plaintext mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// TLS from the first byte (IMAPS, usually port 993).
    #[default]
    Implicit,
    /// Plain connect, then `STARTTLS` before anything else (usually port 143). Fails if the
    /// server does not upgrade.
    Starttls,
}

/// Settings of an `imap` source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapSettings {
    pub host: String,
    /// Defaults to 993 for implicit TLS and 143 for STARTTLS.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: TlsMode,
    pub username: String,
    /// Mailboxes to index. Default: INBOX plus the Sent mailbox. `"*"` expands to every
    /// selectable mailbox except Trash, Spam and Junk and the virtual views (`\All`,
    /// `\Flagged`, `\Important`, e.g. Gmail's All Mail, Starred and Important).
    #[serde(default)]
    pub folders: Option<Vec<String>>,
    /// Only messages received in the last N days (by INTERNALDATE). `0` means no limit.
    #[serde(default = "default_window_days")]
    pub window_days: u32,
    /// Messages larger than this are indexed by their headers only.
    #[serde(default = "default_max_message_mb")]
    pub max_message_mb: u32,
}

fn default_window_days() -> u32 {
    365
}

fn default_max_message_mb() -> u32 {
    25
}

impl ImapSettings {
    pub fn parse(v: &Value) -> Result<Self> {
        let s: Self = serde_json::from_value(v.clone())
            .map_err(|e| Error::Invalid(format!("imap settings: {e}")))?;
        s.check()?;
        Ok(s)
    }

    pub fn port(&self) -> u16 {
        self.port.unwrap_or(Self::default_port(self.tls))
    }

    /// 993 for implicit TLS, 143 for STARTTLS.
    pub fn default_port(tls: TlsMode) -> u16 {
        match tls {
            TlsMode::Implicit => 993,
            TlsMode::Starttls => 143,
        }
    }

    fn check(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::Invalid(format!("imap settings: {m}")));
        let host = self.host.trim();
        if host.is_empty() || host.len() > 253 {
            return bad("`host` is required");
        }
        if host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "/?#@".contains(c))
        {
            return bad("`host` must be a bare host name or IP address");
        }
        if self.port == Some(0) {
            return bad("`port` must be 1-65535");
        }
        if self.username.is_empty() || self.username.chars().any(|c| c == '\r' || c == '\n') {
            return bad("`username` is required and must be one line");
        }
        if let Some(folders) = &self.folders {
            if folders.is_empty() {
                return bad("`folders` must not be empty (omit it for INBOX and Sent)");
            }
            for f in folders {
                if f.is_empty() || f.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
                    return bad("folder names must be non-empty single lines");
                }
            }
        }
        if self.window_days > 36_500 {
            return bad("`window_days` must be at most 36500");
        }
        if self.max_message_mb == 0 || self.max_message_mb > 512 {
            return bad("`max_message_mb` must be 1-512");
        }
        Ok(())
    }
}

/// JSON Schema of the settings object, for UIs.
pub fn schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["host", "username"],
        "properties": {
            "host": { "type": "string", "title": "IMAP server", "examples": ["imap.fastmail.com"] },
            "port": { "type": "integer", "minimum": 1, "maximum": 65535,
                      "description": "Default 993 (implicit TLS) or 143 (STARTTLS)" },
            "tls": { "type": "string", "enum": ["implicit", "starttls"], "default": "implicit" },
            "username": { "type": "string" },
            "folders": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1,
                         "description": "Default: INBOX and Sent. \"*\" = all mailboxes except Trash/Spam/Junk and virtual views (All Mail, Starred, Important)" },
            "window_days": { "type": "integer", "minimum": 0, "maximum": 36500, "default": 365,
                             "description": "Index mail from the last N days (0 = all)" },
            "max_message_mb": { "type": "integer", "minimum": 1, "maximum": 512, "default": 25,
                                "description": "Larger messages are indexed by headers only" }
        }
    })
}
