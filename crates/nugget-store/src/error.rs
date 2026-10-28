//! Conversions from SQLite errors into the cross-crate [`nugget_core::Error`].

use nugget_core::{Error, Result};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Attach context to a `rusqlite` result and convert it to `Error::Storage`.
pub(crate) trait DbExt<T> {
    fn db(self, context: &str) -> Result<T>;
}

impl<T> DbExt<T> for rusqlite::Result<T> {
    fn db(self, context: &str) -> Result<T> {
        self.map_err(|e| Error::Storage(format!("{context}: {e}")))
    }
}

pub(crate) fn format_time(t: OffsetDateTime) -> Result<String> {
    t.format(&Rfc3339)
        .map_err(|e| Error::Other(format!("cannot format timestamp: {e}")))
}

pub(crate) fn parse_time(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| Error::Storage(format!("bad timestamp {s:?} in store: {e}")))
}
