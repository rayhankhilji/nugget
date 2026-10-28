use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Vault rendering and watching options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    /// Put strong identifiers (email addresses, phone numbers, ...) in page frontmatter. Off by
    /// default: they are personal, and the vault is plain text that may be synced elsewhere.
    pub include_identifiers: bool,
    /// Longest entity-name part of a file name, in characters (also capped at 160 bytes).
    pub max_name_chars: usize,
    /// How long the watcher waits for a burst of edits to settle.
    #[serde(with = "millis")]
    pub debounce: Duration,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            include_identifiers: false,
            max_name_chars: 80,
            debounce: Duration::from_millis(750),
        }
    }
}

mod millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }
}
