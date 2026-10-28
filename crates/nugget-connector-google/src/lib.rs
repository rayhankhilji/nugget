//! Gmail and Google Calendar connectors. They talk to Google directly with the user's own OAuth
//! client ("Desktop app" type) through [`nugget_oauth`]; no intermediary service is involved.
//!
//! - **`gmail`** (scope `gmail.readonly`): a newest-first backfill over `window_days`
//!   (`messages.list` + `messages.get?format=raw` → `message/rfc822` bytes), then incremental
//!   `history.list` deltas. An expired history id (404) triggers a bounded resync.
//! - **`gcal`** (scope `calendar.readonly`): `events.list` with `singleEvents=true` over a
//!   -365/+365 day window, then `nextSyncToken` deltas; 410 triggers a full resync. Events become
//!   readable `text/plain` documents; cancelled events become deletes.
//!
//! All HTTP goes through [`MeteredHttp`](nugget_connector::http::MeteredHttp) with feature
//! `sync:gmail` / `sync:gcal` (token refreshes included), so every call is in the egress ledger
//! and redirects are never followed. Endpoints are injectable with [`GoogleEndpoints`] for tests.

mod api;
mod gcal;
mod gmail;
mod settings;
mod text;

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::Registry;

pub use gcal::{CalendarConnector, CalendarFactory};
pub use gmail::{GmailConnector, GmailFactory};

/// Google endpoints. [`Default`] is production; tests point everything at a mock server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleEndpoints {
    pub auth_url: String,
    pub token_url: String,
    /// Gmail API root, e.g. `https://gmail.googleapis.com/gmail/v1`.
    pub gmail_api: String,
    /// Calendar API root, e.g. `https://www.googleapis.com/calendar/v3`.
    pub calendar_api: String,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            auth_url: nugget_oauth::google::AUTH_URL.into(),
            token_url: nugget_oauth::google::TOKEN_URL.into(),
            gmail_api: "https://gmail.googleapis.com/gmail/v1".into(),
            calendar_api: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

impl GoogleEndpoints {
    /// Everything under one base URL (`<base>/auth`, `<base>/token`, `<base>/gmail/v1`,
    /// `<base>/calendar/v3`). For tests.
    pub fn with_base(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            auth_url: format!("{b}/auth"),
            token_url: format!("{b}/token"),
            gmail_api: format!("{b}/gmail/v1"),
            calendar_api: format!("{b}/calendar/v3"),
        }
    }
}

/// In-pass retries for rate limits (429, `userRateLimitExceeded`) and transient failures.
/// After `max_retries` the error (`RateLimited` / `Network`) is returned and the ingest
/// supervisor backs off; progress up to the last checkpoint is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Register the `gmail` and `gcal` kinds with production endpoints.
pub fn register(registry: &mut Registry) {
    registry.register(Arc::new(GmailFactory::new()));
    registry.register(Arc::new(CalendarFactory::new()));
}

#[cfg(test)]
mod tests;
