//! Slack: channel messages through `SLACK_LIST_ALL_CHANNELS` and
//! `SLACK_FETCH_CONVERSATION_HISTORY` (polling).
//!
//! Since 2025-05-29 Slack limits `conversations.history` for apps that are not Marketplace
//! approved (Composio's managed Slack app among them) to 1 request a minute and 15 messages a
//! page. So each sync lists channels once and reads at most [`CHANNELS_PER_SYNC`] channels,
//! least recently synced first, and the bucket allows exactly that many calls back to back.
//! With our own Marketplace-approved app the rate can go up to Tier 3 (50+/minute).
//!
//! Each channel is re-read over the last day, so edits (`edited`) update items and a complete
//! re-read deletes messages that are gone (`tombstone` subtypes are deletes too). Thread replies
//! (`SLACK_FETCH_MESSAGE_THREAD_FROM_A_CONVERSATION`) and user display names are not fetched:
//! authors are Slack user ids. Message fields (`ts`, `user`, `text`, `subtype`, `edited`,
//! `thread_ts`, `reply_count`) are Slack's own; Composio does not document them.

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use super::{
    MAX_BODY_CHARS, Pull, PullError, Rate, SyncAdapter, Tools, Window, cap, first_str, list,
    top_str,
};
use crate::db::NewItem;

pub const SLACK: &str = "slack";
pub const CHANNELS_PER_SYNC: usize = 3;
const BACKFILL_SECS: f64 = 14.0 * 86_400.0;
const LOOKBACK_SECS: f64 = 86_400.0;
const HISTORY_PAGES: usize = 1;

/// Messages that carry no content of their own.
const SKIP_SUBTYPES: &[&str] = &[
    "channel_join",
    "channel_leave",
    "channel_topic",
    "channel_purpose",
    "channel_name",
    "channel_archive",
    "channel_unarchive",
    "group_join",
    "group_leave",
    "bot_add",
    "bot_remove",
    "pinned_item",
    "unpinned_item",
];

pub struct Slack;

fn ts_secs(ts: &str) -> Option<f64> {
    ts.parse::<f64>().ok().filter(|t| t.is_finite() && *t > 0.0)
}

fn ts_time(ts: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos((ts_secs(ts)? * 1e9) as i128).ok()
}

/// One page of history for `channel` → items, deletes and the newest ts seen.
pub fn parse_history(
    channel: &str,
    name: &str,
    data: &Value,
) -> (Vec<NewItem>, Vec<String>, Vec<String>) {
    let mut items = Vec::new();
    let mut deleted = Vec::new();
    let mut seen = Vec::new();
    for m in list(data, &["messages"]) {
        let Some(ts) = first_str(m, &["/ts"]) else {
            continue;
        };
        let id = format!("{channel}:{ts}");
        let subtype = m["subtype"].as_str().unwrap_or("");
        if subtype == "tombstone" || subtype == "message_deleted" {
            deleted.push(id);
            continue;
        }
        seen.push(id.clone());
        if SKIP_SUBTYPES.contains(&subtype) {
            continue;
        }
        let text = first_str(m, &["/text"]).unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        items.push(NewItem {
            source: SLACK.into(),
            external_id: id,
            kind: "message",
            title: Some(name.to_owned()),
            author: first_str(
                m,
                &["/user_profile/real_name", "/user", "/username", "/bot_id"],
            ),
            url: None,
            occurred_at: ts_time(&ts),
            body: cap(text, MAX_BODY_CHARS),
            metadata: json!({
                "channel": channel,
                "thread_ts": m["thread_ts"],
                "reply_count": m["reply_count"],
                "edited": m["edited"]["ts"],
            }),
        });
    }
    (items, deleted, seen)
}

#[async_trait]
impl SyncAdapter for Slack {
    fn toolkit(&self) -> &'static str {
        SLACK
    }
    fn tools(&self) -> &'static [&'static str] {
        &[
            "SLACK_LIST_ALL_CHANNELS",
            "SLACK_FETCH_CONVERSATION_HISTORY",
        ]
    }
    fn rate(&self) -> Rate {
        Rate {
            per_minute: 1,
            burst: 1 + CHANNELS_PER_SYNC as u32,
        }
    }
    fn max_calls(&self) -> u32 {
        1 + CHANNELS_PER_SYNC as u32
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        // `limit` defaults to 1 in Composio's schema; always set it.
        let data = tools
            .call(
                "SLACK_LIST_ALL_CHANNELS",
                json!({"limit": 200, "types": "public_channel,private_channel,im,mpim", "exclude_archived": true}),
            )
            .await?;
        let mut channels: Vec<(String, String)> = list(&data, &["channels"])
            .iter()
            .filter(|c| c["is_member"].as_bool() != Some(false))
            .filter_map(|c| {
                let id = first_str(c, &["/id"])?;
                let name = first_str(c, &["/name"])
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| "Direct message".into());
                Some((id, name))
            })
            .collect();
        let state = &cursor["channels"];
        let synced_at = |id: &str| state[id]["at"].as_i64().unwrap_or(0);
        channels.sort_by_key(|(id, _)| synced_at(id));
        let mut next: Map<String, Value> = state.as_object().cloned().unwrap_or_default();
        let now_s = now.unix_timestamp() as f64;
        for (id, name) in channels.into_iter().take(CHANNELS_PER_SYNC) {
            let st = &state[&id];
            let latest = st["latest"].as_str().and_then(ts_secs);
            // History comes newest first. An unfinished read resumes from its page cursor with
            // the same `oldest`, and `latest` only moves once the whole window was read, so no
            // older message is skipped.
            let resume = st["page"].as_str().map(str::to_owned);
            let oldest = match (resume.is_some(), st["oldest"].as_f64(), latest) {
                (true, Some(o), _) => o,
                (_, _, Some(l)) => l - LOOKBACK_SECS,
                _ => now_s - BACKFILL_SECS,
            };
            let mut page = resume.clone();
            let mut seen = Vec::new();
            let mut newest = st["pending"].as_str().and_then(ts_secs).or(latest);
            let mut complete = false;
            for _ in 0..HISTORY_PAGES {
                let mut args =
                    json!({"channel": id, "oldest": format!("{oldest:.6}"), "limit": 200});
                if let Some(p) = &page {
                    args["cursor"] = json!(p);
                }
                let data = tools.call("SLACK_FETCH_CONVERSATION_HISTORY", args).await?;
                let (items, deleted, s) = parse_history(&id, &name, &data);
                for i in &items {
                    let t = i.external_id.rsplit(':').next().and_then(ts_secs);
                    newest = match (newest, t) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                }
                out.items.extend(items);
                out.deleted.extend(deleted);
                seen.extend(s);
                let more = top_str(&data, &["/response_metadata/next_cursor", "/next_cursor"])
                    .filter(|c| !c.is_empty());
                match more {
                    Some(c) => page = Some(c),
                    None => {
                        complete = true;
                        break;
                    }
                }
            }
            let fmt = |t: Option<f64>| t.map(|n| format!("{n:.6}"));
            let state = if complete {
                // Only a read of the whole window in one go proves what is gone.
                if resume.is_none()
                    && let Ok(since) = OffsetDateTime::from_unix_timestamp(oldest.ceil() as i64 + 1)
                {
                    out.windows.push(Window {
                        kind: Some("message"),
                        prefix: format!("{id}:"),
                        since,
                        seen,
                    });
                }
                json!({"latest": fmt(newest), "at": now.unix_timestamp()})
            } else {
                json!({"latest": fmt(latest), "page": page, "oldest": oldest,
                       "pending": fmt(newest), "at": now.unix_timestamp()})
            };
            next.insert(id, state);
        }
        out.cursor = json!({"channels": next});
        Ok(out)
    }
}
