//! Notion pages: `NOTION_SEARCH_NOTION_PAGE` sorted by `last_edited_time` (newest first) finds
//! what changed, `NOTION_GET_PAGE_MARKDOWN` reads each changed page. Trigger mode: Notion's
//! triggers (`NOTION_PAGE_CREATED`, `NOTION_PAGE_CONTENT_UPDATED`) are webhooks, and pages
//! change rarely, so a trigger nudges the sync and a daily poll is the safety net.
//!
//! Search inputs are from Composio's docs (`filter_value`, `filter_property`, `timestamp`,
//! `direction`, `page_size`, `start_cursor`; there is no filter/sort object). The results'
//! fields (`results`, `next_cursor`, `has_more`, `last_edited_time`, `archived`, `in_trash`,
//! title properties) are Notion's own; Composio does not document them, nor the field holding
//! the page markdown, so several spellings are accepted. Archived or trashed pages are deleted;
//! pages deleted for good are not seen by search and stay until the connection is removed.

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use super::{
    MAX_BODY_CHARS, Pull, PullError, Rate, SyncAdapter, Tools, cap, first_str, list, parse_time,
    rfc3339, top_bool, top_str,
};
use crate::db::NewItem;

pub const NOTION: &str = "notion";
pub const TRIGGERS: &[&str] = &["NOTION_PAGE_CREATED", "NOTION_PAGE_CONTENT_UPDATED"];
const SEARCH_PAGES: usize = 3;
/// Pages read per sync; the rest are read next time (see the cursor's `done`).
const MAX_READS: usize = 15;

pub struct Notion;

/// The page's title: the property whose `type` is `title`.
pub fn page_title(p: &Value) -> Option<String> {
    let props = p["properties"].as_object()?;
    props
        .values()
        .find(|v| v["type"] == "title" || v["title"].is_array())
        .and_then(|v| v["title"].as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|t| first_str(t, &["/plain_text", "/text/content"]))
                .collect::<String>()
        })
        .filter(|t| !t.trim().is_empty())
}

pub fn page_markdown(data: &Value) -> Option<String> {
    if let Value::String(s) = data {
        return Some(s.clone());
    }
    top_str(data, &["/markdown", "/content", "/page_markdown", "/text"])
}

#[async_trait]
impl SyncAdapter for Notion {
    fn toolkit(&self) -> &'static str {
        NOTION
    }
    fn tools(&self) -> &'static [&'static str] {
        &["NOTION_SEARCH_NOTION_PAGE", "NOTION_GET_PAGE_MARKDOWN"]
    }
    fn rate(&self) -> Rate {
        // Notion averages 3 requests a second per integration.
        Rate {
            per_minute: 150,
            burst: 10,
        }
    }
    fn max_calls(&self) -> u32 {
        (SEARCH_PAGES + MAX_READS) as u32
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        _now: OffsetDateTime,
        _backfill_days: u32,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let after = cursor["edited_after"]
            .as_str()
            .and_then(|s| parse_time(Some(&json!(s))));
        // Pages already read during an unfinished pass: id → last_edited_time.
        let mut done: Map<String, Value> = cursor["done"].as_object().cloned().unwrap_or_default();
        let mut start: Option<String> = None;
        let mut newest = after;
        let mut reads = 0;
        let mut finished = false;
        'pages: for _ in 0..SEARCH_PAGES {
            let mut args = json!({"query": "", "filter_value": "page", "filter_property": "object",
                                  "timestamp": "last_edited_time", "direction": "descending", "page_size": 50});
            if let Some(s) = &start {
                args["start_cursor"] = json!(s);
            }
            let data = tools.call("NOTION_SEARCH_NOTION_PAGE", args).await?;
            for p in list(&data, &["results", "pages"]) {
                let Some(id) = first_str(p, &["/id"]) else {
                    continue;
                };
                let edited_s = first_str(p, &["/last_edited_time"]).unwrap_or_default();
                let edited = parse_time(Some(&json!(edited_s)));
                if let (Some(e), Some(a)) = (edited, after)
                    && e <= a
                {
                    finished = true;
                    break 'pages;
                }
                newest = newest.max(edited);
                if p["archived"].as_bool() == Some(true) || p["in_trash"].as_bool() == Some(true) {
                    out.deleted.push(id);
                    continue;
                }
                if done.get(&id).and_then(Value::as_str) == Some(edited_s.as_str()) {
                    continue;
                }
                if reads >= MAX_READS {
                    break 'pages;
                }
                reads += 1;
                let md = tools
                    .call("NOTION_GET_PAGE_MARKDOWN", json!({"page_id": id}))
                    .await?;
                let body = page_markdown(&md).unwrap_or_default();
                out.items.push(NewItem {
                    source: NOTION.into(),
                    external_id: id.clone(),
                    kind: "doc",
                    title: page_title(p),
                    author: first_str(p, &["/created_by/name", "/created_by/id"]),
                    url: first_str(p, &["/url", "/public_url"]),
                    occurred_at: edited,
                    body: cap(body, MAX_BODY_CHARS),
                    metadata: json!({"parent": p["parent"]}),
                });
                done.insert(id, json!(edited_s));
            }
            let more = top_bool(&data, "has_more") != Some(false);
            match top_str(&data, &["/next_cursor"]).filter(|_| more) {
                Some(c) => start = Some(c),
                None => {
                    finished = true;
                    break;
                }
            }
        }
        out.cursor = if finished {
            json!({"edited_after": newest.map(rfc3339)})
        } else {
            // Keep the old mark; skip what was read already when the pass resumes.
            let mut keep = done;
            if keep.len() > 2000 {
                keep.clear();
            }
            json!({"edited_after": after.map(rfc3339), "done": keep})
        };
        Ok(out)
    }
}
