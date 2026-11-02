//! Connected apps: the catalogue (`/v1/apps`) and one sync adapter per app behind a single
//! trait, so adding an app is one file: its tool calls, its parsing and its cursor.
//!
//! An adapter never talks to Composio directly. It gets a [`Tools`] handle bound to one
//! connected account that only lets it call the read tools it declared ([`SyncAdapter::tools`]),
//! holds it to the app's rate limit, and meters every call. It returns a [`Pull`]: items to
//! upsert (unchanged ones are skipped by content hash), explicit deletes, and complete listings
//! of a time window ([`Window`]) so that items gone upstream are deleted here too.
//!
//! Composio documents tool inputs, but most outputs only as `data`; every parser is lenient,
//! accepts the documented field spellings plus the upstream API's own, and skips what it cannot
//! read rather than inventing it. Per-app notes say what is verified and what is defensive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;

use crate::actions::ActionKind;
use crate::composio::ComposioError;
use crate::db::NewItem;

pub mod google;
pub mod logos;
pub mod microsoft;
pub mod notion;
pub mod slack;

#[cfg(test)]
mod conformance;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Mail,
    Calendar,
    Chat,
    Docs,
    Files,
}

/// How a connection learns that something changed upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// Tool calls on a schedule (every 30 minutes while the desktop app is active, daily otherwise).
    Poll,
    /// A Composio trigger posts to `/v1/webhooks/composio`, which marks the connection due; a
    /// daily poll is the safety net. Trigger events cost 10× a tool call ($0.003 vs $0.0003 on
    /// Composio Pro), so only apps where changes are rare use this.
    Trigger,
}

/// One app in the Connections catalogue.
#[derive(Debug, Clone, Copy)]
pub struct AppSpec {
    /// Composio toolkit slug (also `items.source` for what it syncs).
    pub slug: &'static str,
    pub name: &'static str,
    pub categories: &'static [Category],
    /// Whether this build syncs the app into items and chunks.
    pub syncs: bool,
    pub actions: &'static [ActionKind],
    /// The Composio triggers used in [`SyncMode::Trigger`], when the app has suitable ones.
    pub triggers: &'static [&'static str],
    pub default_mode: SyncMode,
}

impl AppSpec {
    /// Composio's public logo CDN; replaced by the logo in Composio's own catalogue when that
    /// is reachable (`GET /toolkits` → `meta.logo`).
    pub fn fallback_logo(&self) -> String {
        format!("https://logos.composio.dev/api/{}", self.slug)
    }
}

use ActionKind as A;
use Category as C;

/// Every app this build knows. Order is the catalogue's default order.
pub const CATALOG: &[AppSpec] = &[
    AppSpec {
        slug: google::GMAIL,
        name: "Gmail",
        categories: &[C::Mail],
        syncs: true,
        actions: &[A::SendMail, A::ReplyMail],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
    AppSpec {
        slug: google::CALENDAR,
        name: "Google Calendar",
        categories: &[C::Calendar],
        syncs: true,
        actions: &[A::CreateEvent, A::UpdateEvent],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
    AppSpec {
        slug: microsoft::OUTLOOK,
        name: "Outlook",
        categories: &[C::Mail, C::Calendar],
        syncs: true,
        // No update: Composio documents no Outlook event update tool we could verify.
        actions: &[A::SendMail, A::ReplyMail, A::CreateEvent],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
    AppSpec {
        slug: slack::SLACK,
        name: "Slack",
        categories: &[C::Chat],
        syncs: true,
        actions: &[A::PostMessage],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
    AppSpec {
        slug: microsoft::TEAMS,
        name: "Microsoft Teams",
        categories: &[C::Chat],
        syncs: true,
        actions: &[],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
    AppSpec {
        slug: notion::NOTION,
        name: "Notion",
        categories: &[C::Docs],
        syncs: true,
        actions: &[A::CreatePage],
        triggers: notion::TRIGGERS,
        default_mode: SyncMode::Trigger,
    },
    AppSpec {
        slug: google::DRIVE,
        name: "Google Drive",
        categories: &[C::Files, C::Docs],
        syncs: true,
        actions: &[],
        triggers: &[],
        default_mode: SyncMode::Poll,
    },
];

pub fn spec(slug: &str) -> Option<&'static AppSpec> {
    CATALOG.iter().find(|a| a.slug == slug)
}

/// The adapter that syncs `slug`, if this build syncs it.
pub fn adapter(slug: &str) -> Option<&'static dyn SyncAdapter> {
    static GMAIL: google::Gmail = google::Gmail;
    static CALENDAR: google::Calendar = google::Calendar;
    static DRIVE: google::Drive = google::Drive;
    static SLACK: slack::Slack = slack::Slack;
    static NOTION: notion::Notion = notion::Notion;
    static OUTLOOK: microsoft::Outlook = microsoft::Outlook;
    static TEAMS: microsoft::Teams = microsoft::Teams;
    Some(match slug {
        google::GMAIL => &GMAIL,
        google::CALENDAR => &CALENDAR,
        google::DRIVE => &DRIVE,
        slack::SLACK => &SLACK,
        notion::NOTION => &NOTION,
        microsoft::OUTLOOK => &OUTLOOK,
        microsoft::TEAMS => &TEAMS,
        _ => return None,
    })
}

// ---- the adapter contract ------------------------------------------------------------------------

/// Composio tool calls as one user through one connected account.
#[async_trait]
pub trait Tools: Send + Sync {
    /// Runs `tool` and returns its `data`. Refuses tools the adapter did not declare.
    async fn call(&self, tool: &'static str, args: Value) -> Result<Value, ComposioError>;
}

/// A complete upstream listing of the items of one scope over a time window: stored items in
/// that scope and window that are not in `seen` were deleted upstream. Only return a window
/// when every page of it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Only items of this kind (an app that syncs mail and events lists them separately).
    pub kind: Option<&'static str>,
    /// Only items whose external id starts with this (a Slack channel's `C123:`).
    pub prefix: String,
    /// Items with `occurred_at` in `[since, now]`.
    pub since: OffsetDateTime,
    pub seen: Vec<String>,
}

/// What one sync of one connection found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pull {
    pub items: Vec<NewItem>,
    /// External ids deleted (or trashed, or cancelled) upstream.
    pub deleted: Vec<String>,
    pub windows: Vec<Window>,
    /// The cursor to store once everything above is applied.
    pub cursor: Value,
    /// A backfill stopped at its page limit: continue in a minute rather than at the cadence.
    pub more: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("rate limited by {0}; retrying later")]
    RateLimited(String),
    #[error(transparent)]
    Composio(#[from] ComposioError),
}

#[async_trait]
pub trait SyncAdapter: Send + Sync {
    fn toolkit(&self) -> &'static str;
    /// Every Composio tool this adapter calls. These are read tools only; [`Tools`] refuses
    /// anything else, so a sync can never send, post or delete.
    fn tools(&self) -> &'static [&'static str];
    /// The rate we hold ourselves to per connected account (the upstream API's limit with
    /// headroom), and how many calls may go back to back.
    fn rate(&self) -> Rate;
    /// Most tool calls one sync may make (bounds Composio cost; the rest waits for next time).
    fn max_calls(&self) -> u32 {
        40
    }
    /// Fetches what changed since `cursor`. `backfill_days` is how far back a first sync may
    /// reach under the user's plan.
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
        backfill_days: u32,
    ) -> Result<Pull, PullError>;
}

// ---- rate limits ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub per_minute: u32,
    pub burst: u32,
}

/// Token buckets per key (a connected account and toolkit). Shared by every sync in the process.
#[derive(Clone, Default)]
pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: tokio::time::Instant,
}

impl RateLimiter {
    /// How long to wait before the next call under `rate` (and takes the token).
    fn reserve(&self, key: &str, rate: Rate, now: tokio::time::Instant) -> Duration {
        let per_sec = f64::from(rate.per_minute.max(1)) / 60.0;
        let burst = f64::from(rate.burst.max(1));
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let b = map.entry(key.to_owned()).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        let elapsed = now.saturating_duration_since(b.at).as_secs_f64();
        b.tokens = (b.tokens + elapsed * per_sec).min(burst);
        b.at = now;
        b.tokens -= 1.0;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / per_sec)
        }
    }

    /// Takes a token if the wait for it is at most `max`, and returns that wait; otherwise
    /// takes nothing and returns `None`.
    pub fn try_reserve(&self, key: &str, rate: Rate, max: Duration) -> Option<Duration> {
        let now = tokio::time::Instant::now();
        let wait = self.reserve(key, rate, now);
        if wait <= max {
            return Some(wait);
        }
        // Give the token back: a refused call must not push later ones further out.
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(b) = map.get_mut(key) {
            b.tokens += 1.0;
        }
        None
    }

    pub async fn acquire(&self, key: &str, rate: Rate) {
        let wait = self.reserve(key, rate, tokio::time::Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

/// Whether a Composio failure is the upstream (or Composio) saying "slow down".
pub fn is_rate_limited(e: &ComposioError) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    m.contains("429")
        || m.contains("rate limit")
        || m.contains("ratelimit")
        || m.contains("too many requests")
}

// ---- shared parsing helpers ------------------------------------------------------------------------

pub(crate) use crate::ingest::{cap, find_list, first_str, parse_time, rfc3339};

/// Longest stored body (bytes, cut on a char boundary).
pub(crate) const MAX_BODY_CHARS: usize = crate::ingest::MAX_BODY;

/// The first array found under any of `keys`, at the top or under `data`/`response_data`, or
/// the value itself when it is an array.
pub(crate) fn list<'a>(data: &'a Value, keys: &[&str]) -> &'a [Value] {
    let found = find_list(data, keys);
    if found.is_empty()
        && let Some(a) = data.as_array()
    {
        return a;
    }
    found
}

/// A string under any of the JSON pointers, at the top or under `data`/`response_data`.
pub(crate) fn top_str(data: &Value, ptrs: &[&str]) -> Option<String> {
    [data, &data["data"], &data["response_data"]]
        .into_iter()
        .find_map(|r| first_str(r, ptrs))
}

pub(crate) fn top_bool(data: &Value, key: &str) -> Option<bool> {
    [data, &data["data"], &data["response_data"]]
        .into_iter()
        .find_map(|r| r[key].as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_consistent() {
        let mut slugs: Vec<_> = CATALOG.iter().map(|a| a.slug).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), CATALOG.len(), "unique slugs");
        for app in CATALOG {
            assert!(
                app.slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} is a toolkit slug",
                app.slug
            );
            assert_eq!(app.syncs, adapter(app.slug).is_some(), "{}", app.slug);
            if let Some(a) = adapter(app.slug) {
                assert_eq!(a.toolkit(), app.slug);
                assert!(!a.tools().is_empty());
                assert!(a.rate().per_minute > 0 && a.rate().burst > 0);
            }
            assert_eq!(
                app.default_mode == SyncMode::Trigger,
                !app.triggers.is_empty(),
                "{}: trigger mode needs a trigger",
                app.slug
            );
            for kind in app.actions {
                assert!(
                    crate::actions::tool_for(*kind, app.slug).is_some(),
                    "{} declares {kind:?} but has no tool for it",
                    app.slug
                );
            }
        }
    }

    #[test]
    fn sync_tools_are_never_write_tools() {
        for app in CATALOG {
            let Some(a) = adapter(app.slug) else { continue };
            for t in a.tools() {
                for kind in crate::actions::ActionKind::ALL {
                    assert_ne!(
                        crate::actions::tool_for(kind, app.slug),
                        Some(*t),
                        "{} syncs with an action tool",
                        app.slug
                    );
                }
                let up = t.to_ascii_uppercase();
                for verb in [
                    "SEND", "POST", "CREATE", "DELETE", "UPDATE", "REPLY", "PATCH",
                ] {
                    assert!(!up.contains(verb), "{t} looks like a write tool");
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn token_bucket_allows_a_burst_then_paces() {
        let l = RateLimiter::default();
        let rate = Rate {
            per_minute: 60,
            burst: 3,
        };
        let t0 = tokio::time::Instant::now();
        for _ in 0..3 {
            assert_eq!(l.reserve("a", rate, t0), Duration::ZERO);
        }
        assert_eq!(l.reserve("a", rate, t0), Duration::from_secs(1));
        assert_eq!(l.reserve("a", rate, t0), Duration::from_secs(2));
        assert_eq!(l.reserve("b", rate, t0), Duration::ZERO, "per key");
        // Refills over time, never past the burst.
        let later = t0 + Duration::from_secs(600);
        for _ in 0..3 {
            assert_eq!(l.reserve("a", rate, later), Duration::ZERO);
        }
        assert!(l.reserve("a", rate, later) > Duration::ZERO);
        // acquire() really waits.
        let start = tokio::time::Instant::now();
        let slow = Rate {
            per_minute: 6,
            burst: 1,
        };
        l.acquire("c", slow).await;
        l.acquire("c", slow).await;
        assert!(tokio::time::Instant::now() - start >= Duration::from_secs(10));
        // A refused reservation takes nothing.
        let max = Duration::from_secs(5);
        assert_eq!(l.try_reserve("d", slow, max), Some(Duration::ZERO));
        assert_eq!(l.try_reserve("d", slow, max), None);
        assert_eq!(l.try_reserve("d", slow, max), None);
        assert_eq!(
            l.try_reserve("d", slow, Duration::from_secs(60)),
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn rate_limit_errors_are_recognised() {
        assert!(is_rate_limited(&ComposioError::Http(
            "429 Too Many Requests: slow down".into()
        )));
        assert!(is_rate_limited(&ComposioError::Tool {
            tool: "SLACK_X".into(),
            message: "ratelimited".into()
        }));
        assert!(!is_rate_limited(&ComposioError::Http("500: oops".into())));
    }
}
