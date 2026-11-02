//! Adapter conformance suite: the same checks for every app in the catalogue.
//!
//! Each app has a fixture: Composio responses for a first sync and for a second sync in
//! which some items were edited, some deleted (explicitly or by vanishing from a complete
//! listing) and some added. The responses are not live recordings (we have no Composio key in
//! CI); they are shaped from the upstream APIs' documented resources (Gmail, Calendar, Drive
//! v3, Slack Web API, Notion API, Microsoft Graph) inside Composio's `data` envelope, which is
//! what Composio documents. The checks:
//!
//! 1. A pull only calls the adapter's declared read tools, within its call budget, and every
//!    item belongs to the app, has a non-empty unique id, a valid kind and a bounded body.
//! 2. The cursor is used: the second sync's calls differ from the first's.
//! 3. Garbage or empty responses never panic and never invent items or deletes.
//! 4. Through `Ingest::sync` and a real Postgres: the first sync stores exactly the expected
//!    items; the second updates the edited ones, deletes the deleted ones, adds the new ones;
//!    a third with unchanged responses changes nothing (content-hash dedup).

use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

use super::{CATALOG, MAX_BODY_CHARS, SyncAdapter, Tools, adapter, rfc3339};
use crate::composio::ComposioError;
use crate::testkit::{Scripted, scripted};

fn on(tool: &str, when: Option<Value>, data: Value) -> Scripted {
    Scripted {
        tool: tool.into(),
        when,
        data,
    }
}

struct Fixture {
    app: &'static str,
    first: Vec<Scripted>,
    /// External ids stored after the first sync.
    first_ids: Vec<String>,
    second: Vec<Scripted>,
    edited: Vec<String>,
    deleted: Vec<String>,
    added: Vec<String>,
}

fn ago(now: OffsetDateTime, minutes: i64) -> OffsetDateTime {
    now - Duration::minutes(minutes)
}

fn slack_ts(t: OffsetDateTime, n: u32) -> String {
    format!("{}.{:06}", t.unix_timestamp(), n)
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn fixtures(now: OffsetDateTime) -> Vec<Fixture> {
    let t = |m: i64| rfc3339(ago(now, m));
    let gmail_msg = |id: &str, mins: i64, subject: &str, labels: &[&str]| {
        json!({"messageId": id, "threadId": format!("t-{id}"), "messageTimestamp": t(mins),
               "sender": "Ann <ann@example.com>", "to": "me@example.com", "subject": subject,
               "messageText": format!("{subject} body"), "labelIds": labels})
    };
    let gcal_event = |id: &str, summary: &str, status: &str| {
        json!({"id": id, "status": status, "summary": summary, "htmlLink": format!("https://calendar.google.com/{id}"),
               "start": {"dateTime": "2026-11-20T10:00:00Z"}, "end": {"dateTime": "2026-11-20T11:00:00Z"},
               "organizer": {"email": "ann@example.com"}})
    };
    let drive_file = |id: &str, name: &str, mime: &str| {
        json!({"id": id, "name": name, "mimeType": mime, "modifiedTime": t(30), "trashed": false,
               "webViewLink": format!("https://docs.google.com/d/{id}"),
               "owners": [{"displayName": "Ann", "emailAddress": "ann@example.com"}]})
    };
    let doc = "application/vnd.google-apps.document";
    let s1 = slack_ts(ago(now, 50), 100);
    let s2 = slack_ts(ago(now, 45), 200);
    let s3 = slack_ts(ago(now, 40), 300);
    let s4 = slack_ts(ago(now, 35), 400);
    let s5 = slack_ts(ago(now, 5), 500);
    let slack_msg = |ts: &str, text: &str| json!({"type": "message", "user": "U024BE7LH", "text": text, "ts": ts});
    let notion_page = |id: &str, title: &str, mins: i64, trash: bool| {
        json!({"object": "page", "id": id, "last_edited_time": t(mins), "in_trash": trash, "archived": false,
               "url": format!("https://www.notion.so/{id}"), "created_by": {"object": "user", "id": "u-1"},
               "parent": {"type": "workspace", "workspace": true},
               "properties": {"Name": {"id": "title", "type": "title",
                               "title": [{"type": "text", "plain_text": title}]}}})
    };
    let outlook_msg = |id: &str, subject: &str, mins: i64, draft: bool| {
        json!({"id": id, "subject": subject, "receivedDateTime": t(mins), "isDraft": draft,
               "from": {"emailAddress": {"name": "Ann", "address": "ann@example.com"}},
               "toRecipients": [{"emailAddress": {"address": "me@example.com"}}],
               "body": {"contentType": "html", "content": format!("<p>{subject} body</p>")},
               "conversationId": "conv-1", "webLink": format!("https://outlook.office.com/{id}")})
    };
    let outlook_event = |id: &str, subject: &str, cancelled: bool| {
        json!({"id": id, "subject": subject, "isCancelled": cancelled,
               "start": {"dateTime": "2026-11-20T10:00:00.0000000", "timeZone": "UTC"},
               "end": {"dateTime": "2026-11-20T10:30:00.0000000", "timeZone": "UTC"},
               "location": {"displayName": "Room 1"}, "organizer": {"emailAddress": {"name": "Ann"}},
               "attendees": [{"emailAddress": {"name": "Bob", "address": "bob@example.com"}}]})
    };
    let teams_msg = |id: &str, text: &str, deleted: bool, kind: &str| {
        json!({"id": id, "messageType": kind, "createdDateTime": t(20), "lastModifiedDateTime": t(10),
               "deletedDateTime": if deleted { json!(t(5)) } else { Value::Null },
               "from": {"user": {"displayName": "Ann"}},
               "body": {"contentType": "html", "content": format!("<div>{text}</div>")},
               "webUrl": format!("https://teams.microsoft.com/l/message/{id}")})
    };
    vec![
        Fixture {
            app: "gmail",
            first: vec![on(
                "GMAIL_FETCH_EMAILS",
                None,
                json!({"messages": [
                    gmail_msg("m1", 20, "Lease", &["INBOX"]),
                    gmail_msg("m2", 30, "Invoice", &["INBOX"]),
                    gmail_msg("m3", 40, "Spam offer", &["SPAM"]),
                ]}),
            )],
            first_ids: ids(&["m1", "m2"]),
            second: vec![on(
                "GMAIL_FETCH_EMAILS",
                None,
                json!({"messages": [
                    gmail_msg("m1", 20, "Lease (updated)", &["INBOX", "IMPORTANT"]),
                    gmail_msg("m4", 5, "New", &["INBOX"]),
                ]}),
            )],
            edited: ids(&["m1"]),
            // m2 vanished from the re-read window (trashed upstream).
            deleted: ids(&["m2"]),
            added: ids(&["m4"]),
        },
        Fixture {
            app: "googlecalendar",
            first: vec![on(
                "GOOGLECALENDAR_EVENTS_LIST",
                None,
                json!({"items": [
                    gcal_event("e1", "Dentist", "confirmed"), gcal_event("e2", "Lunch", "confirmed"),
                ]}),
            )],
            first_ids: ids(&["e1", "e2"]),
            second: vec![on(
                "GOOGLECALENDAR_EVENTS_LIST",
                None,
                json!({"items": [
                    gcal_event("e1", "Dentist (moved)", "confirmed"), gcal_event("e2", "Lunch", "cancelled"),
                    gcal_event("e3", "Standup", "confirmed"),
                ]}),
            )],
            edited: ids(&["e1"]),
            deleted: ids(&["e2"]),
            added: ids(&["e3"]),
        },
        Fixture {
            app: "googledrive",
            first: vec![
                on(
                    "GOOGLEDRIVE_GET_CHANGES_START_PAGE_TOKEN",
                    None,
                    json!({"startPageToken": "100"}),
                ),
                on(
                    "GOOGLEDRIVE_FIND_FILE",
                    None,
                    json!({"files": [
                        drive_file("d1", "Plan", doc), drive_file("f2", "Scan.pdf", "application/pdf"),
                    ]}),
                ),
                on(
                    "GOOGLEDRIVE_DOWNLOAD_FILE",
                    Some(json!({"fileId": "d1"})),
                    json!({"downloaded_file_content": "Plan v1"}),
                ),
            ],
            first_ids: ids(&["d1", "f2"]),
            second: vec![
                on(
                    "GOOGLEDRIVE_LIST_CHANGES",
                    Some(json!({"pageToken": "100"})),
                    json!({"newStartPageToken": "101", "changes": [
                        {"fileId": "d1", "removed": false, "file": drive_file("d1", "Plan", doc)},
                        {"fileId": "f2", "removed": true},
                        {"fileId": "f3", "removed": false, "file": drive_file("f3", "Notes.txt", "text/plain")},
                    ]}),
                ),
                on(
                    "GOOGLEDRIVE_DOWNLOAD_FILE",
                    Some(json!({"fileId": "d1"})),
                    json!({"downloaded_file_content": "Plan v2"}),
                ),
            ],
            edited: ids(&["d1"]),
            deleted: ids(&["f2"]),
            added: ids(&["f3"]),
        },
        Fixture {
            app: "slack",
            first: vec![
                on(
                    "SLACK_LIST_ALL_CHANNELS",
                    None,
                    json!({"ok": true, "channels": [
                    {"id": "C1", "name": "general", "is_member": true},
                    {"id": "C2", "name": "random", "is_member": true},
                    {"id": "C3", "name": "other", "is_member": false},
                ], "response_metadata": {"next_cursor": ""}}),
                ),
                on(
                    "SLACK_FETCH_CONVERSATION_HISTORY",
                    Some(json!({"channel": "C1"})),
                    json!({"ok": true, "messages": [
                    slack_msg(&s3, "Ship it"),
                    {"type": "message", "subtype": "channel_join", "user": "U2", "text": "joined", "ts": s2},
                    slack_msg(&s1, "Release on Friday"),
                ], "has_more": false}),
                ),
                on(
                    "SLACK_FETCH_CONVERSATION_HISTORY",
                    Some(json!({"channel": "C2"})),
                    json!({"ok": true, "messages": [
                        slack_msg(&s4, "Lunch?"),
                    ]}),
                ),
            ],
            first_ids: vec![format!("C1:{s1}"), format!("C1:{s3}"), format!("C2:{s4}")],
            second: vec![
                on(
                    "SLACK_LIST_ALL_CHANNELS",
                    None,
                    json!({"channels": [
                        {"id": "C1", "name": "general", "is_member": true}, {"id": "C2", "name": "random", "is_member": true},
                    ]}),
                ),
                on(
                    "SLACK_FETCH_CONVERSATION_HISTORY",
                    Some(json!({"channel": "C1"})),
                    json!({"messages": [
                        slack_msg(&s5, "New thread"),
                        {"type": "message", "user": "U024BE7LH", "text": "Release on Saturday", "ts": s1,
                         "edited": {"user": "U024BE7LH", "ts": slack_ts(ago(now, 4), 1)}},
                    ]}),
                ),
                on(
                    "SLACK_FETCH_CONVERSATION_HISTORY",
                    Some(json!({"channel": "C2"})),
                    json!({"messages": [
                        slack_msg(&s4, "Lunch?"),
                    ]}),
                ),
            ],
            edited: vec![format!("C1:{s1}")],
            deleted: vec![format!("C1:{s3}")],
            added: vec![format!("C1:{s5}")],
        },
        Fixture {
            app: "notion",
            first: vec![
                on(
                    "NOTION_SEARCH_NOTION_PAGE",
                    None,
                    json!({"object": "list", "results": [
                    notion_page("p1", "Roadmap", 120, false), notion_page("p2", "Old notes", 180, false),
                ], "next_cursor": null, "has_more": false}),
                ),
                on(
                    "NOTION_GET_PAGE_MARKDOWN",
                    Some(json!({"page_id": "p1"})),
                    json!({"markdown": "# Roadmap\nQ1"}),
                ),
                on(
                    "NOTION_GET_PAGE_MARKDOWN",
                    Some(json!({"page_id": "p2"})),
                    json!({"markdown": "notes"}),
                ),
            ],
            first_ids: ids(&["p1", "p2"]),
            second: vec![
                on(
                    "NOTION_SEARCH_NOTION_PAGE",
                    None,
                    json!({"results": [
                    notion_page("p3", "Hiring", 10, false), notion_page("p2", "Old notes", 30, true),
                    notion_page("p1", "Roadmap", 60, false), notion_page("p0", "Ancient", 600, false),
                ], "next_cursor": null, "has_more": false}),
                ),
                on(
                    "NOTION_GET_PAGE_MARKDOWN",
                    Some(json!({"page_id": "p1"})),
                    json!({"markdown": "# Roadmap\nQ1, Q2"}),
                ),
                on(
                    "NOTION_GET_PAGE_MARKDOWN",
                    Some(json!({"page_id": "p3"})),
                    json!({"markdown": "Hire two"}),
                ),
            ],
            edited: ids(&["p1"]),
            deleted: ids(&["p2"]),
            added: ids(&["p3"]),
        },
        Fixture {
            app: "outlook",
            first: vec![
                on(
                    "OUTLOOK_LIST_MESSAGES",
                    None,
                    json!({"value": [
                        outlook_msg("o1", "Contract", 20, false), outlook_msg("o4", "Agenda", 30, false),
                        outlook_msg("o9", "Draft", 25, true),
                    ]}),
                ),
                on(
                    "OUTLOOK_LIST_EVENTS",
                    None,
                    json!({"value": [
                        outlook_event("ev1", "Review", false), outlook_event("ev2", "1:1", false),
                    ]}),
                ),
            ],
            first_ids: ids(&["o1", "o4", "ev1", "ev2"]),
            second: vec![
                on(
                    "OUTLOOK_LIST_MESSAGES",
                    None,
                    json!({"value": [
                        outlook_msg("o1", "Contract v2", 20, false), outlook_msg("o3", "Hello", 2, false),
                    ]}),
                ),
                on(
                    "OUTLOOK_LIST_EVENTS",
                    None,
                    json!({"value": [
                        outlook_event("ev1", "Review", true), outlook_event("ev2", "1:1 (moved)", false),
                    ]}),
                ),
            ],
            edited: ids(&["o1", "ev2"]),
            deleted: ids(&["o4", "ev1"]),
            added: ids(&["o3"]),
        },
        Fixture {
            app: "microsoft_teams",
            first: vec![
                on(
                    "MICROSOFT_TEAMS_CHATS_GET_ALL_CHATS",
                    None,
                    json!({"value": [{"id": "19:a", "topic": "Project", "chatType": "group"}]}),
                ),
                on(
                    "MICROSOFT_TEAMS_LIST_USER_CHAT_MESSAGES",
                    Some(json!({"chat_id": "19:a"})),
                    json!({"value": [
                        teams_msg("t1", "Kickoff at 10", false, "message"), teams_msg("t4", "Typo", false, "message"),
                        teams_msg("t2", "added Bob", false, "systemEventMessage"),
                    ]}),
                ),
            ],
            first_ids: ids(&["19:a:t1", "19:a:t4"]),
            second: vec![
                on(
                    "MICROSOFT_TEAMS_CHATS_GET_ALL_CHATS",
                    None,
                    json!({"value": [{"id": "19:a", "topic": "Project"}]}),
                ),
                on(
                    "MICROSOFT_TEAMS_LIST_USER_CHAT_MESSAGES",
                    Some(json!({"chat_id": "19:a"})),
                    json!({"value": [
                        teams_msg("t1", "Kickoff at 11", false, "message"), teams_msg("t3", "Agenda attached", false, "message"),
                        teams_msg("t4", "", true, "message"),
                    ]}),
                ),
            ],
            edited: ids(&["19:a:t1"]),
            deleted: ids(&["19:a:t4"]),
            added: ids(&["19:a:t3"]),
        },
    ]
}

/// Answers from a script, records calls; no rate limits.
struct ScriptTools {
    script: Vec<Scripted>,
    garbage: Option<Value>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl ScriptTools {
    fn new(script: Vec<Scripted>) -> Self {
        Self {
            script,
            garbage: None,
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Tools for ScriptTools {
    async fn call(&self, tool: &'static str, args: Value) -> Result<Value, ComposioError> {
        self.calls.lock().unwrap().push((tool.into(), args.clone()));
        if let Some(g) = &self.garbage {
            return Ok(g.clone());
        }
        Ok(scripted(&self.script, tool, &args).unwrap_or(json!({})))
    }
}

fn check_pull(a: &dyn SyncAdapter, tools: &ScriptTools, pull: &super::Pull) {
    let app = a.toolkit();
    let calls = tools.calls.lock().unwrap();
    assert!(
        calls.len() as u32 <= a.max_calls(),
        "{app}: {} calls over its budget",
        calls.len()
    );
    for (tool, _) in calls.iter() {
        assert!(
            a.tools().contains(&tool.as_str()),
            "{app} called undeclared {tool}"
        );
    }
    let mut seen = BTreeSet::new();
    for i in &pull.items {
        assert_eq!(i.source, app, "{app}: item source");
        assert!(!i.external_id.is_empty(), "{app}: empty id");
        assert!(
            seen.insert(i.external_id.clone()),
            "{app}: duplicate {}",
            i.external_id
        );
        assert!(
            ["email", "event", "doc", "message"].contains(&i.kind),
            "{app}: kind {}",
            i.kind
        );
        assert!(i.body.len() <= MAX_BODY_CHARS, "{app}: body too long");
        assert!(i.metadata.is_object(), "{app}: metadata is an object");
    }
    assert!(
        pull.cursor.is_object(),
        "{app}: cursor is an object: {}",
        pull.cursor
    );
}

#[tokio::test]
async fn every_app_has_a_fixture() {
    let now = OffsetDateTime::now_utc();
    let have: BTreeSet<_> = fixtures(now).iter().map(|f| f.app).collect();
    for app in CATALOG.iter().filter(|a| a.syncs) {
        assert!(
            have.contains(app.slug),
            "{} has no conformance fixture",
            app.slug
        );
    }
}

#[tokio::test]
async fn pulls_conform() {
    let now = OffsetDateTime::now_utc();
    for f in fixtures(now) {
        let a = adapter(f.app).unwrap();
        let first = ScriptTools::new(f.first.clone());
        let p1 = a.pull(&first, &json!({}), now, 365).await.unwrap();
        check_pull(a, &first, &p1);
        let got: BTreeSet<_> = p1.items.iter().map(|i| i.external_id.clone()).collect();
        let want: BTreeSet<_> = f.first_ids.iter().cloned().collect();
        assert_eq!(got, want, "{}: first pull", f.app);

        let second = ScriptTools::new(f.second.clone());
        let later = now + Duration::minutes(1);
        let p2 = a.pull(&second, &p1.cursor, later, 365).await.unwrap();
        check_pull(a, &second, &p2);
        let c1 = first.calls.lock().unwrap().clone();
        let c2 = second.calls.lock().unwrap().clone();
        assert_ne!(c1, c2, "{}: the cursor changes what is asked", f.app);

        // Determinism: the same answers give the same pull.
        let again = ScriptTools::new(f.second.clone());
        assert_eq!(
            a.pull(&again, &p1.cursor, later, 365).await.unwrap(),
            p2,
            "{}",
            f.app
        );
    }
}

#[tokio::test]
async fn garbage_never_invents_items() {
    let now = OffsetDateTime::now_utc();
    let junk = [
        json!({}),
        Value::Null,
        json!("not json"),
        json!([]),
        json!({"data": {"messages": "x", "items": 5, "results": {"a": 1}, "value": null, "files": [1, "x"]}}),
        json!({"messages": [{"no": "id"}], "items": [{"summary": "no id"}], "results": [{}], "value": [7],
               "channels": [{"name": "no id"}], "files": [{"name": "no id"}], "changes": [{"removed": "yes"}]}),
    ];
    for app in CATALOG.iter().filter(|a| a.syncs) {
        let a = adapter(app.slug).unwrap();
        for g in &junk {
            let tools = ScriptTools {
                script: Vec::new(),
                garbage: Some(g.clone()),
                calls: Mutex::new(Vec::new()),
            };
            let p = a.pull(&tools, &json!({}), now, 365).await.unwrap();
            check_pull(a, &tools, &p);
            assert!(p.items.is_empty(), "{}: invented items from {g}", app.slug);
            assert!(
                p.deleted.is_empty(),
                "{}: invented deletes from {g}",
                app.slug
            );
            assert!(p.windows.iter().all(|w| w.seen.is_empty()), "{}", app.slug);
        }
    }
}

/// Through `Ingest::sync` into a real Postgres: edits, deletes, adds and dedup.
#[tokio::test]
async fn syncs_propagate_edits_and_deletes() {
    use std::sync::Arc;

    use crate::composio::Composio;
    use crate::gateway::Gateway;
    use crate::ingest::Ingest;
    use crate::testkit::{FakeComposio, subscriber, test_db, test_prices};

    let Some((db, pool)) = test_db().await else {
        return;
    };
    let now = OffsetDateTime::now_utc();
    let composio = Arc::new(FakeComposio::default());
    let mut ingest = Ingest::new(
        db.clone(),
        composio.clone() as Arc<dyn Composio>,
        Gateway::new(None, None, String::new(), String::new()),
        Arc::new(crate::plans::Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap()),
    );
    for f in fixtures(now) {
        let u = subscriber(&pool, "heed").await;
        let uid = u.to_string();
        let account = format!("ca_{}_{}", &uid[..8], f.app);
        db.upsert_connection(u, f.app, &account, "active")
            .await
            .unwrap();
        let conn = || async {
            db.connections(u)
                .await
                .unwrap()
                .into_iter()
                .find(|c| c.toolkit == f.app)
                .unwrap()
        };
        let stored = || async {
            let rows = pool
                .query(
                    "select external_id, content_hash from public.items where user_id = $1",
                    &[&u],
                )
                .await
                .unwrap();
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
                .collect::<std::collections::BTreeMap<_, _>>()
        };

        *composio.script.lock().unwrap() = f.first.clone();
        // Real syncs are at least minutes apart; start each with full buckets.
        ingest.limiter = super::RateLimiter::default();
        let r1 = ingest.sync(u, &conn().await).await;
        assert_eq!(r1.error, None, "{}: {r1:?}", f.app);
        let s1 = stored().await;
        assert_eq!(
            s1.keys().cloned().collect::<Vec<_>>(),
            f.first_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            "{}: first sync",
            f.app
        );
        let chunks: i64 = pool
            .query_one(
                "select count(*) from public.chunks where user_id = $1",
                &[&u],
            )
            .await
            .unwrap()
            .get(0);
        assert!(
            chunks >= s1.len() as i64,
            "{}: every item is searchable",
            f.app
        );

        *composio.script.lock().unwrap() = f.second.clone();
        // Real syncs are at least minutes apart; start each with full buckets.
        ingest.limiter = super::RateLimiter::default();
        let r2 = ingest.sync(u, &conn().await).await;
        assert_eq!(r2.error, None, "{}: {r2:?}", f.app);
        let s2 = stored().await;
        for id in &f.edited {
            assert_ne!(s1.get(id), s2.get(id), "{}: {id} edited", f.app);
            assert!(s2.contains_key(id), "{}: {id} kept", f.app);
        }
        for id in &f.deleted {
            assert!(!s2.contains_key(id), "{}: {id} deleted", f.app);
        }
        for id in &f.added {
            assert!(s2.contains_key(id), "{}: {id} added", f.app);
        }
        assert_eq!(r2.added as usize, f.added.len(), "{}: {r2:?}", f.app);
        assert!(r2.updated as usize >= f.edited.len(), "{}: {r2:?}", f.app);
        assert_eq!(r2.deleted as usize, f.deleted.len(), "{}: {r2:?}", f.app);

        // Unchanged answers: nothing is rewritten.
        // Real syncs are at least minutes apart; start each with full buckets.
        ingest.limiter = super::RateLimiter::default();
        let r3 = ingest.sync(u, &conn().await).await;
        assert_eq!((r3.added, r3.updated), (0, 0), "{}: dedup {r3:?}", f.app);
        assert_eq!(stored().await, s2, "{}: stable", f.app);
    }
    // Every call went through the right user's account, and was metered.
    let calls = composio.calls.lock().unwrap().len() as i64;
    let metered: i64 = pool
        .query_one(
            "select count(*) from public.usage_events where kind = 'tool'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(calls, metered);
}

/// A sync that hits a rate limit keeps its cursor and retries soon; nothing is lost.
#[tokio::test]
async fn rate_limited_sync_keeps_its_cursor() {
    use std::sync::Arc;

    use crate::composio::Composio;
    use crate::gateway::Gateway;
    use crate::ingest::Ingest;
    use crate::testkit::{FakeComposio, subscriber, test_db, test_prices};

    let Some((db, pool)) = test_db().await else {
        return;
    };
    let composio = Arc::new(FakeComposio::default());
    composio
        .fail
        .lock()
        .unwrap()
        .insert("SLACK_LIST_ALL_CHANNELS".into(), "429 ratelimited".into());
    let ingest = Ingest::new(
        db.clone(),
        composio.clone() as Arc<dyn Composio>,
        Gateway::new(None, None, String::new(), String::new()),
        Arc::new(crate::plans::Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap()),
    );
    let u = subscriber(&pool, "heed").await;
    let account = format!("ca_{}_slack", &u.to_string()[..8]);
    db.upsert_connection(u, "slack", &account, "active")
        .await
        .unwrap();
    let c = db.connections(u).await.unwrap().remove(0);
    let r = ingest.sync(u, &c).await;
    assert!(
        r.error.as_deref().unwrap().contains("rate limited"),
        "{r:?}"
    );
    let row = pool
        .query_one(
            "select sync_cursor, next_sync_at <= now() + interval '16 minutes' from public.connections where user_id = $1",
            &[&u],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Value>(0), json!({}), "cursor kept");
    assert!(row.get::<_, bool>(1), "retried within 15 minutes");
}
