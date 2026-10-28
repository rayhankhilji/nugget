//! CalDAV connector (`caldav`): calendars from any CalDAV server (Fastmail, iCloud, Nextcloud,
//! Radicale, Baïkal, ...).
//!
//! ## Settings
//! See [`CalDavSettings`]: `{url, username, calendars?}`. `url` may be the server root, a
//! principal, the calendar home or a single calendar collection. The password (usually an app
//! password) is read from the keychain under [`nugget_core::secrets::password_account`];
//! missing → `Error::Unauthorized`. Basic auth is only ever sent over https (plain http is
//! accepted for loopback hosts only).
//!
//! ## Sync model
//! Discovery: `PROPFIND` for `current-user-principal` → `calendar-home-set` → calendars.
//! Per calendar, RFC 6578 `sync-collection` with the stored `sync-token` (paged via 507
//! truncation), or, when the server has no sync support, a `getctag` check plus an ETag listing.
//! Changed members are fetched with `calendar-multiget`. An invalid token (403/409
//! `valid-sync-token`) triggers a full resync that also removes members no longer listed.
//!
//! Item key: the event's absolute href path (`<calendar path><resource>`), version: ETag,
//! content: `text/calendar` bytes.
//!
//! ## Cursor (JSON)
//! `{"v":1,"calendars":{"/dav/cal/work/":{"sync_token":"...","ctag":"..."}}}`

mod xml;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::{
    AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::secrets::password_account;
use nugget_core::{Error, Result};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::xml::{CALDAV, CS, DAV, DavResponse, MultiStatus};

/// Connector kind.
pub const KIND: &str = "caldav";
/// Egress-ledger feature for every CalDAV request.
pub const FEATURE: &str = "sync:caldav";
/// Egress-ledger feature for events created on the user's behalf ([`CalDavConnector::put_event`]).
pub const ACTION_FEATURE: &str = "action:caldav";

/// Cap on one response body.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// Events per `calendar-multiget`.
const MULTIGET_BATCH: usize = 50;
/// Results asked for per `sync-collection` page (servers may return fewer and truncate).
const SYNC_PAGE: usize = 500;
/// Guard against a server that truncates forever.
const MAX_SYNC_PAGES: usize = 10_000;

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

/// Settings of a `caldav` source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalDavSettings {
    /// Server root, principal, calendar home or calendar collection. `https://` (plain `http://`
    /// only for loopback hosts).
    pub url: String,
    pub username: String,
    /// Restrict to these calendars (display name, href, or last path segment). Default: all
    /// calendars holding events or tasks.
    #[serde(default)]
    pub calendars: Option<Vec<String>>,
}

impl CalDavSettings {
    pub fn parse(v: &Value) -> Result<Self> {
        let s: Self = serde_json::from_value(v.clone())
            .map_err(|e| Error::Invalid(format!("caldav settings: {e}")))?;
        s.base_url()?;
        if s.username.is_empty() || s.username.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "caldav settings: `username` is required".into(),
            ));
        }
        if let Some(c) = &s.calendars
            && (c.is_empty() || c.iter().any(|x| x.trim().is_empty()))
        {
            return Err(Error::Invalid(
                "caldav settings: `calendars` entries must be non-empty".into(),
            ));
        }
        Ok(s)
    }

    fn base_url(&self) -> Result<Url> {
        let mut u = Url::parse(self.url.trim())
            .map_err(|e| Error::Invalid(format!("caldav settings: bad `url`: {e}")))?;
        check_transport(&u)?;
        if !u.username().is_empty() || u.password().is_some() {
            return Err(Error::Invalid(
                "caldav settings: put credentials in `username` and the keychain, not the URL"
                    .into(),
            ));
        }
        u.set_fragment(None);
        u.set_query(None);
        if !u.path().ends_with('/') {
            let p = format!("{}/", u.path());
            u.set_path(&p);
        }
        Ok(u)
    }
}

/// https only, except plain http to a loopback host (local servers and tests).
fn check_transport(u: &Url) -> Result<()> {
    match u.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(u) => Ok(()),
        "http" => Err(Error::Invalid(
            "caldav: refusing plain http to a non-loopback host; use https".into(),
        )),
        s => Err(Error::Invalid(format!(
            "caldav: unsupported URL scheme `{s}`"
        ))),
    }
}

fn is_loopback(u: &Url) -> bool {
    match u.host_str() {
        Some("localhost") => true,
        Some(h) => h
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn settings_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["url", "username"],
        "properties": {
            "url": { "type": "string", "format": "uri", "title": "CalDAV URL",
                     "examples": ["https://caldav.fastmail.com/", "https://caldav.icloud.com/"],
                     "description": "Server, principal, calendar home or a single calendar (https)" },
            "username": { "type": "string" },
            "calendars": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1,
                           "description": "Only these calendars (display name or href). Default: all" }
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------------------------

/// Factory for `caldav` sources.
#[derive(Debug, Default, Clone, Copy)]
pub struct CalDavFactory;

impl CalDavFactory {
    pub fn new() -> Self {
        Self
    }
}

impl ConnectorFactory for CalDavFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "CalDAV calendar",
            auth: AuthKind::Password,
            settings_schema: settings_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        CalDavSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(CalDavConnector::new(
            CalDavSettings::parse(settings)?,
            ctx,
        )?))
    }
}

// ---------------------------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default)]
    calendars: BTreeMap<String, CalCursor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CalCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ctag: Option<String>,
}

impl Cursor {
    fn load(v: Option<Value>) -> Self {
        match v.map(serde_json::from_value::<Cursor>) {
            Some(Ok(c)) if c.v == 1 => c,
            Some(_) => {
                tracing::warn!("ignoring unreadable CalDAV cursor; resyncing");
                Self::fresh()
            }
            None => Self::fresh(),
        }
    }

    fn fresh() -> Self {
        Self {
            v: 1,
            calendars: BTreeMap::new(),
        }
    }

    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ---------------------------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Calendar {
    url: Url,
    /// Collection path, always ending in `/`. Prefix of every member key.
    path: String,
    name: Option<String>,
    ctag: Option<String>,
    sync_token: Option<String>,
    /// Accepts `VEVENT` components (or does not say).
    events: bool,
}

/// One configured CalDAV account.
pub struct CalDavConnector {
    settings: CalDavSettings,
    base: Url,
    ctx: ConnectorContext,
    http: MeteredHttp,
}

impl std::fmt::Debug for CalDavConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavConnector")
            .field("url", &self.base.as_str())
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

const PROPFIND_DISCOVERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/">
  <D:prop>
    <D:resourcetype/>
    <D:displayname/>
    <D:current-user-principal/>
    <C:calendar-home-set/>
    <C:supported-calendar-component-set/>
    <CS:getctag/>
    <D:sync-token/>
  </D:prop>
</D:propfind>"#;

const PROPFIND_ETAGS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:resourcetype/>
    <D:getetag/>
  </D:prop>
</D:propfind>"#;

fn sync_collection_body(token: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:sync-collection xmlns:D="DAV:">
  <D:sync-token>{}</D:sync-token>
  <D:sync-level>1</D:sync-level>
  <D:limit><D:nresults>{SYNC_PAGE}</D:nresults></D:limit>
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:sync-collection>"#,
        xml::escape(token)
    )
}

fn multiget_body(paths: &[String]) -> String {
    let hrefs: String = paths
        .iter()
        .map(|p| format!("  <D:href>{}</D:href>\n", xml::escape(p)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
{hrefs}</C:calendar-multiget>"#
    )
}

/// Providers known to spread one account over per-partition hosts under their own registrable
/// domain. Credentials may move between hosts of the same entry (https only).
const PARTITIONED_PROVIDERS: &[&str] = &["icloud.com"];

/// The [`PARTITIONED_PROVIDERS`] entry `host` is a subdomain of, if any.
fn partitioned_provider(host: &str) -> Option<&'static str> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    PARTITIONED_PROVIDERS.iter().copied().find(|d| {
        host.strip_suffix(d)
            .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1)
    })
}

/// Outcome of a `sync-collection` attempt.
enum SyncResult {
    Done(CalCursor),
    /// The server does not do `sync-collection` here; use the ETag listing.
    Unsupported,
}

/// Known items of one calendar: key → version.
type KnownMap = HashMap<String, Option<String>>;

impl CalDavConnector {
    pub fn new(settings: CalDavSettings, ctx: ConnectorContext) -> Result<Self> {
        let base = settings.base_url()?;
        let http = MeteredHttp::new(ctx.egress.clone(), FEATURE)?.with_max_body(MAX_BODY);
        Ok(Self {
            settings,
            base,
            ctx,
            http,
        })
    }

    fn password(&self) -> Result<String> {
        let account = password_account(self.ctx.source.as_str());
        match self.ctx.secrets.get(&account)? {
            Some(p) if !p.is_empty() => Ok(p),
            _ => Err(Error::Unauthorized(format!(
                "no CalDAV password stored for source `{}`",
                self.ctx.source
            ))),
        }
    }

    /// Hosts we send credentials to: the configured one, or (https on 443 only) another host of
    /// the same known partitioned provider ([`PARTITIONED_PROVIDERS`], e.g. iCloud sends
    /// clients from `caldav.icloud.com` to `pNN-caldav.icloud.com`). A generic "sibling under
    /// the parent domain" rule is not safe: under a public suffix such as `duckdns.org`, every
    /// sibling belongs to someone else.
    fn check_url(&self, u: &Url) -> Result<()> {
        check_transport(u)?;
        if !u.username().is_empty() || u.password().is_some() {
            return Err(Error::Invalid("caldav: credentials in a server URL".into()));
        }
        let (Some(h), Some(bh)) = (u.host_str(), self.base.host_str()) else {
            return Err(Error::Invalid("caldav: URL without a host".into()));
        };
        let same = h.eq_ignore_ascii_case(bh)
            && u.port_or_known_default() == self.base.port_or_known_default()
            && u.scheme() == self.base.scheme();
        let sibling = u.scheme() == "https"
            && self.base.scheme() == "https"
            && u.port_or_known_default() == Some(443)
            && self.base.port_or_known_default() == Some(443)
            && bh.parse::<IpAddr>().is_err()
            && partitioned_provider(bh).is_some_and(|p| partitioned_provider(h) == Some(p));
        if same || sibling {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "caldav: refusing to send credentials to `{h}` (configured host is `{bh}`)"
            )))
        }
    }

    fn resolve(&self, base: &Url, href: &str) -> Result<Url> {
        let mut u = base
            .join(href.trim())
            .map_err(|e| Error::Invalid(format!("caldav: bad href `{href}`: {e}")))?;
        u.set_fragment(None);
        self.check_url(&u)?;
        Ok(u)
    }

    async fn send(
        &self,
        password: &str,
        method: &'static str,
        url: &Url,
        depth: Option<&'static str>,
        body: String,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        self.check_url(url)?;
        let m = Method::from_bytes(method.as_bytes())
            .map_err(|e| Error::Invalid(format!("http method: {e}")))?;
        let mut req = self
            .http
            .request(m, url.as_str())
            .basic_auth(&self.settings.username, Some(password))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(body);
        if let Some(d) = depth {
            req = req.header("Depth", d);
        }
        let resp = tokio::select! {
            r = self.http.send(req, &[]) => r?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        if resp.status.is_redirection() {
            let to = resp
                .headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?");
            return Err(Error::Network(format!(
                "CalDAV server redirected {method} {url} to {to}; redirects are not followed \
                 (update the source URL)"
            )));
        }
        Ok(resp)
    }

    async fn propfind(
        &self,
        password: &str,
        url: &Url,
        depth: &'static str,
        body: &str,
        cancel: &CancellationToken,
    ) -> Result<MultiStatus> {
        let resp = self
            .send(password, "PROPFIND", url, Some(depth), body.into(), cancel)
            .await?
            .error_for_status()?;
        xml::multistatus(&resp.body)
    }

    /// The response describing `url` itself (falls back to the first one).
    fn find_self<'a>(&self, ms: &'a MultiStatus, url: &Url) -> Option<&'a DavResponse> {
        let want = url.path().trim_end_matches('/');
        ms.responses
            .iter()
            .find(|r| {
                self.resolve(url, &r.href)
                    .is_ok_and(|u| u.path().trim_end_matches('/') == want)
            })
            .or_else(|| ms.responses.first())
    }

    fn calendar_from(&self, r: &DavResponse, url: Url) -> Calendar {
        let mut url = url;
        if !url.path().ends_with('/') {
            let p = format!("{}/", url.path());
            url.set_path(&p);
        }
        Calendar {
            path: url.path().to_string(),
            url,
            name: r.prop_text(DAV, "displayname"),
            ctag: r.prop_text(CS, "getctag"),
            sync_token: r.prop_text(DAV, "sync-token"),
            events: Self::holds_events(r),
        }
    }

    fn holds_events(r: &DavResponse) -> bool {
        let Some(set) = r.prop(CALDAV, "supported-calendar-component-set") else {
            return true;
        };
        let mut comps = set.children(CALDAV, "comp").peekable();
        comps.peek().is_none()
            || comps.any(|c| {
                c.attr("name")
                    .is_some_and(|n| n.eq_ignore_ascii_case("VEVENT"))
            })
    }

    /// Create an event: `PUT` the iCalendar object `ics` as `<resource>` (e.g. `<uuid>.ics`;
    /// letters, digits, `-`, `_` and `.` only) into the user's default calendar, the first
    /// discovered calendar (within `calendars`, if set) that accepts events. Never overwrites
    /// (`If-None-Match: *`). Recorded in the egress ledger as [`ACTION_FEATURE`]. Returns the new
    /// event's URL.
    pub async fn put_event(
        &self,
        resource: &str,
        ics: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let ok = !resource.is_empty()
            && resource.len() <= 200
            && !resource.starts_with('.')
            && resource
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !ok {
            return Err(Error::Invalid(format!(
                "caldav: bad event resource name `{resource}`"
            )));
        }
        let password = self.password()?;
        let calendars = self.discover(&password, cancel).await?;
        let cal = calendars
            .iter()
            .find(|c| c.events)
            .ok_or_else(|| Error::NotFound("CalDAV: no calendar accepts events".into()))?;
        let url = self.resolve(&cal.url, resource)?;
        let http =
            MeteredHttp::new(self.ctx.egress.clone(), ACTION_FEATURE)?.with_max_body(1024 * 1024);
        let req = http
            .request(Method::PUT, url.as_str())
            .basic_auth(&self.settings.username, Some(&password))
            .header(
                reqwest::header::CONTENT_TYPE,
                "text/calendar; charset=utf-8",
            )
            .header(reqwest::header::IF_NONE_MATCH, "*")
            .body(ics.to_owned());
        let resp = tokio::select! {
            r = http.send(req, &[]) => r?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        if resp.status.as_u16() == 412 {
            return Err(Error::Invalid(format!("CalDAV: {url} already exists")));
        }
        resp.error_for_status()?;
        Ok(url.to_string())
    }

    fn holds_items(r: &DavResponse) -> bool {
        let Some(set) = r.prop(CALDAV, "supported-calendar-component-set") else {
            return true;
        };
        let comps: Vec<String> = set
            .children(CALDAV, "comp")
            .filter_map(|c| c.attr("name").map(str::to_ascii_uppercase))
            .collect();
        comps.is_empty()
            || comps
                .iter()
                .any(|c| matches!(c.as_str(), "VEVENT" | "VTODO" | "VJOURNAL"))
    }

    async fn discover(&self, password: &str, cancel: &CancellationToken) -> Result<Vec<Calendar>> {
        let base = &self.base;
        let ms = self
            .propfind(password, base, "0", PROPFIND_DISCOVERY, cancel)
            .await?;
        let me = self
            .find_self(&ms, base)
            .ok_or_else(|| Error::NotFound(format!("CalDAV: nothing at {base}")))?;
        if me.is_calendar() {
            return Ok(vec![self.calendar_from(me, base.clone())]);
        }
        let home = if let Some(h) = me.prop_href(CALDAV, "calendar-home-set") {
            self.resolve(base, &h)?
        } else if let Some(p) = me.prop_href(DAV, "current-user-principal") {
            let principal = self.resolve(base, &p)?;
            let ms = self
                .propfind(password, &principal, "0", PROPFIND_DISCOVERY, cancel)
                .await?;
            let h = self
                .find_self(&ms, &principal)
                .and_then(|r| r.prop_href(CALDAV, "calendar-home-set"))
                .ok_or_else(|| {
                    Error::NotFound("CalDAV principal has no calendar-home-set".into())
                })?;
            self.resolve(&principal, &h)?
        } else {
            base.clone()
        };
        let ms = self
            .propfind(password, &home, "1", PROPFIND_DISCOVERY, cancel)
            .await?;
        let mut cals = Vec::new();
        for r in &ms.responses {
            if !r.is_calendar() || !Self::holds_items(r) {
                continue;
            }
            let url = self.resolve(&home, &r.href)?;
            cals.push(self.calendar_from(r, url));
        }
        if let Some(wanted) = &self.settings.calendars {
            cals.retain(|c| {
                wanted.iter().any(|w| {
                    let w = w.trim();
                    c.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(w))
                        || c.path == w
                        || c.url.as_str() == w
                        || c.path.trim_end_matches('/').rsplit('/').next() == Some(w)
                })
            });
        }
        cals.sort_by(|a, b| a.path.cmp(&b.path));
        cals.dedup_by(|a, b| a.path == b.path);
        Ok(cals)
    }

    async fn run_sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let password = self.password()?;
        let mut cursor = Cursor::load(cursor);
        let calendars = self.discover(&password, cancel).await?;
        if calendars.is_empty() {
            // Never treat an empty discovery as "everything was deleted".
            return Err(Error::NotFound(
                "CalDAV: no calendars found (check the URL and `calendars`)".into(),
            ));
        }

        // Group known items under their calendar.
        let mut known: HashMap<String, KnownMap> = HashMap::new();
        let mut orphans = Vec::new();
        for (key, version) in self.ctx.known.versions().await? {
            match calendars.iter().find(|c| key.as_str().starts_with(&c.path)) {
                Some(c) => {
                    known
                        .entry(c.path.clone())
                        .or_default()
                        .insert(key.0, version);
                }
                None => orphans.push(key),
            }
        }
        // Items of calendars that vanished or were deselected.
        for key in orphans {
            sink.send(Change::Delete(key)).await?;
        }
        cursor
            .calendars
            .retain(|p, _| calendars.iter().any(|c| &c.path == p));

        for cal in &calendars {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut k = known.remove(&cal.path).unwrap_or_default();
            let prev = cursor.calendars.get(&cal.path).cloned().unwrap_or_default();
            let mut next = None;
            if cal.sync_token.is_some() {
                match self
                    .sync_collection(&password, cal, &prev, &mut k, &mut cursor, sink, cancel)
                    .await?
                {
                    SyncResult::Done(c) => next = Some(c),
                    SyncResult::Unsupported => {
                        tracing::info!(calendar = %cal.path, "sync-collection unsupported; using ETags");
                    }
                }
            }
            let next = match next {
                Some(c) => c,
                None => {
                    self.etag_sync(&password, cal, &prev, &mut k, sink, cancel)
                        .await?
                }
            };
            cursor.calendars.insert(cal.path.clone(), next);
            sink.checkpoint(cursor.to_value()).await?;
        }
        Ok(cursor.to_value())
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_collection(
        &self,
        password: &str,
        cal: &Calendar,
        prev: &CalCursor,
        known: &mut KnownMap,
        cursor: &mut Cursor,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<SyncResult> {
        let mut token = prev.sync_token.clone().unwrap_or_default();
        let mut full = token.is_empty();
        let mut reset = false;
        let mut seen: HashSet<String> = HashSet::new();
        for _ in 0..MAX_SYNC_PAGES {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let resp = self
                .send(
                    password,
                    "REPORT",
                    &cal.url,
                    Some("0"),
                    sync_collection_body(&token),
                    cancel,
                )
                .await?;
            let status = resp.status.as_u16();
            if matches!(status, 403 | 409)
                && String::from_utf8_lossy(&resp.body).contains("valid-sync-token")
            {
                if token.is_empty() || reset {
                    return Err(Error::Network(
                        "CalDAV server rejected a fresh sync-collection".into(),
                    ));
                }
                tracing::info!(calendar = %cal.path, "sync-token no longer valid; full resync");
                token.clear();
                full = true;
                reset = true;
                seen.clear();
                continue;
            }
            if matches!(status, 400 | 405 | 501) && seen.is_empty() {
                return Ok(SyncResult::Unsupported);
            }
            let resp = resp.error_for_status()?;
            let ms = xml::multistatus(&resp.body)?;

            let mut truncated = false;
            let mut fetch = Vec::new();
            for r in &ms.responses {
                let url = self.resolve(&cal.url, &r.href)?;
                let key = url.path().to_string();
                if key.trim_end_matches('/') == cal.path.trim_end_matches('/') {
                    truncated |= r.status == Some(507);
                    continue;
                }
                if !key.starts_with(&cal.path) {
                    continue;
                }
                let deleted = r.status == Some(404)
                    || (r.status.is_none()
                        && !r.propstats.is_empty()
                        && r.propstats.iter().all(|(s, _)| *s == 404));
                if deleted {
                    if known.remove(&key).is_some() {
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                    continue;
                }
                if r.is_collection() {
                    continue;
                }
                seen.insert(key.clone());
                let etag = r.prop_text(DAV, "getetag");
                if etag.is_some() && known.get(&key) == Some(&etag) {
                    continue;
                }
                fetch.push(key);
            }
            self.multiget(password, cal, &fetch, known, sink, cancel)
                .await?;
            token = ms.sync_token.clone().ok_or_else(|| {
                Error::Network("sync-collection response without a sync-token".into())
            })?;
            let entry = CalCursor {
                sync_token: Some(token.clone()),
                ctag: cal.ctag.clone(),
            };
            if !full {
                // Resumable: everything up to this token is applied once the checkpoint commits.
                cursor.calendars.insert(cal.path.clone(), entry.clone());
                sink.checkpoint(cursor.to_value()).await?;
            }
            if !truncated {
                if full {
                    let stale: Vec<String> = known
                        .keys()
                        .filter(|k| !seen.contains(*k))
                        .cloned()
                        .collect();
                    for k in stale {
                        known.remove(&k);
                        sink.send(Change::Delete(ItemKey::new(k))).await?;
                    }
                }
                return Ok(SyncResult::Done(entry));
            }
        }
        Err(Error::Network(
            "CalDAV sync-collection never finished paging".into(),
        ))
    }

    /// Fallback without `sync-collection`: compare `getctag`, then list ETags.
    async fn etag_sync(
        &self,
        password: &str,
        cal: &Calendar,
        prev: &CalCursor,
        known: &mut KnownMap,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<CalCursor> {
        if cal.ctag.is_some() && prev.ctag == cal.ctag && prev.sync_token.is_none() {
            return Ok(prev.clone());
        }
        let ms = self
            .propfind(password, &cal.url, "1", PROPFIND_ETAGS, cancel)
            .await?;
        let mut listing: HashMap<String, Option<String>> = HashMap::new();
        for r in &ms.responses {
            let url = self.resolve(&cal.url, &r.href)?;
            let key = url.path().to_string();
            if !key.starts_with(&cal.path)
                || key.trim_end_matches('/') == cal.path.trim_end_matches('/')
                || r.is_collection()
                || r.ok_props().next().is_none()
            {
                continue;
            }
            listing.insert(key, r.prop_text(DAV, "getetag"));
        }
        let gone: Vec<String> = known
            .keys()
            .filter(|k| !listing.contains_key(*k))
            .cloned()
            .collect();
        for k in gone {
            known.remove(&k);
            sink.send(Change::Delete(ItemKey::new(k))).await?;
        }
        let mut fetch: Vec<String> = listing
            .iter()
            .filter(|(k, etag)| etag.is_none() || known.get(*k) != Some(*etag))
            .map(|(k, _)| k.clone())
            .collect();
        fetch.sort();
        self.multiget(password, cal, &fetch, known, sink, cancel)
            .await?;
        Ok(CalCursor {
            sync_token: None,
            ctag: cal.ctag.clone(),
        })
    }

    async fn multiget(
        &self,
        password: &str,
        cal: &Calendar,
        keys: &[String],
        known: &mut KnownMap,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        for chunk in keys.chunks(MULTIGET_BATCH) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let wanted: HashSet<&str> = chunk.iter().map(String::as_str).collect();
            let resp = self
                .send(
                    password,
                    "REPORT",
                    &cal.url,
                    Some("1"),
                    multiget_body(chunk),
                    cancel,
                )
                .await?
                .error_for_status()?;
            let ms = xml::multistatus(&resp.body)?;
            for r in &ms.responses {
                let url = self.resolve(&cal.url, &r.href)?;
                let key = url.path().to_string();
                if !wanted.contains(key.as_str()) {
                    continue;
                }
                let Some(data) = r.prop(CALDAV, "calendar-data").map(|e| e.text.clone()) else {
                    if r.status == Some(404) && known.remove(&key).is_some() {
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                    continue;
                };
                if data.trim().is_empty() {
                    continue;
                }
                // XML parsing normalizes line ends to LF; iCalendar wants CRLF.
                let data = data.replace("\r\n", "\n").replace('\n', "\r\n");
                let etag = r.prop_text(DAV, "getetag");
                let mut doc = Document::new(
                    key.clone(),
                    "text/calendar",
                    Content::Bytes(data.into_bytes()),
                );
                doc.version = etag.clone();
                doc.url = Some(url.to_string());
                doc.metadata = json!({
                    "calendar": cal.name,
                    "calendar_href": cal.path,
                    "href": key,
                    "etag": etag,
                });
                sink.send(Change::Upsert(doc)).await?;
                known.insert(key, etag);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Connector for CalDavConnector {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.run_sync(cursor, sink, cancel).await
    }
}

#[cfg(test)]
mod tests;
