//! Conformance (`nugget_connector::testkit`): crash resumption for each kind, a large history
//! with checkpoints, unicode everywhere, and reconcile deletes.

use std::fmt::Write as _;
use std::sync::Arc;

use nugget_connector::testkit::{Replica, UNICODE_SAMPLES, assert_crash_resumable};
use nugget_connector::{Change, Connector, Content, Document};
use nugget_connector_messaging::{CHECKPOINT_EVERY, register};
use serde_json::{Value, json};

use crate::common::*;

/// A seed index holding a stale item the pass must delete.
fn seed(key: &str) -> Arc<Replica> {
    let r = Replica::new();
    let mut d = Document::new(key, "text/markdown", Content::Text("old".into()));
    d.version = Some("v0".into());
    r.apply(&Change::Upsert(d));
    r
}

async fn crash_matrix(kind: Kind, settings: Value, stale: &str) {
    let env = Arc::new(Env::new());
    let report = assert_crash_resumable(
        |replica| {
            let env = env.clone();
            let settings = settings.clone();
            async move { env.build(kind, &settings, replica) as Arc<dyn Connector> }
        },
        None,
        seed(stale).snapshot(),
    )
    .await;
    assert!(report.sends > 2, "{report:?}");
}

#[tokio::test]
async fn whatsapp_is_crash_resumable() {
    crash_matrix(
        Kind::WhatsApp,
        json!({ "folder": fixtures("whatsapp"), "me": "Tanuj" }),
        "wa:gone/2020-01",
    )
    .await;
}

#[tokio::test]
async fn telegram_is_crash_resumable() {
    crash_matrix(
        Kind::Telegram,
        json!({ "path": fixtures("telegram") }),
        "tg:999/2020-01",
    )
    .await;
}

#[tokio::test]
async fn imessage_is_crash_resumable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::imessage::scenario(&tmp.path().join("Messages"));
    let path = db.path.clone();
    drop(db);
    crash_matrix(
        Kind::IMessage,
        json!({ "db_path": path, "contacts": false }),
        "im:gone/2020-01",
    )
    .await;
}

/// 5 000 messages over 100 months: 100 documents, checkpoints every 50 sends, and a crash
/// anywhere resumes to the same index.
#[tokio::test]
async fn large_history_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("big");
    let mut chat = String::new();
    for i in 0..5000u32 {
        let month = i / 50; // 100 months from Jan 2016
        let (y, m) = (2016 + month / 12, month % 12 + 1);
        let day = i % 50 / 2 + 1;
        let _ = writeln!(
            chat,
            "{day:02}/{m:02}/{y}, {:02}:{:02} - {}: message number {i}",
            i % 24,
            i % 60,
            if i % 3 == 0 { "Tanuj" } else { "Ivy" }
        );
    }
    write(&folder, "WhatsApp Chat with Ivy.txt", &chat);
    let settings = json!({ "folder": folder, "me": "Tanuj" });

    let env = Env::new();
    let replica = Replica::new();
    let sink = env.pass(Kind::WhatsApp, &settings, &replica).await;
    assert_eq!(replica.len(), 100);
    assert_eq!(sink.sends(), 100);
    // Two mid-pass checkpoints plus the final commit.
    assert_eq!(sink.checkpoint_calls(), 100 / CHECKPOINT_EVERY + 1);
    let total: u64 = sink
        .upserts()
        .iter()
        .map(|d| d.metadata["message_count"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 5000);
    assert!(keys(&replica).contains("wa:ivy/2024-04"));

    let report = assert_crash_resumable(
        |replica| {
            let env = &env;
            let settings = settings.clone();
            async move { env.build(Kind::WhatsApp, &settings, replica) }
        },
        None,
        Default::default(),
    )
    .await;
    assert_eq!(report.sends, 100);
    assert_eq!(report.checkpoints, 3);
}

#[tokio::test]
async fn unicode_names_and_text() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tg");
    let chats: Vec<Value> = UNICODE_SAMPLES
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "name": s, "type": "private_group", "id": 100 + i,
                "messages": [{
                    "id": 1, "type": "message", "date_unixtime": "1714554000",
                    "from": s, "from_id": format!("user{i}"),
                    "text": ["", {"type": "plain", "text": *s}, "\n---\n# x"]
                }]
            })
        })
        .collect();
    write(
        &dir,
        "result.json",
        &json!({ "personal_information": {"user_id": 1}, "chats": {"list": chats} }).to_string(),
    );
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::Telegram, &json!({ "path": dir }), &replica)
        .await;
    assert_eq!(replica.len(), UNICODE_SAMPLES.len());
    for d in sink.upserts() {
        assert_well_formed(&d);
        let t = text(&d);
        assert!(t.contains("> ---\n> # x"), "{t}");
    }
    // Texts survive (line separators split lines; BOMs and bidi marks only in names).
    let first = doc(&sink, "tg:100/2024-05");
    assert!(text(&first).contains("> Résumé — naïve café"));
    let flag = doc(&sink, "tg:101/2024-05");
    assert!(text(&flag).contains("👩‍👩‍👧‍👦 family 🇯🇵 flag"));
    assert_eq!(flag.metadata["conversation"], "👩‍👩‍👧‍👦 family 🇯🇵 flag");

    // Same samples in a WhatsApp export's text.
    let wa = tmp.path().join("wa");
    let mut chat = String::new();
    for (i, s) in UNICODE_SAMPLES.iter().enumerate() {
        let _ = writeln!(chat, "31/12/2023, 21:{i:02} - Uma: {s}");
    }
    write(&wa, "WhatsApp Chat with Uma.txt", &chat);
    let replica = Replica::new();
    let sink = env
        .pass(Kind::WhatsApp, &json!({ "folder": wa }), &replica)
        .await;
    let d = doc(&sink, "wa:uma/2023-12");
    assert_well_formed(&d);
    assert_eq!(d.metadata["message_count"], UNICODE_SAMPLES.len());
    assert!(text(&d).contains("> שלום עולם"));
}

#[test]
fn registry_has_the_three_kinds() {
    let mut r = nugget_connector::Registry::new();
    register(&mut r);
    let kinds: Vec<_> = r.kinds().iter().map(|k| k.kind).collect();
    assert_eq!(kinds, ["imessage", "telegram", "whatsapp"]);
    for k in r.kinds() {
        assert_eq!(k.auth, nugget_connector::AuthKind::None);
        assert_eq!(k.settings_schema["type"], "object");
        assert!(k.settings_schema["properties"]["private"].is_object());
    }
}

#[tokio::test]
async fn poll_and_watch_capabilities() {
    let env = Env::new();
    let wa = env.build(
        Kind::WhatsApp,
        &json!({ "folder": fixtures("whatsapp") }),
        Replica::new(),
    );
    assert!(wa.supports_watch());
    assert_eq!(wa.poll_interval().as_secs(), 300);
    let tg = env.build(
        Kind::Telegram,
        &json!({ "path": fixtures("telegram") }),
        Replica::new(),
    );
    assert!(tg.supports_watch());
    let im = env.build(
        Kind::IMessage,
        &json!({ "db_path": "/nonexistent/chat.db" }),
        Replica::new(),
    );
    assert!(!im.supports_watch());
    assert_eq!(im.poll_interval().as_secs(), 120);
}
