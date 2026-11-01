//! Live watching: a new export is imported, a removed one deleted. Every wait is bounded and
//! polls for the condition (no sleeps as assertions).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_connector::testkit::{Replica, ReplicaSink};
use nugget_connector::{CancellationToken, ChangeSink};
use nugget_core::Error;
use serde_json::json;

use crate::common::*;

/// Poll `replica` until `pred` holds, calling `poke` every so often (re-touching a file covers
/// a watcher that was not registered yet when the first write happened).
async fn wait_until(
    what: &str,
    replica: &Replica,
    pred: impl Fn(&std::collections::BTreeSet<String>) -> bool,
    poke: impl Fn(u32),
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut n = 0u32;
    loop {
        poke(n);
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until {
            if pred(&keys(replica)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        n += 1;
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; have {:?}",
            keys(replica)
        );
    }
}

fn chat(extra: u32) -> String {
    let mut s = "31/12/2023, 21:15 - Nia: hello from a new export\n".to_owned();
    for i in 0..extra {
        s.push_str(&format!("31/12/2023, 21:16 - Nia: again {i}\n"));
    }
    s
}

async fn run_watch(
    kind: Kind,
    settings: serde_json::Value,
    dir: &Path,
    new_file: &str,
    key: &str,
    body: impl Fn(u32) -> String,
) {
    let env = Env::new();
    let replica = Replica::new();
    env.pass(kind, &settings, &replica).await;
    let before = keys(&replica);

    let conn = env.build(kind, &settings, replica.clone());
    let sink: Arc<dyn ChangeSink> = Arc::new(ReplicaSink::new(replica.clone()));
    let cancel = CancellationToken::new();
    let task = {
        let (conn, cancel) = (conn.clone(), cancel.clone());
        tokio::spawn(async move { conn.watch(sink, cancel).await })
    };

    let path = dir.join(new_file);
    wait_until(
        "the new export",
        &replica,
        |k| k.contains(key),
        |n| {
            write(dir, new_file, &body(n));
        },
    )
    .await;

    std::fs::remove_file(&path).unwrap();
    wait_until("its removal", &replica, |k| *k == before, |_| {}).await;

    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("watch stops promptly")
        .unwrap();
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
}

#[tokio::test]
async fn whatsapp_watch_picks_up_new_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("exports");
    copy_dir(&fixtures("whatsapp"), &dir);
    run_watch(
        Kind::WhatsApp,
        json!({ "folder": dir }),
        &dir,
        "WhatsApp Chat with Nia.txt",
        "wa:nia/2023-12",
        chat,
    )
    .await;
}

#[tokio::test]
async fn telegram_watch_picks_up_new_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tg");
    copy_dir(&fixtures("telegram"), &dir);
    let sub = dir.join("ChatExport_new");
    std::fs::create_dir_all(&sub).unwrap();
    run_watch(
        Kind::Telegram,
        json!({ "path": dir }),
        &sub,
        "result.json",
        "tg:555/2024-05",
        |n| {
            json!({
                "name": "Nia", "type": "personal_chat", "id": 555,
                "messages": [{"id": 1, "type": "message", "date_unixtime": "1714554000",
                              "from": "Nia", "from_id": "user555", "text": format!("hi {n}")}]
            })
            .to_string()
        },
    )
    .await;
}

/// Regression (JJ1-5): the live path reports an unreadable export too.
#[tokio::test]
async fn whatsapp_watch_reports_unreadable_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("exports");
    write(
        &dir,
        "WhatsApp Chat with Kai.txt",
        "2026年3月14日 9:06 - Kai: hi\n",
    );
    let env = Env::new();
    let replica = Replica::new();
    let settings = json!({ "folder": dir });
    let conn = env.build(Kind::WhatsApp, &settings, replica.clone());
    let sink = Arc::new(ReplicaSink::new(replica.clone()));
    let cancel = CancellationToken::new();
    let task = {
        let (conn, cancel) = (conn.clone(), cancel.clone());
        let sink: Arc<dyn ChangeSink> = sink.clone();
        tokio::spawn(async move { conn.watch(sink, cancel).await })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while sink.problems.lock().unwrap().is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the report"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        sink.problems.lock().unwrap()[0].0,
        "whatsapp:WhatsApp Chat with Kai.txt"
    );
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("watch stops promptly")
        .unwrap();
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
}
