//! `outlook-calendar`: the default Outlook calendar over Microsoft Graph with `Calendars.Read`.
//!
//! Scopes: `Calendars.Read` (read events; no write), `offline_access` (a refresh token, so
//! syncing continues without signing in again) and `User.Read` (lets the consent screen name the
//! account). Tokens live only in the secret store.
//!
//! `/me/calendarView/delta` lists the event instances inside a `-past_days/+future_days` window:
//! recurring series come back expanded (occurrences and exceptions), so every instance is its
//! own document. Events become readable `text/plain` documents; cancelled and `@removed`
//! events become deletes.
//!
//! Cursor (JSON): `{"v":1,"start":<unix>,"end":<unix>,...}` with the window of the listing plus
//! - `"delta_link":"<url>"` in incremental mode,
//! - `"full":true,"next_link":"<url>"|null,"resync":bool,"reconcile":bool` while a full listing
//!   runs (`next_link` also appears part-way through an incremental round),
//! - `"verify":["<id>",...]` (omitted when empty): indexed events a resync did not list and that
//!   are still to be checked, [`MAX_VERIFY`] per pass.
//!
//! The window of a delta link is fixed when it is created. Once it has slid by more than
//! [`REWINDOW_SECS`], a plain resync lists the current window, so new future events are picked
//! up. A pass without a cursor while events are indexed (a settings update resets the cursor)
//! is a *reconciling* full listing: indexed events it does not return are deleted when they lie
//! outside the (possibly new) window, and checked otherwise. A plain resync (410,
//! `syncStateNotFound`, or a slid window) checks only events inside the window; the others are
//! expected to be missing and stay. A resync interrupted part-way restarts its listing.
//!
//! Versions are `<changeKey>#<start>:<end>` (Unix seconds), so the window test needs no request.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use nugget_connector::{
    AuthFlow, AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext,
    ConnectorFactory, Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::graph::{DeltaPage, Get, GraphApi, is_removed, needs_resync, status_error};
use crate::oauth::{SCOPE_CALENDARS_READ, SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ};
use crate::settings::{CalendarSettings, valid_folder_id};
use crate::text::{clean_line, plain_text, strip_html};
use crate::{MicrosoftEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "outlook-calendar";
const FEATURE: &str = "sync:outlook-calendar";
const SCOPES: &[&str] = &[SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ, SCOPE_CALENDARS_READ];
const PAGE_SIZE: &str = "odata.maxpagesize=100";
const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";
const UTC_TIMES: &str = "outlook.timezone=\"UTC\"";
/// Unlisted events checked per pass; the rest wait in the cursor.
const MAX_VERIFY: usize = 500;
/// Longest list of events carried in the cursor for later checks.
const MAX_CARRY: usize = 20_000;
/// Slack on the window boundary when deciding whether an unlisted event should have been listed.
const WINDOW_SLACK_SECS: i64 = 86_400;
/// How far the window may slide before the listing is redone.
pub(crate) const REWINDOW_SECS: i64 = 7 * 86_400;
const VERIFY_CONCURRENCY: usize = 4;
const MAX_BODY_CHARS: usize = 20_000;
const MAX_ATTENDEES: usize = 200;
const MAX_RESTARTS: u32 = 3;
const MAX_PAGES: u32 = 100_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    /// Window of the current listing / delta link (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_link: Option<String>,
    /// A full listing of the window is in progress.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    full: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resync: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reconcile: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verify: Vec<String>,
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn fresh(resync: bool, reconcile: bool) -> Self {
        Self {
            v: 1,
            full: true,
            resync,
            reconcile,
            ..Default::default()
        }
    }
}

/// Factory for `outlook-calendar` sources.
#[derive(Debug, Clone, Default)]
pub struct OutlookCalendarFactory {
    endpoints: MicrosoftEndpoints,
    retry: RetryPolicy,
}

impl OutlookCalendarFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_endpoints(mut self, endpoints: MicrosoftEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

impl ConnectorFactory for OutlookCalendarFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "Outlook calendar",
            auth: AuthKind::OAuth,
            settings_schema: CalendarSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        CalendarSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = CalendarSettings::parse(settings)?;
        let api = GraphApi::new(
            FEATURE,
            SCOPES,
            &settings.common(),
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        Ok(Arc::new(OutlookCalendarConnector { api, settings, ctx }))
    }
}

/// One account's default Outlook calendar.
pub struct OutlookCalendarConnector {
    api: GraphApi,
    settings: CalendarSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for OutlookCalendarConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlookCalendarConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct DateTimeTz {
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
    #[serde(rename = "timeZone")]
    time_zone: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EmailAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Recipient {
    #[serde(rename = "emailAddress")]
    email: EmailAddress,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ResponseStatus {
    response: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Attendee {
    #[serde(rename = "type")]
    kind: Option<String>,
    status: Option<ResponseStatus>,
    #[serde(rename = "emailAddress")]
    email: EmailAddress,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Location {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Body {
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    content: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OnlineMeeting {
    #[serde(rename = "joinUrl")]
    join_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Event {
    id: String,
    #[serde(rename = "changeKey")]
    change_key: Option<String>,
    subject: Option<String>,
    body: Option<Body>,
    #[serde(rename = "bodyPreview")]
    body_preview: Option<String>,
    start: Option<DateTimeTz>,
    end: Option<DateTimeTz>,
    #[serde(rename = "originalStartTimeZone")]
    original_start_time_zone: Option<String>,
    #[serde(rename = "isAllDay")]
    is_all_day: Option<bool>,
    #[serde(rename = "isCancelled")]
    is_cancelled: Option<bool>,
    location: Option<Location>,
    locations: Vec<Location>,
    organizer: Option<Recipient>,
    attendees: Vec<Attendee>,
    #[serde(rename = "onlineMeeting")]
    online_meeting: Option<OnlineMeeting>,
    #[serde(rename = "onlineMeetingUrl")]
    online_meeting_url: Option<String>,
    #[serde(rename = "onlineMeetingProvider")]
    online_meeting_provider: Option<String>,
    #[serde(rename = "webLink")]
    web_link: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "seriesMasterId")]
    series_master_id: Option<String>,
    #[serde(rename = "iCalUId")]
    ical_uid: Option<String>,
    #[serde(rename = "showAs")]
    show_as: Option<String>,
    sensitivity: Option<String>,
    categories: Vec<String>,
    #[serde(rename = "createdDateTime")]
    created: Option<String>,
    #[serde(rename = "lastModifiedDateTime")]
    last_modified: Option<String>,
}

#[async_trait]
impl Connector for OutlookCalendarConnector {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    async fn begin_auth(&self) -> Result<AuthFlow> {
        self.api.begin_auth().await
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.api.ensure_token().await?;
        let parsed = cursor.and_then(|v| match serde_json::from_value::<Cursor>(v) {
            Ok(c) if c.full || c.delta_link.is_some() => Some(c),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(source = %self.ctx.source, "unreadable outlook-calendar cursor ({e}); starting over");
                None
            }
        });
        let mut cur = match parsed {
            Some(c) => c,
            None => {
                // No cursor but indexed events: a reconciling pass (see the module docs).
                let reconcile = !self.ctx.known.versions().await?.is_empty();
                if reconcile {
                    tracing::info!(source = %self.ctx.source, "outlook-calendar cursor reset; reconciling resync");
                }
                Cursor::fresh(reconcile, reconcile)
            }
        };
        if !cur.full && cur.next_link.is_none() && self.window_slid(&cur) {
            tracing::info!(source = %self.ctx.source, "outlook-calendar window moved; relisting");
            cur = Cursor::fresh(true, false);
        }
        self.listing(cur, sink, cancel).await.map(|c| c.to_value())
    }
}

impl OutlookCalendarConnector {
    fn window(&self) -> (i64, i64) {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        (
            now - i64::from(self.settings.past_days) * 86_400,
            now + i64::from(self.settings.future_days) * 86_400,
        )
    }

    fn window_slid(&self, cur: &Cursor) -> bool {
        let (start, _) = self.window();
        cur.start.is_none_or(|s| start - s > REWINDOW_SECS)
    }

    fn initial_url(&self, start: i64, end: i64) -> Result<url::Url> {
        let fmt = |t: i64| {
            OffsetDateTime::from_unix_timestamp(t)
                .ok()
                .and_then(|t| t.format(&Rfc3339).ok())
                .unwrap_or_default()
        };
        self.api.url(
            &["me", "calendarView", "delta"],
            &[("startDateTime", &fmt(start)), ("endDateTime", &fmt(end))],
        )
    }

    async fn listing(
        &self,
        mut cur: Cursor,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Cursor> {
        if cur.resync && cur.full && cur.next_link.is_some() {
            // Pages listed before the interruption are not remembered: list again from the
            // start (same window) rather than take their events for missing.
            cur.next_link = None;
        }
        let mut seen: HashSet<String> = HashSet::new();
        // Indexed versions, kept up to date with what this pass sends: an event listed again
        // unchanged (a resync, or a repeat on a later page) is not sent again.
        let mut live: HashMap<String, Option<String>> = self
            .ctx
            .known
            .versions()
            .await?
            .into_iter()
            .map(|(k, v)| (k.as_str().to_string(), v))
            .collect();
        let mut restarts = 0u32;
        let mut pages = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::Invalid(
                    "outlook-calendar: delta listing does not end".into(),
                ));
            }
            if cur.full && cur.next_link.is_none() && (cur.start.is_none() || cur.end.is_none()) {
                let (s, e) = self.window();
                cur.start = Some(s);
                cur.end = Some(e);
            }
            let url = match (&cur.next_link, &cur.delta_link) {
                (Some(n), _) => self.api.link(n)?,
                (None, Some(d)) if !cur.full => self.api.link(d)?,
                _ => self.initial_url(cur.start.unwrap_or(0), cur.end.unwrap_or(0))?,
            };
            let get = Get {
                prefer: &[PAGE_SIZE, IMMUTABLE_IDS, UTC_TIMES],
                ..Default::default()
            };
            let resp = self.api.get(&url, get, cancel).await?;
            if needs_resync(&resp) {
                if restarts >= MAX_RESTARTS {
                    return Err(Error::Other(
                        "outlook-calendar: delta token keeps expiring".into(),
                    ));
                }
                tracing::info!(source = %self.ctx.source, "outlook-calendar delta token expired; full resync");
                restarts += 1;
                cur = Cursor::fresh(true, cur.reconcile);
                seen.clear();
                continue;
            }
            if resp.status.as_u16() != 200 {
                return Err(status_error("outlook calendarView delta", &resp));
            }
            let page: DeltaPage = GraphApi::json(&resp, "outlook calendarView delta")?;
            let next = page
                .next_link
                .filter(|l| !l.is_empty())
                .map(|l| self.api.link(&l).map(|_| l))
                .transpose()?;
            let delta = page
                .delta_link
                .filter(|l| !l.is_empty())
                .map(|l| self.api.link(&l).map(|_| l))
                .transpose()?;
            if next.is_none() && delta.is_none() {
                return Err(Error::Invalid(
                    "outlook-calendar delta page has neither nextLink nor deltaLink".into(),
                ));
            }
            for item in page.value {
                if !item.is_object() {
                    continue;
                }
                let removed = is_removed(&item);
                let Ok(ev) = serde_json::from_value::<Event>(item) else {
                    tracing::debug!(source = %self.ctx.source, "skipping malformed event entry");
                    continue;
                };
                if !valid_event_id(&ev.id) {
                    continue;
                }
                if !removed && ev.kind.as_deref() == Some("seriesMaster") {
                    // The calendar view lists every occurrence; a series master alongside them
                    // would duplicate the first one.
                    continue;
                }
                if removed || ev.is_cancelled == Some(true) {
                    seen.remove(&ev.id);
                    live.remove(&ev.id);
                    sink.send(Change::Delete(ItemKey::new(ev.id))).await?;
                } else {
                    seen.insert(ev.id.clone());
                    let doc = event_document(ev);
                    let key = doc.key.as_str().to_string();
                    if doc.version.is_some() && live.get(&key) == Some(&doc.version) {
                        continue;
                    }
                    live.insert(key, doc.version.clone());
                    sink.send(Change::Upsert(doc)).await?;
                }
            }
            if let Some(n) = next {
                cur.next_link = Some(n);
                sink.checkpoint(cur.to_value()).await?;
                continue;
            }
            // Last page: check unlisted events (a fresh resync's, or those carried over).
            let mut verify = std::mem::take(&mut cur.verify);
            if cur.full && cur.resync {
                let known = self.ctx.known.versions().await?;
                let window = cur.start.zip(cur.end);
                let missing = missing_candidates(&known, &seen, window);
                if cur.reconcile {
                    for key in missing.out_of_window {
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                }
                verify = missing.check;
            } else if !verify.is_empty() {
                let known = self.ctx.known.versions().await?;
                verify.retain(|k| known.contains_key(&ItemKey::new(k.as_str())));
            }
            let verify = self.verify_batch(verify, sink, cancel).await?;
            return Ok(Cursor {
                v: 1,
                start: cur.start,
                end: cur.end,
                delta_link: delta,
                verify,
                ..Default::default()
            });
        }
    }

    /// Check the first [`MAX_VERIFY`] ids of `missing` (in order) and delete the gone or
    /// cancelled events; return the rest (capped at [`MAX_CARRY`]) for the next pass.
    async fn verify_batch(
        &self,
        mut missing: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        let rest = if missing.len() > MAX_VERIFY {
            let mut rest = missing.split_off(MAX_VERIFY);
            tracing::info!(
                source = %self.ctx.source,
                "outlook-calendar: checking {MAX_VERIFY} unlisted events now, {} on later passes",
                rest.len()
            );
            rest.truncate(MAX_CARRY);
            rest
        } else {
            Vec::new()
        };
        let mut checks = futures::stream::iter(missing)
            .map(|id: String| async move {
                let exists = self.event_exists(&id, cancel).await;
                (id, exists)
            })
            .buffered(VERIFY_CONCURRENCY);
        while let Some((id, exists)) = checks.next().await {
            if !exists? {
                sink.send(Change::Delete(ItemKey::new(id))).await?;
            }
        }
        Ok(rest)
    }

    async fn event_exists(&self, id: &str, cancel: &CancellationToken) -> Result<bool> {
        if !valid_event_id(id) {
            return Ok(false);
        }
        let url = self
            .api
            .url(&["me", "events", id], &[("$select", "id,isCancelled")])?;
        let get = Get {
            prefer: &[IMMUTABLE_IDS],
            ..Default::default()
        };
        let resp = self.api.get(&url, get, cancel).await?;
        match resp.status.as_u16() {
            404 | 410 => Ok(false),
            200 => {
                let ev: Event = GraphApi::json(&resp, "outlook events.get")?;
                Ok(ev.is_cancelled != Some(true))
            }
            _ => Err(status_error("outlook events.get", &resp)),
        }
    }
}

/// The span in a version written by [`event_document`].
fn version_span(version: Option<&str>) -> Option<(i64, i64)> {
    let (_, span) = version?.rsplit_once('#')?;
    let (s, e) = span.split_once(':')?;
    Some((s.parse().ok()?, e.parse().ok()?))
}

/// Indexed events a resync listing did not return.
struct Missing {
    check: Vec<String>,
    out_of_window: Vec<String>,
}

/// The calendar view keeps events that end after the window start and begin before its end.
fn missing_candidates(
    known: &HashMap<ItemKey, Option<String>>,
    seen: &HashSet<String>,
    window: Option<(i64, i64)>,
) -> Missing {
    let mut check = Vec::new();
    let mut out_of_window = Vec::new();
    for (k, v) in known {
        if seen.contains(k.as_str()) {
            continue;
        }
        let in_window = match (version_span(v.as_deref()), window) {
            (Some((s, e)), Some((min, max))) => {
                e > min - WINDOW_SLACK_SECS && s < max + WINDOW_SLACK_SECS
            }
            _ => true,
        };
        if in_window {
            check.push(k.as_str().to_string());
        } else {
            out_of_window.push(k.as_str().to_string());
        }
    }
    check.sort();
    out_of_window.sort();
    Missing {
        check,
        out_of_window,
    }
}

fn valid_event_id(id: &str) -> bool {
    valid_folder_id(id)
}

// ---- Time zones ----

/// Windows time zone names (as Outlook reports them) → IANA, after CLDR `windowsZones.xml`.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Cuba Standard Time", "America/Havana"),
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Eastern Standard Time", "America/New_York"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("Mountain Standard Time", "America/Denver"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("UTC", "UTC"),
    ("UTC+12", "Etc/GMT-12"),
    ("UTC+13", "Etc/GMT-13"),
    ("UTC-02", "Etc/GMT+2"),
    ("UTC-08", "Etc/GMT+8"),
    ("UTC-09", "Etc/GMT+9"),
    ("UTC-11", "Etc/GMT+11"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
];

/// A Graph time zone name (IANA, Windows, `UTC`, `tzone://Microsoft/Utc`) as a time zone.
fn zone(name: &str) -> Option<TimeZone> {
    let name = name.trim();
    if name.is_empty() || name.len() > 100 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "utc" | "etc/utc" | "gmt" | "coordinated universal time" | "tzone://microsoft/utc"
    ) {
        return Some(TimeZone::UTC);
    }
    if let Some((_, iana)) = WINDOWS_ZONES
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(name))
    {
        return TimeZone::get(iana).ok();
    }
    if name.contains('/') && !name.starts_with("tzone:") {
        return TimeZone::get(name).ok();
    }
    None
}

/// A Graph `dateTimeTimeZone` as an instant. The date-time carries no offset; its zone is the
/// `timeZone` field (UTC when we asked for it).
fn instant(t: &DateTimeTz) -> Option<Timestamp> {
    let dt: jiff::civil::DateTime = t.date_time.as_deref()?.trim().parse().ok()?;
    let tz = zone(t.time_zone.as_deref().unwrap_or("UTC"))?;
    dt.to_zoned(tz).ok().map(|z| z.timestamp())
}

fn civil_date(t: &DateTimeTz) -> Option<jiff::civil::Date> {
    let dt: jiff::civil::DateTime = t.date_time.as_deref()?.trim().parse().ok()?;
    Some(dt.date())
}

fn fmt_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
}

/// "2025-11-03 10:00 – 11:30 (Pacific Standard Time, UTC-08:00)" in the event's own time zone
/// (UTC when it is unknown), or "2025-11-03 (all day)".
fn describe_when(ev: &Event) -> Option<String> {
    let start = ev.start.as_ref()?;
    if ev.is_all_day == Some(true) {
        let first = civil_date(start);
        // All-day end dates are exclusive.
        let last = ev
            .end
            .as_ref()
            .and_then(civil_date)
            .and_then(|d| d.yesterday().ok());
        return Some(match (first, last) {
            (Some(f), Some(l)) if l > f => format!("{f} – {l} (all day)"),
            (Some(f), _) => format!("{f} (all day)"),
            _ => format!(
                "{} (all day)",
                clean_line(start.date_time.as_deref().unwrap_or("?"), 40)
            ),
        });
    }
    let Some(s) = instant(start) else {
        return start.date_time.as_deref().map(|d| clean_line(d, 40));
    };
    let e = ev.end.as_ref().and_then(instant);
    let label = ev
        .original_start_time_zone
        .as_deref()
        .filter(|n| zone(n).is_some())
        .or(start.time_zone.as_deref().filter(|n| zone(n).is_some()));
    let tz = label.and_then(zone).unwrap_or(TimeZone::UTC);
    let s = s.to_zoned(tz.clone());
    let hm = |z: &jiff::Zoned| format!("{:02}:{:02}", z.hour(), z.minute());
    let mut out = format!("{} {}", s.date(), hm(&s));
    if let Some(e) = e {
        let e = e.to_zoned(tz);
        if e.date() == s.date() {
            out.push_str(&format!(" – {}", hm(&e)));
        } else {
            out.push_str(&format!(" – {} {}", e.date(), hm(&e)));
        }
    }
    let off = fmt_offset(s.offset().seconds());
    out.push_str(&match label {
        Some(l) if !l.starts_with("tzone:") && !l.eq_ignore_ascii_case("utc") => {
            format!(" ({}, {off})", clean_line(l, 100))
        }
        _ => format!(" ({off})"),
    });
    Some(out)
}

/// An event's `(start, end)` in Unix seconds.
fn event_span(ev: &Event) -> Option<(i64, i64)> {
    let start = instant(ev.start.as_ref()?)?.as_second();
    let end = ev
        .end
        .as_ref()
        .and_then(instant)
        .map_or(start, |t| t.as_second());
    Some((start, end))
}

fn person(e: &EmailAddress) -> Option<String> {
    let name = e
        .name
        .as_deref()
        .map(|s| clean_line(s, 200))
        .filter(|s| !s.is_empty());
    let addr = e
        .address
        .as_deref()
        .map(|s| clean_line(s, 320))
        .filter(|s| !s.is_empty());
    match (name, addr) {
        (Some(n), Some(a)) if n != a => Some(format!("{n} <{a}>")),
        (Some(n), _) => Some(n),
        (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

fn https(url: Option<&str>) -> Option<String> {
    url.map(str::trim)
        .filter(|u| {
            u.starts_with("https://") && u.len() <= 4096 && !u.chars().any(char::is_control)
        })
        .map(str::to_string)
}

fn event_document(ev: Event) -> Document {
    let title = ev
        .subject
        .as_deref()
        .map(|s| clean_line(s, 500))
        .filter(|s| !s.is_empty());
    let when = describe_when(&ev);
    let organizer = ev.organizer.as_ref().and_then(|o| person(&o.email));
    let attendees: Vec<&Attendee> = ev.attendees.iter().take(MAX_ATTENDEES).collect();
    let mut places: Vec<String> = Vec::new();
    for l in ev.location.iter().chain(ev.locations.iter()) {
        if let Some(n) = l.display_name.as_deref().map(|n| clean_line(n, 500))
            && !n.is_empty()
            && !places.contains(&n)
            && places.len() < 20
        {
            places.push(n);
        }
    }
    let join = https(
        ev.online_meeting
            .as_ref()
            .and_then(|m| m.join_url.as_deref()),
    )
    .or_else(|| https(ev.online_meeting_url.as_deref()));

    let mut text = String::new();
    text.push_str(title.as_deref().unwrap_or("(no title)"));
    text.push('\n');
    if let Some(w) = &when {
        text.push_str(&format!("When: {w}\n"));
    }
    if !places.is_empty() {
        text.push_str(&format!("Where: {}\n", places.join("; ")));
    }
    if let Some(o) = &organizer {
        text.push_str(&format!("Organizer: {o}\n"));
    }
    let people: Vec<String> = attendees
        .iter()
        .filter(|a| a.kind.as_deref() != Some("resource"))
        .filter_map(|a| {
            let p = person(&a.email)?;
            let mut extra = Vec::new();
            if let Some(r) = a.status.as_ref().and_then(|s| s.response.as_deref()) {
                extra.push(match r {
                    "none" | "notResponded" => "no response",
                    "accepted" => "accepted",
                    "declined" => "declined",
                    "tentativelyAccepted" => "maybe",
                    "organizer" => "organizer",
                    _ => "unknown response",
                });
            }
            if a.kind.as_deref() == Some("optional") {
                extra.push("optional");
            }
            Some(if extra.is_empty() {
                p
            } else {
                format!("{p} ({})", extra.join(", "))
            })
        })
        .collect();
    if !people.is_empty() {
        text.push_str(&format!("Attendees: {}\n", people.join("; ")));
    }
    if let Some(j) = &join {
        text.push_str(&format!("Online meeting: {j}\n"));
    }
    let body = ev.body.as_ref().and_then(|b| {
        let content = b.content.as_deref()?;
        Some(
            if b.content_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("html"))
            {
                strip_html(content, MAX_BODY_CHARS)
            } else {
                plain_text(content, MAX_BODY_CHARS)
            },
        )
    });
    let body = body
        .filter(|b| !b.is_empty())
        .or_else(|| {
            ev.body_preview
                .as_deref()
                .map(|p| plain_text(p, MAX_BODY_CHARS))
        })
        .filter(|b| !b.is_empty());
    if let Some(b) = &body {
        text.push('\n');
        text.push_str(b);
        text.push('\n');
    }

    let time_json = |t: Option<&DateTimeTz>| {
        t.map(|t| json!({"date_time": t.date_time, "time_zone": t.time_zone}))
    };
    let span = event_span(&ev);
    let metadata = json!({
        "event_id": ev.id,
        "type": ev.kind,
        "series_master_id": ev.series_master_id,
        "ical_uid": ev.ical_uid,
        "start": time_json(ev.start.as_ref()),
        "end": time_json(ev.end.as_ref()),
        "start_unix": span.map(|s| s.0),
        "end_unix": span.map(|s| s.1),
        "all_day": ev.is_all_day.unwrap_or(false),
        "time_zone": ev.original_start_time_zone.as_deref().map(|z| clean_line(z, 100)),
        "location": places,
        "organizer": ev.organizer.as_ref().map(|o| json!({"email": o.email.address, "name": o.email.name})),
        "attendees": attendees.iter().map(|a| json!({
            "email": a.email.address, "name": a.email.name, "type": a.kind,
            "response": a.status.as_ref().and_then(|s| s.response.clone()),
        })).collect::<Vec<_>>(),
        "online_meeting_url": join,
        "online_meeting_provider": ev.online_meeting_provider,
        "show_as": ev.show_as,
        "sensitivity": ev.sensitivity,
        "categories": ev.categories.iter().take(50).map(|c| clean_line(c, 200)).collect::<Vec<_>>(),
        "created": ev.created,
        "updated": ev.last_modified,
    });

    let mut doc = Document::new(ev.id.clone(), "text/plain", Content::Text(text));
    doc.title = title;
    doc.version = ev
        .change_key
        .clone()
        .or_else(|| ev.last_modified.clone())
        .map(|v| match span {
            Some((s, e)) => format!("{v}#{s}:{e}"),
            None => v,
        });
    doc.modified_at = ev
        .last_modified
        .as_deref()
        .and_then(|u| OffsetDateTime::parse(u, &Rfc3339).ok());
    doc.url = https(ev.web_link.as_deref());
    doc.metadata = metadata;
    doc
}

#[cfg(test)]
mod unit {
    use super::*;

    fn t(dt: &str, tz: &str) -> Option<DateTimeTz> {
        Some(DateTimeTz {
            date_time: Some(dt.into()),
            time_zone: Some(tz.into()),
        })
    }

    #[test]
    fn describes_times_in_the_original_zone() {
        let ev = Event {
            start: t("2025-11-03T18:00:00.0000000", "UTC"),
            end: t("2025-11-03T19:30:00.0000000", "UTC"),
            original_start_time_zone: Some("Pacific Standard Time".into()),
            ..Default::default()
        };
        assert_eq!(
            describe_when(&ev).unwrap(),
            "2025-11-03 10:00 – 11:30 (Pacific Standard Time, UTC-08:00)"
        );
        let ev = Event {
            start: t("2025-07-01T22:00:00", "UTC"),
            end: t("2025-07-01T23:00:00", "UTC"),
            original_start_time_zone: Some("Europe/Paris".into()),
            ..Default::default()
        };
        assert_eq!(
            describe_when(&ev).unwrap(),
            "2025-07-02 00:00 – 01:00 (Europe/Paris, UTC+02:00)"
        );
        // Unknown zones fall back to UTC.
        let ev = Event {
            start: t("2025-07-01T22:00:00", "UTC"),
            original_start_time_zone: Some("tzone://Microsoft/Custom".into()),
            ..Default::default()
        };
        assert_eq!(describe_when(&ev).unwrap(), "2025-07-01 22:00 (UTC+00:00)");
    }

    #[test]
    fn describes_all_day_and_garbage() {
        let ev = Event {
            is_all_day: Some(true),
            start: t("2025-11-03T00:00:00.0000000", "UTC"),
            end: t("2025-11-06T00:00:00.0000000", "UTC"),
            ..Default::default()
        };
        assert_eq!(
            describe_when(&ev).unwrap(),
            "2025-11-03 – 2025-11-05 (all day)"
        );
        let ev = Event {
            is_all_day: Some(true),
            start: t("2025-11-03T00:00:00", "UTC"),
            end: t("2025-11-04T00:00:00", "UTC"),
            ..Default::default()
        };
        assert_eq!(describe_when(&ev).unwrap(), "2025-11-03 (all day)");
        let ev = Event {
            start: t("nope", "UTC"),
            ..Default::default()
        };
        assert_eq!(describe_when(&ev).unwrap(), "nope");
        assert!(describe_when(&Event::default()).is_none());
    }

    #[test]
    fn zones_resolve() {
        assert!(zone("W. Europe Standard Time").is_some());
        assert!(zone("utc").is_some());
        assert!(zone("America/New_York").is_some());
        assert!(zone("Mars/Olympus").is_none());
        assert!(zone("../../etc/passwd").is_none());
        assert!(zone("").is_none());
        for (w, iana) in WINDOWS_ZONES {
            assert!(TimeZone::get(iana).is_ok(), "{w} → {iana}");
        }
    }

    #[test]
    fn version_spans_round_trip() {
        assert_eq!(version_span(Some("ck==#10:20")), Some((10, 20)));
        assert_eq!(version_span(Some("ck")), None);
    }
}
