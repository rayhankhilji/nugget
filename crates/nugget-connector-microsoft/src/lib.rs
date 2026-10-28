//! Microsoft 365 connectors (Outlook mail, calendar, OneDrive) over Microsoft Graph delta queries.
//! They talk to Microsoft directly with the user's own Entra app registration (a public client;
//! the secret is optional) through [`nugget_oauth`], with the Microsoft identity platform preset
//! in [`oauth`]. No intermediary service is involved.
//!
//! - **`outlook`** (scope `Mail.Read`): per-folder `/me/mailFolders/{id}/messages/delta` over a
//!   `window_days` backfill (`receivedDateTime ge ...`), then delta links. Each message is fetched
//!   as MIME (`/me/messages/{id}/$value` → `message/rfc822`). `@removed` entries are deletes;
//!   an expired delta token (410 / `syncStateNotFound`) triggers a resync.
//! - **`outlook-calendar`** (scope `Calendars.Read`): `/me/calendarView/delta` over a
//!   -past/+future window (recurrences expanded by the calendar view), then delta links. Events
//!   become readable `text/plain` documents; cancelled and removed events become deletes.
//! - **`onedrive`** (scope `Files.Read`): `/me/drive/root/delta`, with folder include/exclude
//!   filters and a size cap. File bytes come from the pre-authenticated
//!   `@microsoft.graph.downloadUrl`, fetched **without** the bearer token and only from HTTPS
//!   Microsoft download hosts ([`MicrosoftEndpoints::download_hosts`]).
//!
//! All HTTP goes through [`MeteredHttp`](nugget_connector::http::MeteredHttp) with feature
//! `sync:<kind>` (token refreshes and downloads included), so every exchange is in the egress
//! ledger and redirects are never followed. Endpoints are injectable for tests.

mod calendar;
mod graph;
pub mod oauth;
mod onedrive;
mod outlook;
mod settings;
mod text;

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::Registry;

pub use calendar::{OutlookCalendarConnector, OutlookCalendarFactory};
pub use onedrive::{OneDriveConnector, OneDriveFactory};
pub use outlook::{OutlookConnector, OutlookFactory};

/// Microsoft endpoints. [`Default`] is production; tests point everything at a mock server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftEndpoints {
    /// Identity platform root; the tenant is appended (`<login>/<tenant>/oauth2/v2.0/...`).
    pub login: String,
    /// Graph API root, e.g. `https://graph.microsoft.com/v1.0`. `@odata.nextLink` and
    /// `@odata.deltaLink` must stay under it (same origin and path prefix).
    pub graph_api: String,
    /// Hosts that may serve OneDrive downloads: an entry matches the host itself and its
    /// subdomains.
    pub download_hosts: Vec<String>,
    /// Allow plain-HTTP download URLs. Never in production; tests only.
    pub allow_http_downloads: bool,
}

/// Microsoft-owned hosts that serve `@microsoft.graph.downloadUrl` content (OneDrive personal
/// and OneDrive for Business / SharePoint).
pub const DOWNLOAD_HOSTS: &[&str] = &[
    "1drv.com",
    "1drv.ms",
    "livefilestore.com",
    "microsoftpersonalcontent.com",
    "onedrive.com",
    "sharepoint.com",
    "svc.ms",
];

impl Default for MicrosoftEndpoints {
    fn default() -> Self {
        Self {
            login: oauth::LOGIN_BASE.into(),
            graph_api: "https://graph.microsoft.com/v1.0".into(),
            download_hosts: DOWNLOAD_HOSTS.iter().map(|h| (*h).to_string()).collect(),
            allow_http_downloads: false,
        }
    }
}

impl MicrosoftEndpoints {
    /// Everything under one base URL (`<base>/<tenant>/oauth2/v2.0/{authorize,token}`,
    /// `<base>/v1.0`), with downloads allowed from `127.0.0.1` over HTTP. For tests.
    pub fn with_base(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            login: b.to_string(),
            graph_api: format!("{b}/v1.0"),
            download_hosts: vec!["127.0.0.1".into()],
            allow_http_downloads: true,
        }
    }
}

/// In-pass retries for throttling (429, 503/504 with `Retry-After`) and transient failures.
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
            max_delay: Duration::from_secs(120),
        }
    }
}

/// Register the `outlook`, `outlook-calendar` and `onedrive` kinds with production endpoints.
pub fn register(registry: &mut Registry) {
    registry.register(Arc::new(OutlookFactory::new()));
    registry.register(Arc::new(OutlookCalendarFactory::new()));
    registry.register(Arc::new(OneDriveFactory::new()));
}

#[cfg(test)]
mod tests;
