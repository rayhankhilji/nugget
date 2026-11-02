//! Gmail, Google Calendar and Google Drive.
//!
//! - Gmail: `GMAIL_FETCH_EMAILS` with an `after:` query (M1). Trashed or spammed mail is
//!   deleted; a complete listing of the re-read window deletes what vanished.
//! - Calendar: `GOOGLECALENDAR_EVENTS_LIST` with `updatedMin` and `showDeleted`, so cancelled
//!   and deleted events come back with `status: cancelled` and are deleted here.
//! - Drive: `GOOGLEDRIVE_FIND_FILE` for the first backfill (`GOOGLEDRIVE_LIST_FILES` is
//!   deprecated), then the changes feed (`GOOGLEDRIVE_GET_CHANGES_START_PAGE_TOKEN`,
//!   `GOOGLEDRIVE_LIST_CHANGES`) for edits and removals. Google Docs text comes from
//!   `GOOGLEDRIVE_DOWNLOAD_FILE` with `mime_type: text/plain`; Composio documents the result
//!   only as `downloaded_file_content` without its inner shape, so inline text is used when
//!   present and the file is indexed by name and metadata otherwise. Other files are indexed by
//!   name and metadata only.

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use time::{Duration, OffsetDateTime};

use super::{
    MAX_BODY_CHARS, Pull, PullError, Rate, SyncAdapter, Tools, Window, cap, first_str, list,
    parse_time, rfc3339, top_str,
};
use crate::db::NewItem;
use crate::ingest;

pub const GMAIL: &str = ingest::GMAIL;
pub const CALENDAR: &str = ingest::CALENDAR;
pub const DRIVE: &str = "googledrive";

const MAX_PAGES: usize = 5;

pub struct Gmail;

#[async_trait]
impl SyncAdapter for Gmail {
    fn toolkit(&self) -> &'static str {
        GMAIL
    }
    fn tools(&self) -> &'static [&'static str] {
        &["GMAIL_FETCH_EMAILS"]
    }
    fn rate(&self) -> Rate {
        // Gmail allows 250 quota units per user per second; a list is 5 + 5 per message read.
        Rate {
            per_minute: 120,
            burst: 10,
        }
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let mut page: Option<String> = None;
        let mut newest = None;
        let mut seen = Vec::new();
        let mut complete = false;
        let mut after = None;
        for _ in 0..MAX_PAGES {
            let (tool, args) = ingest::page_request(GMAIL, cursor, page.as_deref(), now)
                .expect("gmail is supported");
            after = args["query"]
                .as_str()
                .and_then(|q| q.strip_prefix("after:"))
                .and_then(|s| s.parse::<i64>().ok());
            let data = tools.call(tool, args).await?;
            let (items, next) = ingest::parse_gmail(&data);
            for item in items {
                seen.push(item.external_id.clone());
                let labels = item.metadata["labels"].as_array();
                if labels.is_some_and(|l| l.iter().any(|x| x == "TRASH" || x == "SPAM")) {
                    out.deleted.push(item.external_id);
                    continue;
                }
                newest = newest.max(item.occurred_at);
                out.items.push(item);
            }
            match next {
                Some(n) => page = Some(n),
                None => {
                    complete = true;
                    break;
                }
            }
        }
        if complete && let Some(a) = after {
            // A minute inside the query's window, so an item on the boundary is never misjudged.
            if let Ok(since) = OffsetDateTime::from_unix_timestamp(a + 60) {
                out.windows.push(Window {
                    kind: Some("email"),
                    prefix: String::new(),
                    since,
                    seen,
                });
            }
        }
        out.cursor = ingest::next_cursor(GMAIL, newest, cursor, now);
        Ok(out)
    }
}

pub struct Calendar;

#[async_trait]
impl SyncAdapter for Calendar {
    fn toolkit(&self) -> &'static str {
        CALENDAR
    }
    fn tools(&self) -> &'static [&'static str] {
        &["GOOGLECALENDAR_EVENTS_LIST"]
    }
    fn rate(&self) -> Rate {
        Rate {
            per_minute: 120,
            burst: 10,
        }
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let mut page: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let (tool, mut args) = ingest::page_request(CALENDAR, cursor, page.as_deref(), now)
                .expect("calendar is supported");
            // With updatedMin, deleted events are returned anyway; ask for them explicitly too.
            args["showDeleted"] = json!(true);
            let data = tools.call(tool, args).await?;
            for e in list(&data, &["items", "events", "event_data"]) {
                if e["status"].as_str() == Some("cancelled")
                    && let Some(id) = first_str(e, &["/id"])
                {
                    out.deleted.push(id);
                }
            }
            let (items, next) = ingest::parse_calendar(&data);
            out.items.extend(items);
            match next {
                Some(n) => page = Some(n),
                None => break,
            }
        }
        out.cursor = ingest::next_cursor(CALENDAR, None, cursor, now);
        Ok(out)
    }
}

pub struct Drive;

const DOC_MIME: &str = "application/vnd.google-apps.document";
const FILE_FIELDS: &str =
    "id,name,mimeType,modifiedTime,webViewLink,trashed,owners(displayName,emailAddress)";
/// Google Docs exported per sync; the rest are indexed by name until they change again.
const MAX_EXPORTS: usize = 10;
const DRIVE_BACKFILL_DAYS: i64 = 30;

fn drive_item(f: &Value, text: Option<String>) -> Option<NewItem> {
    let id = first_str(f, &["/id"])?;
    let mime = first_str(f, &["/mimeType", "/mime_type"]).unwrap_or_default();
    Some(NewItem {
        source: DRIVE.into(),
        external_id: id,
        kind: "doc",
        title: first_str(f, &["/name", "/title"]),
        author: first_str(f, &["/owners/0/displayName", "/owners/0/emailAddress"]),
        url: first_str(f, &["/webViewLink", "/web_view_link"]),
        occurred_at: parse_time(f.get("modifiedTime").or(f.get("modified_time"))),
        body: cap(text.unwrap_or_default(), MAX_BODY_CHARS),
        metadata: json!({"mime_type": mime}),
    })
}

/// Inline text from `GOOGLEDRIVE_DOWNLOAD_FILE`, if the result carries it. Defensive: the
/// shape of `downloaded_file_content` is not documented; a hosted-file reference (a URL or an
/// object without text) yields nothing rather than a guess.
pub fn download_text(data: &Value) -> Option<String> {
    let c = [data, &data["data"], &data["response_data"]]
        .into_iter()
        .map(|r| &r["downloaded_file_content"])
        .find(|v| !v.is_null())?;
    let text = match c {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => first_str(c, &["/content", "/text", "/data"]),
        _ => None,
    }?;
    let t = text.trim();
    (!t.is_empty() && !t.starts_with("http://") && !t.starts_with("https://")).then(|| t.to_owned())
}

impl Drive {
    async fn with_text(
        &self,
        tools: &dyn Tools,
        f: &Value,
        exports: &mut usize,
    ) -> Result<Option<NewItem>, PullError> {
        let is_doc = f["mimeType"].as_str() == Some(DOC_MIME);
        let mut text = None;
        if is_doc
            && *exports < MAX_EXPORTS
            && let Some(id) = first_str(f, &["/id"])
        {
            *exports += 1;
            let data = tools
                .call(
                    "GOOGLEDRIVE_DOWNLOAD_FILE",
                    json!({"fileId": id, "mime_type": "text/plain"}),
                )
                .await?;
            text = download_text(&data);
        }
        Ok(drive_item(f, text))
    }
}

#[async_trait]
impl SyncAdapter for Drive {
    fn toolkit(&self) -> &'static str {
        DRIVE
    }
    fn tools(&self) -> &'static [&'static str] {
        &[
            "GOOGLEDRIVE_FIND_FILE",
            "GOOGLEDRIVE_GET_CHANGES_START_PAGE_TOKEN",
            "GOOGLEDRIVE_LIST_CHANGES",
            "GOOGLEDRIVE_DOWNLOAD_FILE",
        ]
    }
    fn rate(&self) -> Rate {
        Rate {
            per_minute: 120,
            burst: 15,
        }
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let mut exports = 0;
        let Some(token) = cursor["page_token"].as_str() else {
            // First sync: remember where the changes feed starts, then backfill recent files.
            let start = tools
                .call("GOOGLEDRIVE_GET_CHANGES_START_PAGE_TOKEN", json!({}))
                .await?;
            let token = top_str(&start, &["/startPageToken", "/start_page_token", "/token"]);
            let since = rfc3339(now - Duration::days(DRIVE_BACKFILL_DAYS));
            let data = tools
                .call(
                    "GOOGLEDRIVE_FIND_FILE",
                    json!({
                        "q": format!("modifiedTime > '{since}' and trashed = false"),
                        "fields": format!("nextPageToken,files({FILE_FIELDS})"),
                        "pageSize": 100,
                        "orderBy": "modifiedTime desc",
                    }),
                )
                .await?;
            for f in list(&data, &["files"]) {
                if let Some(item) = self.with_text(tools, f, &mut exports).await? {
                    out.items.push(item);
                }
            }
            // Without a start token there is no feed to follow; try again next time.
            out.cursor = match token {
                Some(t) => json!({"page_token": t}),
                None => cursor.clone(),
            };
            return Ok(out);
        };
        let mut token = token.to_owned();
        for _ in 0..MAX_PAGES {
            let data = tools
                .call(
                    "GOOGLEDRIVE_LIST_CHANGES",
                    json!({
                        "pageToken": token, "pageSize": 100, "includeRemoved": true,
                        "fields": format!("nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))"),
                    }),
                )
                .await?;
            for c in list(&data, &["changes"]) {
                let file = &c["file"];
                let id =
                    first_str(c, &["/fileId", "/file_id"]).or_else(|| first_str(file, &["/id"]));
                let Some(id) = id else { continue };
                if c["removed"].as_bool() == Some(true) || file["trashed"].as_bool() == Some(true) {
                    out.deleted.push(id);
                } else if file.is_object()
                    && let Some(item) = self.with_text(tools, file, &mut exports).await?
                {
                    out.items.push(item);
                }
            }
            if let Some(fresh) = top_str(&data, &["/newStartPageToken", "/new_start_page_token"]) {
                token = fresh;
                break;
            }
            match top_str(&data, &["/nextPageToken", "/next_page_token"]) {
                Some(n) => token = n,
                None => break,
            }
        }
        let mut c = Map::new();
        c.insert("page_token".into(), json!(token));
        out.cursor = Value::Object(c);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_text_is_defensive() {
        assert_eq!(
            download_text(&json!({"downloaded_file_content": "Hello"})).as_deref(),
            Some("Hello")
        );
        assert_eq!(
            download_text(&json!({"data": {"downloaded_file_content": {"content": " Doc "}}}))
                .as_deref(),
            Some("Doc")
        );
        assert_eq!(
            download_text(&json!({"downloaded_file_content": "https://s3.example/x"})),
            None,
            "a hosted file is not its text"
        );
        assert_eq!(
            download_text(
                &json!({"downloaded_file_content": {"s3url": "https://s3/x", "mimetype": "text/plain"}})
            ),
            None
        );
        assert_eq!(download_text(&json!({})), None);
    }
}
