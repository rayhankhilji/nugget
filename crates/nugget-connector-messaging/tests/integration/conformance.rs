//! Conformance (`nugget_connector::testkit`): crash resumption for each kind, reconcile deletes
//! and checkpoints over a large history.

use std::fmt::Write as _;
use std::sync::Arc;

use nugget_connector::testkit::{Replica, assert_crash_resumable};
use nugget_connector::{Change, Connector, Content, Document};
use nugget_connector_messaging::CHECKPOINT_EVERY;
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
async fn every_kind_is_crash_resumable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::imessage::scenario(&tmp.path().join("Messages"));
    let path = db.path.clone();
    drop(db);
    let cases = [
        (
            Kind::WhatsApp,
            json!({ "folder": fixtures("whatsapp"), "me": "Tanuj" }),
            "wa:gone/2020-01",
        ),
        (
            Kind::Telegram,
            json!({ "path": fixtures("telegram") }),
            "tg:999/2020-01",
        ),
        (
            Kind::IMessage,
            json!({ "db_path": path, "contacts": false }),
            "im:gone/2020-01",
        ),
    ];
    for (kind, settings, stale) in cases {
        crash_matrix(kind, settings, stale).await;
    }
}

/// 5 000 messages over 100 months: 100 documents, checkpoints every 50 sends.
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
}
