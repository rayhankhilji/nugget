//! Gmail and Google Calendar sync through Composio tool calls into `items` and `chunks`.
//!
//! The tool output shapes are not documented by Composio (only `data`), so parsing is lenient:
//! several field spellings are accepted, and anything unparseable is skipped, never invented.

use std::sync::Arc;

use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::composio::Composio;
use crate::db::{Connection, Db, NewItem, SyncRun, Upsert};
use crate::gateway::Gateway;
use crate::plans::{Catalog, Plan};

pub const GMAIL: &str = "gmail";
pub const CALENDAR: &str = "googlecalendar";
/// Toolkits this build can ingest.
pub const SUPPORTED: &[&str] = &[GMAIL, CALENDAR];

const MAX_BODY: usize = 64 * 1024;
const CHUNK_CHARS: usize = 1200;
const CHUNK_OVERLAP: usize = 200;
const MAX_PAGES: usize = 5;
/// Longest first-sync backfill, whatever the plan's history (keeps the first sync bounded).
const MAX_BACKFILL_DAYS: u32 = 3 * 365;
/// How soon to continue a backfill that did not finish in one run.
const CONTINUE_SECS: i64 = 60;
const IDLE_SECS: i64 = 24 * 60 * 60;
/// How long one sync run may hold its connection before another may start (a crashed run's
/// lease simply expires).
const RUN_LEASE_SECS: i64 = 15 * 60;
/// Toolkits that get the plan's full cadence; others sync at most every 15 minutes, which keeps
/// Composio tool-call cost bounded for users with many apps.
const PRIMARY: &[&str] = &[GMAIL, CALENDAR];
const SECONDARY_MIN_SECS: i64 = 15 * 60;

/// Seconds until the next sync: the plan's cadence for mail and calendar while the user is
/// active, at most every 15 minutes for other apps, daily when idle.
pub fn next_interval(active: bool, toolkit: &str, policy: SyncPolicy) -> i64 {
    match (active, PRIMARY.contains(&toolkit)) {
        (false, _) => IDLE_SECS,
        (true, true) => policy.interval_secs,
        (true, false) => policy.interval_secs.max(SECONDARY_MIN_SECS),
    }
}

/// What a plan means for syncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPolicy {
    pub interval_secs: i64,
    pub backfill_days: u32,
    pub history_days: Option<u32>,
}

impl SyncPolicy {
    pub fn for_plan(plan: Option<&Plan>) -> Self {
        match plan {
            Some(p) => Self {
                interval_secs: i64::from(p.sync_interval_minutes) * 60,
                backfill_days: p
                    .history_days
                    .unwrap_or(MAX_BACKFILL_DAYS)
                    .min(MAX_BACKFILL_DAYS),
                history_days: p.history_days,
            },
            // No live subscription: the scheduler skips the user; a stray run stays minimal.
            None => Self {
                interval_secs: IDLE_SECS,
                backfill_days: 30,
                history_days: Some(30),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct SyncReport {
    pub toolkit: String,
    pub added: u32,
    pub updated: u32,
    pub error: Option<String>,
    /// Another sync of this connection was already running; this one did nothing.
    pub in_progress: bool,
}

// ---- text ---------------------------------------------------------------------------------------

/// Plain text from HTML: drops tags, `<script>`/`<style>` content and decodes common entities.
pub fn strip_html(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            for skip in ["script", "style"] {
                if lower[i + 1..].starts_with(skip) {
                    let close = format!("</{skip}");
                    i = lower[i..].find(&close).map_or(bytes.len(), |p| i + p);
                    break;
                }
            }
            let tag_end = html[i..].find('>').map_or(bytes.len(), |p| i + p + 1);
            let tag = &lower[i..tag_end.min(lower.len())];
            if ["<br", "<p", "</p", "<div", "</div", "<li", "<tr", "<h"]
                .iter()
                .any(|t| tag.starts_with(t))
            {
                out.push('\n');
            }
            i = tag_end;
        } else {
            let next = html[i..].find('<').map_or(bytes.len(), |p| i + p);
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    normalise_ws(&decoded)
}

/// Collapses runs of blank lines and trailing spaces.
fn normalise_ws(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_owned()
}

/// Drops quoted replies: `>` lines and everything after an "On … wrote:" line.
pub fn trim_quoted(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("On ") && t.ends_with("wrote:") {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_owned()
}

fn cap(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Splits text into chunks of about `size` characters with `overlap`, breaking at whitespace
/// where possible. Always on char boundaries.
pub fn chunk(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + size).min(chars.len());
        if end < chars.len()
            && let Some(ws) = (start + size / 2..end)
                .rev()
                .find(|&i| chars[i].is_whitespace())
        {
            end = ws;
        }
        let piece: String = chars[start..end].iter().collect();
        if !piece.trim().is_empty() {
            out.push(piece.trim().to_owned());
        }
        if end >= chars.len() {
            break;
        }
        start = (end.saturating_sub(overlap)).max(start + 1);
    }
    out
}

pub fn content_hash(item: &NewItem) -> String {
    let mut h = Sha256::new();
    for part in [
        item.title.as_deref().unwrap_or(""),
        item.author.as_deref().unwrap_or(""),
        &item.body,
    ] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    h.update(
        item.occurred_at
            .map(|t| t.unix_timestamp())
            .unwrap_or(0)
            .to_be_bytes(),
    );
    h.update(item.metadata.to_string().as_bytes());
    format!("{:x}", h.finalize())
}

/// What gets embedded and searched: a header line plus the body.
pub fn indexed_text(item: &NewItem) -> String {
    let mut head = Vec::new();
    if let Some(t) = &item.title {
        head.push(t.clone());
    }
    if let Some(a) = &item.author {
        head.push(format!("from {a}"));
    }
    format!("{}\n{}", head.join(" — "), item.body)
        .trim()
        .to_owned()
}

// ---- parsing --------------------------------------------------------------------------------------

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.pointer(k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The list under the first of `keys` found at the top or under `data`/`response_data`.
fn find_list<'a>(data: &'a Value, keys: &[&str]) -> &'a [Value] {
    for root in [data, &data["data"], &data["response_data"]] {
        for k in keys {
            if let Some(a) = root[*k].as_array() {
                return a;
            }
        }
    }
    &[]
}

fn next_token(data: &Value) -> Option<String> {
    [data, &data["data"], &data["response_data"]]
        .into_iter()
        .find_map(|r| first_str(r, &["/nextPageToken", "/next_page_token"]))
}

fn parse_time(v: Option<&Value>) -> Option<OffsetDateTime> {
    match v? {
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339).ok().or_else(|| {
            // Gmail internalDate-style millisecond strings.
            s.parse::<i64>()
                .ok()
                .and_then(|ms| OffsetDateTime::from_unix_timestamp(ms / 1000).ok())
        }),
        Value::Number(n) => {
            let x = n.as_i64()?;
            let secs = if x > 100_000_000_000 { x / 1000 } else { x };
            OffsetDateTime::from_unix_timestamp(secs).ok()
        }
        _ => None,
    }
}

fn header(m: &Value, name: &str) -> Option<String> {
    m.pointer("/payload/headers")?
        .as_array()?
        .iter()
        .find(|h| {
            h["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .and_then(|h| h["value"].as_str())
        .map(str::to_owned)
}

/// Text from a Gmail API payload: prefers `text/plain`, falls back to stripped `text/html`.
fn payload_text(p: &Value) -> Option<String> {
    let decode = |d: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(d.trim_end_matches('='))
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    };
    let mut plain = None;
    let mut html = None;
    let mut stack = vec![p];
    while let Some(part) = stack.pop() {
        let mime = part["mimeType"].as_str().unwrap_or("");
        if let Some(d) = part.pointer("/body/data").and_then(Value::as_str) {
            if mime == "text/plain" && plain.is_none() {
                plain = decode(d);
            } else if mime == "text/html" && html.is_none() {
                html = decode(d).map(|h| strip_html(&h));
            }
        }
        stack.extend(part["parts"].as_array().into_iter().flatten());
    }
    plain.or(html)
}

pub fn parse_gmail(data: &Value) -> (Vec<NewItem>, Option<String>) {
    let items = find_list(data, &["messages", "emails"])
        .iter()
        .filter_map(|m| {
            let id = first_str(m, &["/messageId", "/id", "/message_id"])?;
            let body = first_str(m, &["/messageText", "/message_text", "/body", "/text"])
                .map(|b| if b.contains("</") { strip_html(&b) } else { b })
                .or_else(|| payload_text(&m["payload"]))
                .or_else(|| first_str(m, &["/preview/body", "/snippet", "/preview"]))
                .unwrap_or_default();
            let thread = first_str(m, &["/threadId", "/thread_id"]);
            let labels = m["labelIds"].clone();
            Some(NewItem {
                source: GMAIL.into(),
                url: Some(format!(
                    "https://mail.google.com/mail/u/0/#all/{}",
                    thread.as_deref().unwrap_or(&id)
                )),
                external_id: id,
                kind: "email",
                title: first_str(m, &["/subject"]).or_else(|| header(m, "Subject")),
                author: first_str(m, &["/sender", "/from"]).or_else(|| header(m, "From")),
                occurred_at: parse_time(
                    m.get("messageTimestamp")
                        .or(m.get("internalDate"))
                        .or(m.get("date")),
                ),
                body: cap(trim_quoted(&body), MAX_BODY),
                metadata: json!({
                    "thread_id": thread,
                    "to": first_str(m, &["/to"]).or_else(|| header(m, "To")),
                    "labels": if labels.is_array() { labels } else { json!([]) },
                }),
            })
        })
        .collect();
    (items, next_token(data))
}

pub fn parse_calendar(data: &Value) -> (Vec<NewItem>, Option<String>) {
    let items = find_list(data, &["items", "events", "event_data"])
        .iter()
        .filter(|e| e["status"].as_str() != Some("cancelled"))
        .filter_map(|e| {
            let id = first_str(e, &["/id"])?;
            let start = e.pointer("/start/dateTime").or_else(|| e.pointer("/start/date"));
            let start_s = start.and_then(Value::as_str).unwrap_or("");
            let end_s = first_str(e, &["/end/dateTime", "/end/date"]).unwrap_or_default();
            let attendees: Vec<String> = e["attendees"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| first_str(a, &["/displayName", "/email"]))
                .collect();
            let location = first_str(e, &["/location"]);
            let desc = first_str(e, &["/description"]).map(|d| strip_html(&d)).unwrap_or_default();
            let mut body = format!("When: {start_s} – {end_s}");
            if let Some(l) = &location {
                body.push_str(&format!("\nWhere: {l}"));
            }
            if !attendees.is_empty() {
                body.push_str(&format!("\nAttendees: {}", attendees.join(", ")));
            }
            if !desc.is_empty() {
                body.push_str(&format!("\n\n{desc}"));
            }
            // All-day events carry a date only.
            let occurred = parse_time(start).or_else(|| {
                let mut p = start_s.splitn(3, '-').map(str::parse::<i32>);
                let (y, m, d) = (p.next()?.ok()?, p.next()?.ok()?, p.next()?.ok()?);
                let month = time::Month::try_from(u8::try_from(m).ok()?).ok()?;
                let date = time::Date::from_calendar_date(y, month, u8::try_from(d).ok()?).ok()?;
                Some(date.midnight().assume_utc())
            });
            Some(NewItem {
                source: CALENDAR.into(),
                external_id: id,
                kind: "event",
                title: first_str(e, &["/summary"]),
                author: first_str(e, &["/organizer/displayName", "/organizer/email"]),
                url: first_str(e, &["/htmlLink"]),
                occurred_at: occurred,
                body: cap(body, MAX_BODY),
                metadata: json!({"start": start_s, "end": end_s, "location": location, "attendees": attendees}),
            })
        })
        .collect();
    (items, next_token(data))
}

// ---- sync -----------------------------------------------------------------------------------------

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// Tool name and arguments for one page, given the stored cursor.
pub fn page_request(
    toolkit: &str,
    cursor: &Value,
    page: Option<&str>,
    now: OffsetDateTime,
    backfill_days: u32,
) -> Option<(&'static str, Value)> {
    match toolkit {
        GMAIL => {
            let after = gmail_after(cursor, now, backfill_days);
            let mut args = json!({"query": format!("after:{after}"), "max_results": 50, "verbose": true, "include_payload": true});
            if let Some(p) = page {
                args["page_token"] = json!(p);
            }
            Some(("GMAIL_FETCH_EMAILS", args))
        }
        CALENDAR => {
            let mut args =
                json!({"calendarId": "primary", "singleEvents": true, "maxResults": 250});
            match cursor["updated_min"].as_str() {
                Some(u) => args["updatedMin"] = json!(u),
                None => {
                    args["timeMin"] = json!(rfc3339(now - Duration::days(30)));
                    args["timeMax"] = json!(rfc3339(now + Duration::days(60)));
                }
            }
            if let Some(p) = page {
                args["pageToken"] = json!(p);
            }
            Some(("GOOGLECALENDAR_EVENTS_LIST", args))
        }
        _ => None,
    }
}

/// Gmail's `after:` (epoch seconds): the cursor, or on the first sync the backfill window.
pub fn gmail_after(cursor: &Value, now: OffsetDateTime, backfill_days: u32) -> i64 {
    cursor["after"]
        .as_i64()
        .unwrap_or((now - Duration::days(i64::from(backfill_days))).unix_timestamp())
}

pub fn parse_page(toolkit: &str, data: &Value) -> (Vec<NewItem>, Option<String>) {
    if toolkit == GMAIL {
        parse_gmail(data)
    } else {
        parse_calendar(data)
    }
}

/// The cursor after a sync run that started at `started`. `pending` is the next page when the
/// run stopped before the end (a long Gmail backfill): the same query resumes there, and the
/// newest timestamp seen so far is carried until the backfill completes.
pub fn next_cursor(
    toolkit: &str,
    newest: Option<OffsetDateTime>,
    old: &Value,
    started: OffsetDateTime,
    backfill_days: u32,
    pending: Option<String>,
) -> Value {
    match toolkit {
        GMAIL => {
            let carried = old["newest"].as_i64();
            let newest = newest.map(|t| t.unix_timestamp()).max(carried);
            match pending {
                Some(page) => {
                    json!({"after": gmail_after(old, started, backfill_days), "page": page, "newest": newest})
                }
                // Re-read one hour back: mail can land with an older timestamp; unchanged items are skipped by hash.
                None => match newest {
                    Some(t) => json!({"after": t - 3600}),
                    None => json!({"after": gmail_after(old, started, backfill_days)}),
                },
            }
        }
        _ => json!({"updated_min": rfc3339(started - Duration::minutes(5))}),
    }
}

#[derive(Clone)]
pub struct Ingest {
    pub db: Db,
    pub composio: Arc<dyn Composio>,
    pub gateway: Gateway,
    pub catalog: Arc<Catalog>,
}

impl Ingest {
    /// Syncs one connection: fetches new pages, stores changed items, advances the cursor.
    pub async fn sync(&self, user: Uuid, conn: &Connection) -> SyncReport {
        let mut report = SyncReport {
            toolkit: conn.toolkit.clone(),
            ..Default::default()
        };
        // One run per connection at a time, whether started by a request or the scheduler.
        match self.db.claim_sync(user, conn.id, RUN_LEASE_SECS).await {
            Ok(true) => {}
            Ok(false) => {
                report.in_progress = true;
                return report;
            }
            Err(e) => {
                report.error = Some(e.to_string());
                return report;
            }
        }
        let started = OffsetDateTime::now_utc();
        let policy = SyncPolicy::for_plan(self.plan_of(user).await.as_ref());
        let result = self
            .sync_inner(user, conn, started, policy, &mut report)
            .await;
        let active = self.db.recently_active(user).await.unwrap_or(false);
        let mut next = next_interval(active, &conn.toolkit, policy);
        let (cursor, err, done) = match result {
            Ok((c, more)) => {
                if more {
                    next = CONTINUE_SECS;
                }
                (Some(c), None, !more)
            }
            Err(e) => (None, Some(e), false),
        };
        if let Some(days) = policy.history_days
            && let Err(e) = self.db.prune_history(user, days).await
        {
            tracing::warn!(%e, "could not prune history");
        }
        if let Err(e) = self
            .db
            .finish_sync(
                user,
                conn.id,
                &SyncRun {
                    cursor: cursor.as_ref(),
                    error: err.as_deref(),
                    next_in_secs: next,
                    items: report.added + report.updated,
                    backfill_done: done,
                },
            )
            .await
        {
            tracing::warn!(%e, "could not record sync result");
        }
        report.error = err;
        report
    }

    /// The user's live plan, if any.
    pub async fn plan_of(&self, user: Uuid) -> Option<Plan> {
        crate::plans::entitlement(&self.db, &self.catalog, user)
            .await
            .ok()?
            .map(|e| e.plan)
    }

    /// Returns the new cursor and whether more pages remain (continue soon).
    async fn sync_inner(
        &self,
        user: Uuid,
        conn: &Connection,
        started: OffsetDateTime,
        policy: SyncPolicy,
        report: &mut SyncReport,
    ) -> Result<(Value, bool), String> {
        let uid = user.to_string();
        let mut page: Option<String> = conn.sync_cursor["page"].as_str().map(str::to_owned);
        let mut newest: Option<OffsetDateTime> = None;
        let mut pending = None;
        for i in 0..MAX_PAGES {
            let Some((tool, args)) = page_request(
                &conn.toolkit,
                &conn.sync_cursor,
                page.as_deref(),
                started,
                policy.backfill_days,
            ) else {
                return Err(format!("toolkit {} is not supported yet", conn.toolkit));
            };
            let data = self
                .composio
                .execute(&uid, &conn.composio_account_id, tool, args)
                .await
                .map_err(|e| e.to_string())?;
            let _ = self
                .db
                .record_usage(user, crate::db::UsageKind::Tool, tool, 0, 0, 300)
                .await;
            let (items, next) = parse_page(&conn.toolkit, &data);
            newest = items
                .iter()
                .map(|i| i.occurred_at)
                .fold(newest, Option::max);
            self.store_page(user, conn.id, items, report).await?;
            match next {
                Some(n) if i + 1 == MAX_PAGES => pending = Some(n),
                Some(n) => page = Some(n),
                None => break,
            }
        }
        // Only Gmail resumes mid-query; other toolkits restart from their cursor.
        let pending = pending.filter(|_| conn.toolkit == GMAIL);
        let more = pending.is_some();
        let cursor = next_cursor(
            &conn.toolkit,
            newest,
            &conn.sync_cursor,
            started,
            policy.backfill_days,
            pending,
        );
        Ok((cursor, more))
    }

    /// Stores a page of items: skips unchanged ones (one hash lookup for the page), embeds all
    /// new chunks in batched calls, then writes each item with its chunks.
    async fn store_page(
        &self,
        user: Uuid,
        conn: Uuid,
        items: Vec<NewItem>,
        report: &mut SyncReport,
    ) -> Result<(), String> {
        let Some(source) = items.first().map(|i| i.source.clone()) else {
            return Ok(());
        };
        let ids: Vec<String> = items.iter().map(|i| i.external_id.clone()).collect();
        let stored = self
            .db
            .item_hashes(user, &source, &ids)
            .await
            .map_err(|e| e.to_string())?;
        let changed: Vec<(NewItem, String, Vec<String>)> = items
            .into_iter()
            .filter_map(|item| {
                let hash = content_hash(&item);
                if stored.get(&item.external_id) == Some(&hash) {
                    return None;
                }
                let texts = chunk(&indexed_text(&item), CHUNK_CHARS, CHUNK_OVERLAP);
                Some((item, hash, texts))
            })
            .collect();
        let all: Vec<String> = changed
            .iter()
            .flat_map(|(_, _, t)| t.iter().cloned())
            .collect();
        let mut vectors = self
            .gateway
            .embed(&self.db, user, &all)
            .await
            .map_err(|e| e.to_string())?
            .map(Vec::into_iter);
        for (item, hash, texts) in changed {
            let chunks: Vec<(String, Option<Vec<f32>>)> = texts
                .into_iter()
                .map(|t| (t, vectors.as_mut().and_then(Iterator::next)))
                .collect();
            match self
                .db
                .upsert_item(user, Some(conn), &item, &hash, &chunks)
                .await
            {
                Ok(Upsert::Added) => report.added += 1,
                Ok(Upsert::Updated) => report.updated += 1,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    /// One pass of the background scheduler: claims up to `max` due connections (with a lease,
    /// so other machines skip them) and syncs them, `SYNC_CONCURRENCY` at a time.
    pub async fn run_due(&self, max: i32) -> usize {
        let due = match self.db.claim_due(max, SYNC_LEASE_SECS).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(%e, "claiming due connections failed");
                return 0;
            }
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(SYNC_CONCURRENCY));
        let mut tasks = tokio::task::JoinSet::new();
        for (user, conn_id) in due {
            let this = self.clone();
            let gate = gate.clone();
            tasks.spawn(async move {
                let _permit = gate.acquire_owned().await.ok()?;
                let conns = this.db.connections(user).await.ok()?;
                let c = conns.iter().find(|c| c.id == conn_id)?;
                let r = this.sync(user, c).await;
                crate::ops::METRICS.sync_run(r.error.is_none());
                tracing::info!(toolkit = %r.toolkit, added = r.added, updated = r.updated, failed = r.error.is_some(), "sync");
                Some(())
            });
        }
        let mut n = 0;
        while let Some(r) = tasks.join_next().await {
            if matches!(r, Ok(Some(()))) {
                n += 1;
            }
        }
        n
    }
}

/// How long a claimed connection is reserved for the claiming pass.
const SYNC_LEASE_SECS: i32 = 15 * 60;
/// Connections synced at once per machine (each sync is mostly waiting on Composio).
const SYNC_CONCURRENCY: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text() {
        let t = strip_html(
            "<style>p{}</style><p>Hi &amp; welcome</p><div>Line&nbsp;2</div><script>x()</script><br>end",
        );
        assert_eq!(t, "Hi & welcome\n\nLine 2\n\nend");
    }

    #[test]
    fn quoted_replies_dropped() {
        let t = trim_quoted(
            "Sounds good.\n> earlier\nThanks\nOn Mon, 1 Jan 2026, Bob <b@x> wrote:\nold stuff",
        );
        assert_eq!(t, "Sounds good.\nThanks");
    }

    #[test]
    fn chunks_overlap_and_respect_chars() {
        let text = "été ".repeat(1000);
        let c = chunk(&text, 100, 20);
        assert!(c.len() > 30);
        assert!(c.iter().all(|p| p.chars().count() <= 100));
        assert_eq!(chunk("short", 100, 20), vec!["short"]);
        assert!(chunk("   ", 100, 20).is_empty());
    }

    #[test]
    fn gmail_parse_composio_shape() {
        // Shape as returned by GMAIL_FETCH_EMAILS in Composio's examples (unverified live).
        let data = json!({"messages": [{
            "messageId": "m1", "threadId": "t1", "messageTimestamp": "2026-10-30T09:15:00Z",
            "sender": "Landlord <ll@example.com>", "to": "me@example.com", "subject": "Lease renewal",
            "messageText": "Your lease renews on 1 Dec.\n> quoted", "labelIds": ["INBOX"]
        }, {"no": "id"}], "nextPageToken": "p2"});
        let (items, next) = parse_gmail(&data);
        assert_eq!(next.as_deref(), Some("p2"));
        assert_eq!(items.len(), 1);
        let m = &items[0];
        assert_eq!(m.external_id, "m1");
        assert_eq!(m.title.as_deref(), Some("Lease renewal"));
        assert_eq!(m.body, "Your lease renews on 1 Dec.");
        assert_eq!(m.occurred_at.unwrap().unix_timestamp(), 1_793_351_700);
        assert!(m.url.as_deref().unwrap().ends_with("#all/t1"));
    }

    #[test]
    fn gmail_parse_api_payload_shape() {
        let b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("<p>Invoice &amp; receipt</p>");
        let data = json!({"data": {"messages": [{
            "id": "m2", "internalDate": "1761815700000",
            "payload": {"headers": [{"name": "Subject", "value": "Invoice"}, {"name": "From", "value": "Shop"}],
                        "parts": [{"mimeType": "text/html", "body": {"data": b64}}]}
        }]}});
        let (items, next) = parse_gmail(&data);
        assert_eq!(next, None);
        assert_eq!(items[0].title.as_deref(), Some("Invoice"));
        assert_eq!(items[0].author.as_deref(), Some("Shop"));
        assert_eq!(items[0].body, "Invoice & receipt");
        assert_eq!(
            items[0].occurred_at.unwrap().unix_timestamp(),
            1_761_815_700
        );
    }

    #[test]
    fn calendar_parse() {
        let data = json!({"items": [
            {"id": "e1", "summary": "Dentist", "start": {"dateTime": "2026-11-05T10:00:00+00:00"},
             "end": {"dateTime": "2026-11-05T10:30:00+00:00"}, "location": "High St",
             "attendees": [{"email": "a@x"}], "htmlLink": "https://cal/e1"},
            {"id": "e2", "summary": "Holiday", "start": {"date": "2026-12-24"}, "end": {"date": "2026-12-27"}},
            {"id": "e3", "status": "cancelled"}
        ], "nextPageToken": null});
        let (items, next) = parse_calendar(&data);
        assert_eq!(next, None);
        assert_eq!(items.len(), 2);
        assert!(items[0].body.contains("Where: High St"));
        assert!(items[0].body.contains("Attendees: a@x"));
        assert_eq!(
            items[1].occurred_at.unwrap().date().to_string(),
            "2026-12-24"
        );
    }

    #[test]
    fn requests_and_cursors() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let (tool, args) = page_request(GMAIL, &json!({}), None, now, 365).unwrap();
        assert_eq!(tool, "GMAIL_FETCH_EMAILS");
        assert_eq!(
            args["query"],
            format!("after:{}", 1_800_000_000 - 365 * 86_400)
        );
        let (_, args) = page_request(GMAIL, &json!({"after": 5}), Some("p"), now, 365).unwrap();
        assert_eq!(args["query"], "after:5");
        assert_eq!(args["page_token"], "p");
        let (tool, args) = page_request(CALENDAR, &json!({}), None, now, 365).unwrap();
        assert_eq!(tool, "GOOGLECALENDAR_EVENTS_LIST");
        assert!(args["timeMin"].is_string() && args.get("updatedMin").is_none());
        let c = next_cursor(CALENDAR, None, &json!({}), now, 365, None);
        let (_, args) = page_request(CALENDAR, &c, None, now, 365).unwrap();
        assert!(args["updatedMin"].is_string());
        assert_eq!(
            next_cursor(GMAIL, None, &json!({"after": 9}), now, 365, None),
            json!({"after": 9})
        );
        assert_eq!(
            next_cursor(GMAIL, Some(now), &json!({}), now, 365, None),
            json!({"after": 1_800_000_000 - 3600})
        );
        assert!(page_request("slack", &json!({}), None, now, 365).is_none());
    }

    #[test]
    fn gmail_backfill_resumes_then_advances() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let older = OffsetDateTime::from_unix_timestamp(1_799_000_000).unwrap();
        // Run 1 stops with pages left: same `after`, the page to resume, the newest seen.
        let c1 = next_cursor(GMAIL, Some(now), &json!({}), now, 30, Some("p6".into()));
        assert_eq!(
            c1,
            json!({"after": 1_800_000_000 - 30 * 86_400, "page": "p6", "newest": 1_800_000_000})
        );
        let (_, args) = page_request(GMAIL, &c1, c1["page"].as_str(), now, 30).unwrap();
        assert_eq!(args["page_token"], "p6");
        assert_eq!(
            args["query"],
            format!("after:{}", 1_800_000_000 - 30 * 86_400)
        );
        // Run 2 reaches the end with only older mail: the carried newest wins.
        let c2 = next_cursor(GMAIL, Some(older), &c1, now, 30, None);
        assert_eq!(c2, json!({"after": 1_800_000_000 - 3600}));
    }

    #[test]
    fn sync_policy_from_plan() {
        let c = Catalog::parse(crate::plans::DEFAULT_TOML, |_| None).unwrap();
        let heed = SyncPolicy::for_plan(c.get("heed"));
        assert_eq!(
            heed,
            SyncPolicy {
                interval_secs: 30 * 60,
                backfill_days: 365,
                history_days: Some(365)
            }
        );
        let max = SyncPolicy::for_plan(c.get("heed_max"));
        assert_eq!(
            max,
            SyncPolicy {
                interval_secs: 5 * 60,
                backfill_days: MAX_BACKFILL_DAYS,
                history_days: None
            }
        );
        assert_eq!(SyncPolicy::for_plan(None).interval_secs, IDLE_SECS);
        assert_eq!(next_interval(true, GMAIL, max), 5 * 60);
        assert_eq!(next_interval(true, "slack", max), 15 * 60);
        assert_eq!(next_interval(true, "slack", heed), 30 * 60);
        assert_eq!(next_interval(false, GMAIL, max), IDLE_SECS);
    }

    #[test]
    fn hash_changes_with_content() {
        let (items, _) =
            parse_gmail(&json!({"messages": [{"id": "a", "subject": "x", "messageText": "y"}]}));
        let mut b = items[0].clone();
        let h = content_hash(&items[0]);
        assert_eq!(h, content_hash(&b));
        b.body.push('!');
        assert_ne!(h, content_hash(&b));
    }
}
