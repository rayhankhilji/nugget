//! `gcal`: Google Calendar over the REST API with `calendar.readonly`.
//!
//! Cursor (JSON): `{"v":1,"calendars":{"<calendarId>":<state>}}` where `<state>` is
//! - `{"sync_token":"<tok>"}` once a calendar is in incremental mode, or
//! - `{"time_min":"<rfc3339>","time_max":"<rfc3339>","page_token":"<tok>"|null,"resync":bool,"reconcile":bool}`
//!   while a full (windowed) listing is in progress,
//!
//! plus `"verify":["<key>",...]` (omitted when empty): indexed events a resync did not list and
//! that are still to be checked, [`MAX_VERIFY`] per pass.
//!
//! Calendars that are no longer synced are found from the indexed keys (`<calendarId>/<eventId>`),
//! not only from the cursor, so a reset cursor still drops them. A calendar with indexed events
//! but no cursor state (a settings update resets the cursor) gets a *reconciling* full listing:
//! indexed events it does not return are deleted when they lie outside the (possibly new)
//! window, and checked otherwise. A plain resync (410) checks only events inside the window;
//! the others are expected to be missing and stay. A resync interrupted part-way
//! restarts its listing, so no event is wrongly taken as missing.
//!
//! Versions are `<etag>#<start>:<end>` (Unix seconds), so the window test needs no request.
//!
//! Events are parsed one by one: a malformed entry never fails its page. Its `id` and
//! `status` are still read, so a malformed cancellation is still a delete, and any other
//! malformed event counts as listed (its indexed version is kept, never taken for missing).
//! Existence checks read only `status`, and a check that fails for one event keeps it.
//!
//! Scopes: `calendar.readonly` only (list calendars and read events); nothing is ever written.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
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

use crate::api::{GoogleApi, OAuthSettings, PageGuard, api_url};
use crate::settings::CalendarSettings;
use crate::text::{strip_html, truncate_chars};
use crate::{GoogleEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "gcal";
const FEATURE: &str = "sync:gcal";
const EVENTS_PAGE: &str = "250";
/// Unlisted events checked per pass; the rest wait in the cursor.
const MAX_VERIFY: usize = 500;
/// Longest list of events carried in the cursor for later checks.
const MAX_CARRY: usize = 20_000;
/// Slack on the window boundary when deciding whether an unlisted event should have been listed.
const WINDOW_SLACK_SECS: i64 = 86_400;
const VERIFY_CONCURRENCY: usize = 4;
const MAX_DESCRIPTION: usize = 20_000;
const MAX_ATTENDEES: usize = 200;
/// Guard against a server that keeps invalidating tokens.
const MAX_RESTARTS: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct CalState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time_min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time_max: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_token: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resync: bool,
    /// With `resync`: check every indexed event the listing misses, not only those in the window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reconcile: bool,
    /// Keys still to be checked after a resync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verify: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default)]
    calendars: BTreeMap<String, CalState>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            v: 1,
            calendars: BTreeMap::new(),
        }
    }
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `gcal` sources.
#[derive(Debug, Clone, Default)]
pub struct CalendarFactory {
    endpoints: GoogleEndpoints,
    retry: RetryPolicy,
}

impl CalendarFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_endpoints(mut self, endpoints: GoogleEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

impl ConnectorFactory for CalendarFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "Google Calendar",
            auth: AuthKind::OAuth,
            settings_schema: CalendarSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        CalendarSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = CalendarSettings::parse(settings)?;
        let api = GoogleApi::new(
            FEATURE,
            nugget_oauth::google::SCOPE_CALENDAR_READONLY,
            OAuthSettings {
                client_id: settings.client_id.clone(),
                client_secret: settings.client_secret.clone(),
                login_hint: settings.account_hint.clone(),
            },
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        Ok(Arc::new(CalendarConnector {
            api,
            base: self.endpoints.calendar_api.clone(),
            settings,
            ctx,
        }))
    }
}

/// One Google account's calendars.
pub struct CalendarConnector {
    api: GoogleApi,
    base: String,
    settings: CalendarSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for CalendarConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalendarConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

/// `null` reads as the default (Google sends `"attendees": null` and the like).
fn null_default<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// What is read from an event entry that does not parse as a whole.
#[derive(Deserialize, Default)]
struct EventStub {
    #[serde(default, deserialize_with = "lenient_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    status: Option<String>,
}

/// A string, or nothing for any other JSON value.
fn lenient_string<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

#[derive(Deserialize)]
struct CalendarList {
    #[serde(default)]
    items: Vec<CalendarEntry>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct CalendarEntry {
    id: String,
    #[serde(default, deserialize_with = "null_default")]
    selected: bool,
    #[serde(default, deserialize_with = "null_default")]
    deleted: bool,
}

#[derive(Deserialize)]
struct EventList {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "timeZone")]
    time_zone: Option<String>,
    /// Parsed one by one, so one malformed event does not fail the page.
    #[serde(default, deserialize_with = "null_default")]
    items: Vec<Value>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "nextSyncToken")]
    next_sync_token: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Event {
    id: String,
    status: Option<String>,
    etag: Option<String>,
    updated: Option<String>,
    created: Option<String>,
    #[serde(rename = "htmlLink")]
    html_link: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    organizer: Option<Person>,
    #[serde(deserialize_with = "null_default")]
    attendees: Vec<Attendee>,
    start: Option<EventTime>,
    end: Option<EventTime>,
    #[serde(rename = "recurringEventId")]
    recurring_event_id: Option<String>,
    #[serde(rename = "iCalUID")]
    ical_uid: Option<String>,
    #[serde(rename = "hangoutLink")]
    hangout_link: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Person {
    email: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Attendee {
    email: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "responseStatus")]
    response_status: Option<String>,
    #[serde(deserialize_with = "null_default")]
    optional: bool,
    #[serde(deserialize_with = "null_default")]
    resource: bool,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct EventTime {
    date: Option<String>,
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
    #[serde(rename = "timeZone")]
    time_zone: Option<String>,
}

/// Names and default time zone of the calendar being listed.
#[derive(Default, Clone)]
struct CalInfo {
    id: String,
    summary: Option<String>,
    time_zone: Option<String>,
}

#[async_trait]
impl Connector for CalendarConnector {
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
        let mut cur = cursor
            .and_then(|v| match serde_json::from_value::<Cursor>(v) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(source = %self.ctx.source, "unreadable gcal cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        let calendars = match self.settings.calendar_ids() {
            Some(c) => c,
            None => self.selected_calendars(cancel).await?,
        };

        // Calendars no longer synced: drop their events from the index. They are found from the
        // indexed keys as well as the cursor, which a settings update resets.
        let known = self.ctx.known.versions().await?;
        let known_cals: BTreeSet<&str> = known
            .keys()
            .filter_map(|k| k.as_str().split_once('/').map(|(c, _)| c))
            .collect();
        let wanted: HashSet<&str> = calendars.iter().map(String::as_str).collect();
        let dropped: BTreeSet<String> = cur
            .calendars
            .keys()
            .map(String::as_str)
            .chain(known_cals.iter().copied())
            .filter(|c| !wanted.contains(c))
            .map(str::to_string)
            .collect();
        if !dropped.is_empty() {
            for cal in &dropped {
                let prefix = format!("{cal}/");
                for key in known.keys().filter(|k| k.as_str().starts_with(&prefix)) {
                    sink.send(Change::Delete(key.clone())).await?;
                }
                cur.calendars.remove(cal);
            }
            sink.checkpoint(cur.to_value()).await?;
        }
        // Indexed events but no cursor state: a reconciling full listing.
        for cal in &calendars {
            if !cur.calendars.contains_key(cal) && known_cals.contains(cal.as_str()) {
                tracing::info!(source = %self.ctx.source, calendar = %cal, "gcal cursor reset; reconciling resync");
                cur.calendars.insert(
                    cal.clone(),
                    CalState {
                        resync: true,
                        reconcile: true,
                        ..Default::default()
                    },
                );
            }
        }
        drop(known);

        for cal in &calendars {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.sync_calendar(cal, &mut cur, sink, cancel).await?;
            sink.checkpoint(cur.to_value()).await?;
        }
        Ok(cur.to_value())
    }
}

impl CalendarConnector {
    /// Every calendar the user has selected (shown) in Google Calendar.
    async fn selected_calendars(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..50 {
            let mut query = vec![("maxResults", "250"), ("minAccessRole", "reader")];
            if let Some(p) = &page {
                query.push(("pageToken", p));
            }
            let url = api_url(&self.base, &["users", "me", "calendarList"], &query)?;
            let resp = self.api.get(&url, cancel).await?;
            if resp.status.as_u16() != 200 {
                return Err(Error::Other(format!(
                    "gcal calendarList: HTTP {}",
                    resp.status
                )));
            }
            let list: CalendarList = GoogleApi::json(&resp, "gcal calendarList")?;
            out.extend(
                list.items
                    .into_iter()
                    .filter(|c| c.selected && !c.deleted && valid_calendar_id(&c.id))
                    .map(|c| c.id),
            );
            match list.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => page = Some(t),
                None => break,
            }
        }
        Ok(out)
    }

    fn window(&self) -> (String, String) {
        let now = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .unwrap_or(OffsetDateTime::now_utc());
        let min = now - time::Duration::days(i64::from(self.settings.past_days));
        let max = now + time::Duration::days(i64::from(self.settings.future_days));
        (
            min.format(&Rfc3339).unwrap_or_default(),
            max.format(&Rfc3339).unwrap_or_default(),
        )
    }

    async fn sync_calendar(
        &self,
        cal: &str,
        cur: &mut Cursor,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut state = cur.calendars.get(cal).cloned().unwrap_or_default();
        let mut info = CalInfo {
            id: cal.to_string(),
            ..Default::default()
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut restarts = 0u32;
        let mut pages = PageGuard::default();
        if state.resync && state.page_token.is_some() {
            // Pages listed before the interruption are not remembered: list again from the
            // start (same window) rather than take their events for missing.
            state.page_token = None;
        }
        if let Some(t) = &state.page_token {
            pages.next(t, "gcal events.list")?;
        }
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let full = state.sync_token.is_none() || state.time_min.is_some();
            if full && state.time_min.is_none() {
                let (min, max) = self.window();
                state.time_min = Some(min);
                state.time_max = Some(max);
                state.page_token = None;
                state.sync_token = None;
            }
            let mut query: Vec<(&str, &str)> =
                vec![("singleEvents", "true"), ("maxResults", EVENTS_PAGE)];
            if full {
                query.push(("timeMin", state.time_min.as_deref().unwrap_or_default()));
                query.push(("timeMax", state.time_max.as_deref().unwrap_or_default()));
            } else if let Some(t) = &state.sync_token {
                query.push(("syncToken", t));
            }
            if let Some(p) = &state.page_token {
                query.push(("pageToken", p));
            }
            let url = api_url(&self.base, &["calendars", cal, "events"], &query)?;
            let resp = self.api.get(&url, cancel).await?;
            match resp.status.as_u16() {
                200 => {}
                410 if restarts < MAX_RESTARTS => {
                    // Sync token (or page token) invalidated: full resync of this calendar.
                    tracing::info!(source = %self.ctx.source, calendar = cal, "gcal token expired (410); full resync");
                    restarts += 1;
                    pages = PageGuard::default();
                    state = CalState {
                        resync: true,
                        reconcile: state.reconcile,
                        ..Default::default()
                    };
                    seen.clear();
                    continue;
                }
                404 => {
                    return Err(Error::NotFound(format!(
                        "calendar `{cal}` not found or not shared"
                    )));
                }
                s => return Err(Error::Other(format!("gcal events.list: HTTP {s}"))),
            }
            let page: EventList = GoogleApi::json(&resp, "gcal events.list")?;
            if page.summary.is_some() {
                info.summary = page.summary.clone();
            }
            if page.time_zone.is_some() {
                info.time_zone = page.time_zone.clone();
            }
            for ev in page.items {
                let ev = match serde_json::from_value::<Event>(ev.clone()) {
                    Ok(ev) => ev,
                    Err(e) => {
                        let stub: EventStub = serde_json::from_value(ev).unwrap_or_default();
                        let Some(id) = stub.id.filter(|id| valid_event_id(id)) else {
                            tracing::warn!(source = %self.ctx.source, calendar = cal, "gcal: skipping a malformed event without an id ({e})");
                            continue;
                        };
                        let key = format!("{cal}/{id}");
                        if stub.status.as_deref() == Some("cancelled") {
                            sink.send(Change::Delete(ItemKey::new(key))).await?;
                        } else {
                            // Keep what is indexed; never take it for missing.
                            tracing::warn!(source = %self.ctx.source, calendar = cal, event = %id, "gcal: malformed event; keeping the indexed version ({e})");
                            seen.insert(key);
                        }
                        continue;
                    }
                };
                if !valid_event_id(&ev.id) {
                    continue;
                }
                let key = format!("{cal}/{}", ev.id);
                if ev.status.as_deref() == Some("cancelled") {
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                } else {
                    seen.insert(key);
                    sink.send(Change::Upsert(event_document(&info, ev))).await?;
                }
            }
            if let Some(t) = page.next_page_token.filter(|t| !t.is_empty()) {
                pages.next(&t, "gcal events.list")?;
                state.page_token = Some(t);
                cur.calendars.insert(cal.to_string(), state.clone());
                sink.checkpoint(cur.to_value()).await?;
                continue;
            }
            // Last page: check unlisted events (a fresh resync's, or those carried over).
            let mut verify = std::mem::take(&mut state.verify);
            if full && state.resync {
                let known = self.ctx.known.versions().await?;
                let window = parse_window(&state);
                let missing = missing_candidates(cal, &known, &seen, window);
                if state.reconcile {
                    // Out of scope after a settings change (e.g. a shorter window): drop them.
                    for key in missing.out_of_window {
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                }
                verify = missing.check;
            } else if !verify.is_empty() {
                let known = self.ctx.known.versions().await?;
                verify.retain(|k| known.contains_key(&ItemKey::new(k.as_str())));
            }
            let verify = self.verify_batch(cal, verify, sink, cancel).await?;
            let sync_token = page
                .next_sync_token
                .filter(|t| !t.is_empty())
                .or_else(|| if full { None } else { state.sync_token.clone() });
            let done = match sync_token {
                Some(t) => CalState {
                    sync_token: Some(t),
                    verify,
                    ..Default::default()
                },
                // No sync token offered: the next pass lists the window again.
                None => CalState {
                    verify,
                    ..Default::default()
                },
            };
            cur.calendars.insert(cal.to_string(), done);
            return Ok(());
        }
    }

    /// Check the first [`MAX_VERIFY`] keys of `missing` (in order) and delete the gone or
    /// cancelled events; return the rest (capped at [`MAX_CARRY`]) for the next pass.
    async fn verify_batch(
        &self,
        cal: &str,
        mut missing: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        let prefix = format!("{cal}/");
        missing.retain(|k| k.starts_with(&prefix));
        let rest = if missing.len() > MAX_VERIFY {
            let mut rest = missing.split_off(MAX_VERIFY);
            tracing::info!(
                source = %self.ctx.source,
                calendar = cal,
                "gcal: checking {MAX_VERIFY} unlisted events now, {} on later passes",
                rest.len()
            );
            rest.truncate(MAX_CARRY);
            rest
        } else {
            Vec::new()
        };
        let plen = prefix.len();
        let mut checks = futures::stream::iter(missing)
            .map(|key: String| async move {
                let exists = self.event_exists(cal, &key[plen..], cancel).await;
                (key, exists)
            })
            .buffered(VERIFY_CONCURRENCY);
        while let Some((key, exists)) = checks.next().await {
            let exists = match exists {
                Ok(x) => x,
                // This event only: keep it rather than fail every resync on it.
                Err(e @ (Error::Network(_) | Error::Invalid(_) | Error::Other(_))) => {
                    tracing::warn!(source = %self.ctx.source, event = %key, "gcal: could not check an event; keeping it: {e}");
                    true
                }
                Err(e) => return Err(e),
            };
            if !exists {
                sink.send(Change::Delete(ItemKey::new(key))).await?;
            }
        }
        Ok(rest)
    }

    async fn event_exists(&self, cal: &str, id: &str, cancel: &CancellationToken) -> Result<bool> {
        if !valid_event_id(id) {
            return Ok(false);
        }
        let url = api_url(&self.base, &["calendars", cal, "events", id], &[])?;
        let resp = self.api.get(&url, cancel).await?;
        match resp.status.as_u16() {
            404 | 410 => Ok(false),
            200 => {
                // Only the status matters; the rest may be malformed.
                let ev: EventStub = GoogleApi::json(&resp, "gcal events.get")?;
                Ok(ev.status.as_deref() != Some("cancelled"))
            }
            s => Err(Error::Other(format!("gcal events.get: HTTP {s}"))),
        }
    }
}

/// The listing window of a full pass, in Unix seconds.
fn parse_window(state: &CalState) -> Option<(i64, i64)> {
    let p = |s: Option<&str>| {
        OffsetDateTime::parse(s?, &Rfc3339)
            .ok()
            .map(OffsetDateTime::unix_timestamp)
    };
    Some((p(state.time_min.as_deref())?, p(state.time_max.as_deref())?))
}

/// An event's `(start, end)` in Unix seconds (all-day dates at UTC midnight).
fn event_span(ev: &Event) -> Option<(i64, i64)> {
    let secs = |t: &EventTime| {
        parse_time(t)
            .map(OffsetDateTime::unix_timestamp)
            .or_else(|| {
                let d = time::Date::parse(
                    t.date.as_deref()?,
                    &time::format_description::well_known::Iso8601::DATE,
                )
                .ok()?;
                Some(d.midnight().assume_utc().unix_timestamp())
            })
    };
    let start = secs(ev.start.as_ref()?)?;
    let end = ev.end.as_ref().and_then(secs).unwrap_or(start);
    Some((start, end))
}

/// The span in a version written by [`event_document`].
fn version_span(version: Option<&str>) -> Option<(i64, i64)> {
    let (_, span) = version?.rsplit_once('#')?;
    let (s, e) = span.split_once(':')?;
    Some((s.parse().ok()?, e.parse().ok()?))
}

/// Indexed events of one calendar a resync listing did not return.
struct Missing {
    /// Inside the window (or of unknown time): gone or cancelled unless a check says otherwise.
    check: Vec<String>,
    /// Known to lie outside the window: expected to be missing from the listing.
    out_of_window: Vec<String>,
}

/// Google's window keeps events that end after `timeMin` and start before `timeMax`.
fn missing_candidates(
    cal: &str,
    known: &HashMap<ItemKey, Option<String>>,
    seen: &HashSet<String>,
    window: Option<(i64, i64)>,
) -> Missing {
    let prefix = format!("{cal}/");
    let mut check = Vec::new();
    let mut out_of_window = Vec::new();
    for (k, v) in known {
        if !k.as_str().starts_with(&prefix) || seen.contains(k.as_str()) {
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

fn valid_calendar_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.len() <= 1024
        && !id.contains('/')
        && !id.chars().any(char::is_control)
}

/// Event ids are base32hex, plus `_` and an instance suffix for recurring instances.
fn valid_event_id(id: &str) -> bool {
    // `.` and `..` are dropped from URL paths, so they would address the collection instead.
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.len() <= 1024
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'@' || b == b'.')
}

fn person(name: Option<&str>, email: Option<&str>) -> Option<String> {
    match (
        name.filter(|s| !s.is_empty()),
        email.filter(|s| !s.is_empty()),
    ) {
        (Some(n), Some(e)) if n != e => Some(format!("{n} <{e}>")),
        (Some(n), _) => Some(n.to_string()),
        (None, Some(e)) => Some(e.to_string()),
        (None, None) => None,
    }
}

fn parse_time(t: &EventTime) -> Option<OffsetDateTime> {
    t.date_time
        .as_deref()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
}

fn fmt_date(d: time::Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn fmt_offset(o: time::UtcOffset) -> String {
    let (h, m, _) = o.as_hms();
    let sign = if o.is_negative() { '-' } else { '+' };
    format!("UTC{sign}{:02}:{:02}", h.unsigned_abs(), m.unsigned_abs())
}

/// "2025-11-03 10:00 – 11:00 (Europe/Paris, UTC+01:00)" or "2025-11-03 (all day)".
fn describe_when(
    start: Option<&EventTime>,
    end: Option<&EventTime>,
    cal_tz: Option<&str>,
) -> Option<String> {
    let start = start?;
    if let Some(d) = start.date.as_deref() {
        let first = time::Date::parse(d, &time::format_description::well_known::Iso8601::DATE).ok();
        // All-day end dates are exclusive.
        let last = end
            .and_then(|e| e.date.as_deref())
            .and_then(|e| {
                time::Date::parse(e, &time::format_description::well_known::Iso8601::DATE).ok()
            })
            .and_then(|e| e.previous_day());
        return Some(match (first, last) {
            (Some(f), Some(l)) if l > f => format!("{} – {} (all day)", fmt_date(f), fmt_date(l)),
            (Some(f), _) => format!("{} (all day)", fmt_date(f)),
            _ => format!("{d} (all day)"),
        });
    }
    let s = parse_time(start);
    let e = end.and_then(parse_time);
    let tz = start.time_zone.as_deref().or(cal_tz);
    let Some(s) = s else {
        return start.date_time.clone();
    };
    let hm = |t: OffsetDateTime| format!("{:02}:{:02}", t.hour(), t.minute());
    let mut out = format!("{} {}", fmt_date(s.date()), hm(s));
    if let Some(e) = e {
        let e = e.to_offset(s.offset());
        if e.date() == s.date() {
            out.push_str(&format!(" – {}", hm(e)));
        } else {
            out.push_str(&format!(" – {} {}", fmt_date(e.date()), hm(e)));
        }
    }
    let off = fmt_offset(s.offset());
    out.push_str(&match tz {
        Some(tz) => format!(" ({tz}, {off})"),
        None => format!(" ({off})"),
    });
    Some(out)
}

fn event_document(cal: &CalInfo, ev: Event) -> Document {
    let key = format!("{}/{}", cal.id, ev.id);
    let title = ev
        .summary
        .as_deref()
        .map(|s| truncate_chars(s.trim(), 500))
        .filter(|s| !s.is_empty());
    let when = describe_when(ev.start.as_ref(), ev.end.as_ref(), cal.time_zone.as_deref());
    let organizer = ev
        .organizer
        .as_ref()
        .and_then(|o| person(o.display_name.as_deref(), o.email.as_deref()));
    let attendees: Vec<&Attendee> = ev.attendees.iter().take(MAX_ATTENDEES).collect();

    let mut text = String::new();
    text.push_str(title.as_deref().unwrap_or("(no title)"));
    text.push('\n');
    if let Some(w) = &when {
        text.push_str(&format!("When: {w}\n"));
    }
    if let Some(l) = ev.location.as_deref().filter(|l| !l.trim().is_empty()) {
        text.push_str(&format!("Where: {}\n", truncate_chars(l.trim(), 1000)));
    }
    if let Some(o) = &organizer {
        text.push_str(&format!("Organizer: {o}\n"));
    }
    let people: Vec<String> = attendees
        .iter()
        .filter(|a| !a.resource)
        .filter_map(|a| {
            let p = person(a.display_name.as_deref(), a.email.as_deref())?;
            let mut extra = Vec::new();
            if let Some(r) = a.response_status.as_deref() {
                extra.push(match r {
                    "needsAction" => "no response",
                    "accepted" => "accepted",
                    "declined" => "declined",
                    "tentative" => "maybe",
                    other => other,
                });
            }
            if a.optional {
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
    if let Some(h) = ev.hangout_link.as_deref() {
        text.push_str(&format!("Video call: {h}\n"));
    }
    if let Some(c) = cal.summary.as_deref() {
        text.push_str(&format!("Calendar: {c}\n"));
    }
    if let Some(d) = ev.description.as_deref() {
        let d = strip_html(d, MAX_DESCRIPTION);
        if !d.is_empty() {
            text.push('\n');
            text.push_str(&d);
            text.push('\n');
        }
    }

    let time_json = |t: Option<&EventTime>| {
        t.map(|t| json!({"date": t.date, "date_time": t.date_time, "time_zone": t.time_zone}))
    };
    let metadata = json!({
        "calendar_id": cal.id,
        "calendar": cal.summary,
        "event_id": ev.id,
        "status": ev.status,
        "start": time_json(ev.start.as_ref()),
        "end": time_json(ev.end.as_ref()),
        "all_day": ev.start.as_ref().is_some_and(|s| s.date.is_some()),
        "time_zone": ev.start.as_ref().and_then(|s| s.time_zone.clone()).or(cal.time_zone.clone()),
        "location": ev.location,
        "organizer": ev.organizer.as_ref().map(|o| json!({"email": o.email, "name": o.display_name})),
        "attendees": attendees.iter().map(|a| json!({
            "email": a.email, "name": a.display_name, "response": a.response_status,
            "optional": a.optional, "resource": a.resource,
        })).collect::<Vec<_>>(),
        "recurring_event_id": ev.recurring_event_id,
        "ical_uid": ev.ical_uid,
        "hangout_link": ev.hangout_link,
        "created": ev.created,
        "updated": ev.updated,
    });

    let span = event_span(&ev);
    let mut doc = Document::new(key, "text/plain", Content::Text(text));
    doc.title = title;
    doc.version = ev
        .etag
        .clone()
        .or_else(|| ev.updated.clone())
        .map(|v| match span {
            Some((s, e)) => format!("{v}#{s}:{e}"),
            None => v,
        });
    doc.modified_at = ev
        .updated
        .as_deref()
        .and_then(|u| OffsetDateTime::parse(u, &Rfc3339).ok());
    doc.url = ev.html_link.clone();
    doc.metadata = metadata;
    doc
}

#[cfg(test)]
mod unit {
    use super::*;

    fn t(dt: &str, tz: Option<&str>) -> EventTime {
        EventTime {
            date: None,
            date_time: Some(dt.into()),
            time_zone: tz.map(Into::into),
        }
    }

    fn d(date: &str) -> EventTime {
        EventTime {
            date: Some(date.into()),
            ..Default::default()
        }
    }

    #[test]
    fn describes_times_with_zone() {
        let s = t("2025-11-03T10:00:00+01:00", Some("Europe/Paris"));
        let e = t("2025-11-03T11:30:00+01:00", None);
        assert_eq!(
            describe_when(Some(&s), Some(&e), None).unwrap(),
            "2025-11-03 10:00 – 11:30 (Europe/Paris, UTC+01:00)"
        );
        let e2 = t("2025-11-04T05:00:00Z", None);
        assert_eq!(
            describe_when(
                Some(&t("2025-11-03T22:00:00-05:00", None)),
                Some(&e2),
                Some("America/New_York")
            )
            .unwrap(),
            "2025-11-03 22:00 – 2025-11-04 00:00 (America/New_York, UTC-05:00)"
        );
    }

    #[test]
    fn describes_all_day_events() {
        assert_eq!(
            describe_when(Some(&d("2025-11-03")), Some(&d("2025-11-04")), None).unwrap(),
            "2025-11-03 (all day)"
        );
        assert_eq!(
            describe_when(Some(&d("2025-11-03")), Some(&d("2025-11-06")), None).unwrap(),
            "2025-11-03 – 2025-11-05 (all day)"
        );
        assert_eq!(
            describe_when(Some(&d("garbage")), None, None).unwrap(),
            "garbage (all day)"
        );
        assert_eq!(
            describe_when(Some(&t("nope", None)), None, None).unwrap(),
            "nope"
        );
        assert!(describe_when(None, None, None).is_none());
    }

    #[test]
    fn cursor_shape() {
        let mut c = Cursor::default();
        c.calendars.insert(
            "primary".into(),
            CalState {
                sync_token: Some("st".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            c.to_value(),
            json!({"v": 1, "calendars": {"primary": {"sync_token": "st"}}})
        );
    }
}
