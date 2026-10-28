use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// A connector instance, e.g. `files:~/Documents` or `gmail:me@example.com`.
    SourceId
);
string_id!(
    /// A source item, unique within its source (path, message id, ...).
    ItemKey
);
string_id!(
    /// A knowledge-graph entity id (stable across renames).
    EntityId
);
string_id!(
    /// A configured model provider instance, e.g. `anthropic`, `ollama-local`.
    ProviderId
);

/// Fully-qualified item reference used for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemRef {
    pub source: SourceId,
    pub key: ItemKey,
}
