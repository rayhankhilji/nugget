//! A per-user snapshot of the cloud corpus in an in-memory `nugget_store::Store`, with the
//! metadata shape the local ingest pipeline writes (`extracted` mail headers, the connector's
//! `source`, `mime`, the privacy labels from `nugget_ingest::rule_labels`). `nugget-knowledge` and
//! `nugget-proactive` run on it unchanged. Item keys are the Postgres item ids, so evidence maps
//! straight back to `items` rows.

use nugget_store::{NewChunk, NewItem, Store};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::db::{Db, DbError};

/// History the jobs look at (events in the future are always included).
pub const WINDOW_DAYS: i32 = 180;
/// Items per snapshot at most, newest first.
pub const MAX_ITEMS: i64 = 4_000;

#[derive(Debug, Clone)]
pub struct CloudItem {
    pub id: Uuid,
    pub source: String,
    pub kind: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub occurred_at: Option<OffsetDateTime>,
    pub body: String,
    pub metadata: Value,
    pub content_hash: String,
    pub updated_at: OffsetDateTime,
    pub chunks: Vec<String>,
}

/// The user's recent items with their chunk texts, and the newest `updated_at` among them.
pub async fn load(
    db: &Db,
    user: Uuid,
) -> Result<(Vec<CloudItem>, Option<OffsetDateTime>), DbError> {
    db.scoped(Some(user), async |tx| {
        let rows = tx
            .query(
                "select id, source, kind, title, author, url, occurred_at, body, metadata, content_hash, updated_at
                 from public.items
                 where user_id = $1 and (occurred_at is null
                   or occurred_at > now() - make_interval(days => $2))
                 order by occurred_at desc nulls last limit $3",
                &[&user, &WINDOW_DAYS, &MAX_ITEMS],
            )
            .await?;
        let through = tx
            .query_one(
                "select max(updated_at) from public.items where user_id = $1",
                &[&user],
            )
            .await?
            .get(0);
        let ids: Vec<Uuid> = rows.iter().map(|r| r.get("id")).collect();
        let chunk_rows = tx
            .query(
                "select item_id, text from public.chunks where user_id = $1 and item_id = any($2) order by item_id, ord",
                &[&user, &ids],
            )
            .await?;
        let mut chunks: std::collections::HashMap<Uuid, Vec<String>> = Default::default();
        for r in &chunk_rows {
            chunks.entry(r.get(0)).or_default().push(r.get(1));
        }
        let items = rows
            .iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                CloudItem {
                    id,
                    source: r.get("source"),
                    kind: r.get("kind"),
                    title: r.get("title"),
                    author: r.get("author"),
                    url: r.get("url"),
                    occurred_at: r.get("occurred_at"),
                    body: r.get("body"),
                    metadata: r.get("metadata"),
                    content_hash: r.get("content_hash"),
                    updated_at: r.get("updated_at"),
                    chunks: chunks.remove(&id).unwrap_or_default(),
                }
            })
            .collect();
        Ok((items, through))
    })
    .await
}

/// Splits an address-list header (`A <a@x>, "Doe, J" <j@y>`) on commas outside quotes and
/// angle brackets.
pub fn split_addresses(s: &str) -> Vec<String> {
    let (mut out, mut cur) = (Vec::new(), String::new());
    let (mut quoted, mut angle) = (false, false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
        .collect()
}

fn ids(v: &Value) -> Vec<String> {
    v.as_str()
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn rfc3339(t: Option<OffsetDateTime>) -> Value {
    t.and_then(|t| t.format(&Rfc3339).ok())
        .map_or(Value::Null, Value::String)
}

/// Event metadata synced before the local-connector shape: `start`/`end` strings and attendee
/// names become the objects the extractors read.
fn upgrade_event_meta(it: &CloudItem) -> Value {
    let mut m = it.metadata.clone();
    let Some(o) = m.as_object_mut() else {
        return json!({"event_id": it.id.to_string()});
    };
    for k in ["start", "end"] {
        if let Some(s) = o.get(k).and_then(Value::as_str).map(str::to_owned) {
            let t = if s.contains('T') {
                json!({"date_time": s})
            } else {
                json!({"date": s})
            };
            o.insert(k.into(), t);
        }
    }
    if let Some(Value::Array(a)) = o.get_mut("attendees") {
        for p in a.iter_mut() {
            if let Value::String(s) = p {
                *p = if s.contains('@') {
                    json!({"email": s})
                } else {
                    json!({"name": s})
                };
            }
        }
    }
    o.entry("event_id")
        .or_insert_with(|| json!(it.id.to_string()));
    m
}

/// The item as the local pipeline would have stored it.
pub fn to_store_item(it: &CloudItem) -> NewItem {
    let key = it.id.to_string();
    let m = &it.metadata;
    let (mime, source, extracted) = match it.kind.as_str() {
        "email" => {
            let list = |k: &str| {
                m[k].as_str()
                    .map(split_addresses)
                    .map_or(Value::Array(vec![]), |v| json!(v))
            };
            let mut ex = json!({
                "from": it.author,
                "to": list("to"),
                "cc": list("cc"),
                "subject": it.title,
                "message_id": m["message_id"],
                "in_reply_to": ids(&m["in_reply_to"]),
                "references": ids(&m["references"]),
                "date": rfc3339(it.occurred_at),
            });
            for k in [
                "list_unsubscribe",
                "list_id",
                "precedence",
                "auto_submitted",
            ] {
                if m[k].is_string() {
                    ex[k] = m[k].clone();
                }
            }
            ("message/rfc822", m.clone(), ex)
        }
        "event" => ("text/plain", upgrade_event_meta(it), json!({})),
        _ => ("text/plain", m.clone(), json!({})),
    };
    let mut meta: Map<String, Value> = nugget_ingest::rule_labels(
        &key,
        &[],
        it.title.as_deref(),
        &it.body,
        &extracted,
        m.get("sensitive") == Some(&Value::Bool(true)),
    );
    meta.insert("source".into(), source);
    meta.insert("mime".into(), json!(mime));
    if let Some(u) = &it.url {
        meta.insert("url".into(), json!(u));
    }
    if extracted.as_object().is_some_and(|o| !o.is_empty()) {
        meta.insert("extracted".into(), extracted);
    }
    let mut new = NewItem::new(it.source.as_str(), key, it.content_hash.clone())
        .with_mime(mime)
        .with_metadata(Value::Object(meta));
    new.title = it.title.clone();
    new.text = Some(it.body.clone());
    new.version = Some(it.content_hash.clone());
    new.source_modified_at = it.occurred_at;
    new
}

/// An in-memory store holding `items`.
pub fn build(items: &[CloudItem]) -> nugget_core::Result<Store> {
    let store = Store::open_in_memory()?;
    let batch: Vec<(NewItem, Vec<NewChunk>)> = items
        .iter()
        .map(|it| {
            let chunks = if it.chunks.is_empty() {
                vec![NewChunk::text(it.body.clone())]
            } else {
                it.chunks.iter().cloned().map(NewChunk::text).collect()
            };
            (to_store_item(it), chunks)
        })
        .collect();
    for part in batch.chunks(256) {
        store.upsert_items_with_chunks(part)?;
    }
    Ok(store)
}

/// Whether the snapshot item is confirmed sensitive (kept from models and prose).
pub fn confirmed_sensitive(item: &nugget_store::Item) -> bool {
    nugget_retrieve::is_confirmed_sensitive(&item.metadata)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mail(
        id: u128,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        at: &str,
        labels: &[&str],
    ) -> CloudItem {
        CloudItem {
            id: Uuid::from_u128(id),
            source: "gmail".into(),
            kind: "email".into(),
            title: Some(subject.into()),
            author: Some(from.into()),
            url: None,
            occurred_at: OffsetDateTime::parse(at, &Rfc3339).ok(),
            body: body.into(),
            metadata: json!({"to": to, "labels": labels, "thread_id": format!("t{id}")}),
            content_hash: format!("h{id}"),
            updated_at: OffsetDateTime::now_utc(),
            chunks: vec![],
        }
    }

    #[test]
    fn address_lists_split_outside_quotes() {
        assert_eq!(
            split_addresses(r#""Doe, Jane" <j@x.com>, b@y.com; C <c@z>"#),
            vec![r#""Doe, Jane" <j@x.com>"#, "b@y.com", "C <c@z>"]
        );
        assert!(split_addresses(" , ").is_empty());
    }

    #[test]
    fn mail_gets_local_shape_and_labels() {
        let it = mail(
            1,
            "Sam <sam@acme.example>",
            "me@me.example, \"Lee, A\" <a@b.example>",
            "Contract",
            "Can you send the signed contract by Friday?",
            "2026-10-30T09:00:00Z",
            &["INBOX"],
        );
        let n = to_store_item(&it);
        assert_eq!(n.mime.as_deref(), Some("message/rfc822"));
        let m = &n.metadata;
        assert_eq!(m["extracted"]["from"], "Sam <sam@acme.example>");
        assert_eq!(m["extracted"]["to"][1], "\"Lee, A\" <a@b.example>");
        assert_eq!(m["source"]["labels"][0], "INBOX");
        assert_eq!(m["sensitive"], false);
        assert!(!nugget_retrieve::is_confirmed_sensitive(m));

        let mut s = it.clone();
        s.body = "Your therapy session notes".into();
        let m = to_store_item(&s).metadata;
        assert_eq!(m["sensitive"], true);
        assert!(nugget_retrieve::is_confirmed_sensitive(&m));
    }

    #[test]
    fn old_event_metadata_is_upgraded() {
        let it = CloudItem {
            id: Uuid::from_u128(9),
            source: "googlecalendar".into(),
            kind: "event".into(),
            title: Some("Standup".into()),
            author: None,
            url: None,
            occurred_at: None,
            body: "When: …".into(),
            metadata: json!({"start": "2026-11-05T10:00:00Z", "end": "2026-11-05", "attendees": ["Ann", "b@x.example"]}),
            content_hash: "h".into(),
            updated_at: OffsetDateTime::now_utc(),
            chunks: vec![],
        };
        let m = to_store_item(&it).metadata;
        assert_eq!(m["source"]["start"]["date_time"], "2026-11-05T10:00:00Z");
        assert_eq!(m["source"]["end"]["date"], "2026-11-05");
        assert_eq!(m["source"]["attendees"][1]["email"], "b@x.example");
        assert_eq!(m["source"]["event_id"], it.id.to_string());
    }
}
