//! Outlook (mail and calendar in one toolkit) and Microsoft Teams chats, through Microsoft
//! Graph via Composio. Both poll: Outlook's mail trigger carries no body and mail is high
//! volume; Teams has no Composio triggers at all.
//!
//! - Outlook mail: `OUTLOOK_LIST_MESSAGES` with `received_date_time_ge`, `response_detail:
//!   full` (the default `minimal` leaves out `body`) and Composio's own `page_token` /
//!   `next_page_token` paging. A complete listing of the re-read window deletes what vanished.
//! - Outlook calendar: `OUTLOOK_LIST_EVENTS` with an OData filter on `lastModifiedDateTime`;
//!   `isCancelled` events are deleted.
//! - Teams: `MICROSOFT_TEAMS_CHATS_GET_ALL_CHATS`, then `MICROSOFT_TEAMS_LIST_USER_CHAT_MESSAGES`
//!   per chat (the replacement for the deprecated chat-message tools), filtered on
//!   `lastModifiedDateTime`; messages with `deletedDateTime` are deleted. Channel messages
//!   are not synced yet.
//!
//! Inputs are from Composio's docs; outputs are Graph resources (`value`, `subject`, `from`,
//! `receivedDateTime`, `body`, ...) which Composio does not document, so both `value` and a
//! few other list names are accepted.

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use time::{Duration, OffsetDateTime};

use super::{
    MAX_BODY_CHARS, Pull, PullError, Rate, SyncAdapter, Tools, Window, cap, first_str, list,
    parse_time, rfc3339, top_str,
};
use crate::db::NewItem;
use crate::ingest::{strip_html, trim_quoted};

pub const OUTLOOK: &str = "outlook";
pub const TEAMS: &str = "microsoft_teams";
const MAX_PAGES: usize = 5;
const BACKFILL_DAYS: i64 = 30;
const CHATS_PER_SYNC: usize = 5;

fn graph_body(v: &Value) -> String {
    let content = first_str(v, &["/body/content"]).or_else(|| first_str(v, &["/bodyPreview"]));
    let Some(c) = content else {
        return String::new();
    };
    let html = v["body"]["contentType"]
        .as_str()
        .is_some_and(|t| t.eq_ignore_ascii_case("html"))
        || c.contains("</");
    if html { strip_html(&c) } else { c }
}

fn graph_time(v: &Value, key: &str) -> Option<OffsetDateTime> {
    parse_time(v.get(key)).or_else(|| {
        // Graph's dateTimeTimeZone (`{dateTime: "2026-11-05T10:00:00.0000000", timeZone: "UTC"}`).
        let dt = v[key]["dateTime"].as_str()?;
        let tz = v[key]["timeZone"].as_str().unwrap_or("UTC");
        let trimmed = dt.split('.').next()?;
        if tz.eq_ignore_ascii_case("UTC") {
            parse_time(Some(&json!(format!("{trimmed}Z"))))
        } else {
            None
        }
    })
}

pub fn parse_message(m: &Value) -> Option<NewItem> {
    let id = first_str(m, &["/id"])?;
    if m["isDraft"].as_bool() == Some(true) {
        return None;
    }
    let from_name = first_str(m, &["/from/emailAddress/name"]);
    let from_addr = first_str(m, &["/from/emailAddress/address"]);
    let author = match (from_name, from_addr) {
        (Some(n), Some(a)) => Some(format!("{n} <{a}>")),
        (n, a) => n.or(a),
    };
    let to: Vec<String> = m["toRecipients"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| first_str(r, &["/emailAddress/address"]))
        .collect();
    Some(NewItem {
        source: OUTLOOK.into(),
        external_id: id,
        kind: "email",
        title: first_str(m, &["/subject"]),
        author,
        url: first_str(m, &["/webLink"]),
        occurred_at: graph_time(m, "receivedDateTime").or_else(|| graph_time(m, "sentDateTime")),
        body: cap(trim_quoted(&graph_body(m)), MAX_BODY_CHARS),
        metadata: json!({"conversation_id": m["conversationId"], "to": to}),
    })
}

pub fn parse_event(e: &Value) -> Option<NewItem> {
    let id = first_str(e, &["/id"])?;
    let start = first_str(e, &["/start/dateTime"]).unwrap_or_default();
    let end = first_str(e, &["/end/dateTime"]).unwrap_or_default();
    let zone = first_str(e, &["/start/timeZone"]).unwrap_or_default();
    let location = first_str(e, &["/location/displayName"]);
    let attendees: Vec<String> = e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| first_str(a, &["/emailAddress/name", "/emailAddress/address"]))
        .collect();
    let mut body = format!("When: {start} – {end} {zone}");
    if let Some(l) = &location {
        body.push_str(&format!("\nWhere: {l}"));
    }
    if !attendees.is_empty() {
        body.push_str(&format!("\nAttendees: {}", attendees.join(", ")));
    }
    let desc = graph_body(e);
    if !desc.is_empty() {
        body.push_str(&format!("\n\n{desc}"));
    }
    Some(NewItem {
        source: OUTLOOK.into(),
        external_id: id,
        kind: "event",
        title: first_str(e, &["/subject"]),
        author: first_str(
            e,
            &[
                "/organizer/emailAddress/name",
                "/organizer/emailAddress/address",
            ],
        ),
        url: first_str(e, &["/webLink"]),
        occurred_at: graph_time(e, "start"),
        body: cap(body, MAX_BODY_CHARS),
        metadata: json!({"start": start, "end": end, "time_zone": zone, "location": location, "attendees": attendees}),
    })
}

fn next_page(data: &Value) -> Option<String> {
    top_str(data, &["/next_page_token", "/nextPageToken"])
}

pub struct Outlook;

#[async_trait]
impl SyncAdapter for Outlook {
    fn toolkit(&self) -> &'static str {
        OUTLOOK
    }
    fn tools(&self) -> &'static [&'static str] {
        &["OUTLOOK_LIST_MESSAGES", "OUTLOOK_LIST_EVENTS"]
    }
    fn rate(&self) -> Rate {
        // Graph allows 10,000 requests per 10 minutes per mailbox; stay far below.
        Rate {
            per_minute: 120,
            burst: 12,
        }
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let backfill = now - Duration::days(BACKFILL_DAYS);
        let mail_after = cursor["mail_after"]
            .as_str()
            .and_then(|s| parse_time(Some(&json!(s))))
            .unwrap_or(backfill);

        // Mail.
        let mut page: Option<String> = None;
        let mut newest = None;
        let mut seen = Vec::new();
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            let mut args = json!({"received_date_time_ge": rfc3339(mail_after), "top": 50,
                                  "response_detail": "full", "orderby": ["receivedDateTime desc"]});
            if let Some(p) = &page {
                args["page_token"] = json!(p);
            }
            let data = tools.call("OUTLOOK_LIST_MESSAGES", args).await?;
            for m in list(&data, &["value", "messages", "emails"]) {
                if let Some(item) = parse_message(m) {
                    seen.push(item.external_id.clone());
                    newest = newest.max(item.occurred_at);
                    out.items.push(item);
                }
            }
            match next_page(&data) {
                Some(n) => page = Some(n),
                None => {
                    complete = true;
                    break;
                }
            }
        }
        if complete {
            out.windows.push(Window {
                kind: Some("email"),
                prefix: String::new(),
                since: mail_after + Duration::minutes(1),
                seen,
            });
        }
        // Re-read an hour back next time: mail can land with an older timestamp.
        let next_mail = match newest {
            Some(t) if complete => t - Duration::hours(1),
            _ => mail_after,
        };

        // Calendar.
        let events_after = cursor["events_after"]
            .as_str()
            .and_then(|s| parse_time(Some(&json!(s))))
            .unwrap_or(now - Duration::days(90));
        let mut page: Option<String> = None;
        let mut events_complete = false;
        for _ in 0..MAX_PAGES {
            let mut args = json!({"filter": format!("lastModifiedDateTime ge {}", rfc3339(events_after)), "top": 50});
            if let Some(p) = &page {
                args["page_token"] = json!(p);
            }
            let data = tools.call("OUTLOOK_LIST_EVENTS", args).await?;
            for e in list(&data, &["value", "events"]) {
                if e["isCancelled"].as_bool() == Some(true) {
                    if let Some(id) = first_str(e, &["/id"]) {
                        out.deleted.push(id);
                    }
                } else if let Some(item) = parse_event(e) {
                    out.items.push(item);
                }
            }
            match next_page(&data) {
                Some(n) => page = Some(n),
                None => {
                    events_complete = true;
                    break;
                }
            }
        }
        let next_events = if events_complete {
            now - Duration::minutes(5)
        } else {
            events_after
        };
        out.cursor =
            json!({"mail_after": rfc3339(next_mail), "events_after": rfc3339(next_events)});
        Ok(out)
    }
}

pub struct Teams;

pub fn parse_chat_message(chat: &str, topic: &str, m: &Value) -> Result<Option<NewItem>, String> {
    let Some(id) = first_str(m, &["/id"]) else {
        return Ok(None);
    };
    let ext = format!("{chat}:{id}");
    if !m["deletedDateTime"].is_null() && m.get("deletedDateTime").is_some() {
        return Err(ext);
    }
    if m["messageType"].as_str().is_some_and(|t| t != "message") {
        return Ok(None);
    }
    let body = graph_body(m);
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some(NewItem {
        source: TEAMS.into(),
        external_id: ext,
        kind: "message",
        title: Some(topic.to_owned()),
        author: first_str(
            m,
            &["/from/user/displayName", "/from/application/displayName"],
        ),
        url: first_str(m, &["/webUrl"]),
        occurred_at: graph_time(m, "createdDateTime"),
        body: cap(body, MAX_BODY_CHARS),
        metadata: json!({"chat_id": chat, "last_modified": m["lastModifiedDateTime"]}),
    }))
}

#[async_trait]
impl SyncAdapter for Teams {
    fn toolkit(&self) -> &'static str {
        TEAMS
    }
    fn tools(&self) -> &'static [&'static str] {
        &[
            "MICROSOFT_TEAMS_CHATS_GET_ALL_CHATS",
            "MICROSOFT_TEAMS_LIST_USER_CHAT_MESSAGES",
        ]
    }
    fn rate(&self) -> Rate {
        Rate {
            per_minute: 60,
            burst: 1 + CHATS_PER_SYNC as u32,
        }
    }
    fn max_calls(&self) -> u32 {
        1 + CHATS_PER_SYNC as u32
    }
    async fn pull(
        &self,
        tools: &dyn Tools,
        cursor: &Value,
        now: OffsetDateTime,
    ) -> Result<Pull, PullError> {
        let mut out = Pull::default();
        let data = tools
            .call(
                "MICROSOFT_TEAMS_CHATS_GET_ALL_CHATS",
                json!({"user_id": "me", "top": 50}),
            )
            .await?;
        let state = &cursor["chats"];
        let mut chats: Vec<(String, String, Option<OffsetDateTime>)> =
            list(&data, &["value", "chats"])
                .iter()
                .filter_map(|c| {
                    let id = first_str(c, &["/id"])?;
                    let topic = first_str(c, &["/topic"]).unwrap_or_else(|| "Teams chat".into());
                    Some((id, topic, graph_time(c, "lastUpdatedDateTime")))
                })
                .collect();
        let synced = |id: &str| {
            state[id]["after"]
                .as_str()
                .and_then(|s| parse_time(Some(&json!(s))))
        };
        // Chats with news first (or never synced), least recently synced first.
        chats.retain(|(id, _, updated)| {
            state[id.as_str()]["page"].is_string()
                || match (synced(id), updated) {
                    (Some(s), Some(u)) => *u > s,
                    _ => true,
                }
        });
        chats.sort_by_key(|(id, _, _)| state[id]["at"].as_i64().unwrap_or(0));
        let mut next: Map<String, Value> = state.as_object().cloned().unwrap_or_default();
        for (id, topic, _) in chats.into_iter().take(CHATS_PER_SYNC) {
            let st = &state[&id];
            let after = synced(&id).unwrap_or(now - Duration::days(BACKFILL_DAYS));
            // Graph lists chat messages newest first only. An unfinished read resumes from its
            // page token with the same filter; `after` moves once everything newer was read.
            let mut args = json!({"user_id": "me", "chat_id": id, "top": 50,
                                  "filter": format!("lastModifiedDateTime gt {}", rfc3339(after)),
                                  "orderby": "lastModifiedDateTime desc"});
            if let Some(p) = st["page"].as_str() {
                args["page_token"] = json!(p);
            }
            let data = tools
                .call("MICROSOFT_TEAMS_LIST_USER_CHAT_MESSAGES", args)
                .await?;
            let mut newest = st["pending"]
                .as_str()
                .and_then(|s| parse_time(Some(&json!(s))));
            for m in list(&data, &["value", "messages"]) {
                newest = newest.max(graph_time(m, "lastModifiedDateTime"));
                match parse_chat_message(&id, &topic, m) {
                    Ok(Some(item)) => out.items.push(item),
                    Ok(None) => {}
                    Err(deleted) => out.deleted.push(deleted),
                }
            }
            let state = match next_page(&data) {
                Some(p) => json!({"after": rfc3339(after), "page": p,
                                  "pending": newest.map(rfc3339), "at": now.unix_timestamp()}),
                None => json!({"after": rfc3339(newest.unwrap_or(after).max(after)),
                               "at": now.unix_timestamp()}),
            };
            next.insert(id, state);
        }
        out.cursor = json!({"chats": next});
        Ok(out)
    }
}
