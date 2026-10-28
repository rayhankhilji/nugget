//! Source registry: add / update / enable / remove, persistence and secret cleanup.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use nugget_connector::SecretStore;
use nugget_core::ids::SourceId;
use nugget_core::ipc::{SourceSpec, SourceState, SourceUpdate};
use nugget_ingest::IngestEvent;
use serde_json::json;

fn spec(kind: &str, settings: serde_json::Value) -> SourceSpec {
    SourceSpec {
        id: None,
        kind: kind.into(),
        name: "  ".into(),
        settings,
    }
}

#[tokio::test]
async fn add_generates_ids_validates_and_persists() {
    let h = start(Opts::default()).await;
    let a = h
        .ingest
        .add(spec("fake", json!({"root": "/a"})))
        .await
        .unwrap();
    let b = h
        .ingest
        .add(spec("fake", serde_json::Value::Null))
        .await
        .unwrap();
    assert_eq!(a.id.as_str(), "fake-1");
    assert_eq!(b.id.as_str(), "fake-2");
    assert_eq!(
        a.name, "Fake source",
        "blank names fall back to the kind's display name"
    );
    assert!(a.enabled);
    assert_eq!(b.settings, json!({}));

    // Rejections: unknown kind, factory validation, secrets in settings, bad or duplicate id.
    assert!(h.ingest.add(spec("nope", json!({}))).await.is_err());
    assert!(
        h.ingest
            .add(spec("fake", json!({"bad": true})))
            .await
            .is_err()
    );
    assert!(
        h.ingest
            .add(spec("fake", json!({"imap": {"password": "hunter2"}})))
            .await
            .is_err()
    );
    let mut dup = spec("fake", json!({}));
    dup.id = Some(SourceId::new("fake-1"));
    assert!(h.ingest.add(dup).await.is_err());
    let mut bad = spec("fake", json!({}));
    bad.id = Some(SourceId::new("has:colon"));
    assert!(h.ingest.add(bad).await.is_err());

    let text = std::fs::read_to_string(h.file_path()).unwrap();
    assert!(text.contains("fake-1") && text.contains("fake-2"));
    assert!(!text.contains("hunter2"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(h.file_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(h.ingest.list().await.unwrap().len(), 2);
    assert_eq!(h.ingest.kinds()[0].kind, "fake");
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn generated_ids_skip_ids_that_still_have_data() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "leftover", Some("1"))),
        Step::Return(json!(1)),
    ]);
    let first = h.ingest.add(spec("fake", json!({}))).await.unwrap();
    eventually("indexed", || h.cursor("fake-1") == Some(json!(1))).await;
    h.ingest.remove(&first.id, false).await.unwrap();
    // fake-1's items were kept, so the id is not reused.
    let next = h.ingest.add(spec("fake", json!({}))).await.unwrap();
    assert_eq!(next.id.as_str(), "fake-2");
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn sources_survive_a_restart_with_their_enabled_state() {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let store = nugget_store::Store::open_in_memory().unwrap();
    let h = start(Opts {
        dir: Some(dir.clone()),
        store: Some(store.clone()),
        ..Default::default()
    })
    .await;
    h.add("fake-1").await;
    h.add("fake-2").await;
    h.ingest
        .set_enabled(&SourceId::new("fake-2"), false)
        .await
        .unwrap();
    h.ingest.shutdown().await;

    let factory = Arc::new(FakeFactory::default());
    let h2 = start(Opts {
        dir: Some(dir),
        store: Some(store),
        factory: Some(factory.clone()),
        ..Default::default()
    })
    .await;
    let list = h2.ingest.list().await.unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].enabled && !list[1].enabled);
    assert_eq!(list[1].state, SourceState::Disabled);
    eventually("enabled source syncs", || {
        factory.state("fake-1").calls() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        factory.state("fake-2").calls(),
        0,
        "disabled source stays idle"
    );
    h2.ingest.shutdown().await;
}

#[tokio::test]
async fn disable_stops_and_enable_restarts() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.watch.store(true, std::sync::atomic::Ordering::SeqCst);
    h.add("fake-1").await;
    h.wait_state("fake-1", SourceState::Watching).await;

    let info = h
        .ingest
        .set_enabled(&SourceId::new("fake-1"), false)
        .await
        .unwrap();
    assert_eq!(info.state, SourceState::Disabled);
    assert!(!info.enabled);
    assert_eq!(
        st.watch_cancelled.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(h.ingest.sync_now(Some(&SourceId::new("fake-1"))).is_err());
    let calls = st.calls();

    h.ingest
        .set_enabled(&SourceId::new("fake-1"), true)
        .await
        .unwrap();
    eventually("restarted", || st.calls() > calls).await;
    h.wait_state("fake-1", SourceState::Watching).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn settings_changes_are_validated_and_reset_the_cursor() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Return(json!("c1"))]);
    h.add("fake-1").await;
    eventually("first pass", || h.cursor("fake-1") == Some(json!("c1"))).await;
    h.wait_state("fake-1", SourceState::Idle).await;

    let id = SourceId::new("fake-1");
    let bad = SourceUpdate {
        id: id.clone(),
        name: None,
        enabled: None,
        settings: Some(json!({"bad": true})),
    };
    assert!(h.ingest.update(bad).await.is_err());
    assert!(
        h.ingest
            .update(SourceUpdate {
                id: id.clone(),
                name: None,
                enabled: None,
                settings: Some(json!({"token": "abc"})),
            })
            .await
            .is_err()
    );

    st.script(vec![Step::Return(json!("c2"))]);
    let info = h
        .ingest
        .update(SourceUpdate {
            id: id.clone(),
            name: Some("Renamed".into()),
            enabled: None,
            settings: Some(json!({"root": "/new"})),
        })
        .await
        .unwrap();
    assert_eq!(info.name, "Renamed");
    assert_eq!(info.settings, json!({"root": "/new"}));
    eventually("second pass", || h.cursor("fake-1") == Some(json!("c2"))).await;
    // The connector was rebuilt and started over without a cursor.
    assert_eq!(*st.cursors_seen.lock().unwrap(), vec![None, None]);
    assert_eq!(h.factory.contexts.lock().unwrap().len(), 2);

    // A rename alone does not restart anything.
    h.ingest
        .update(SourceUpdate {
            id: id.clone(),
            name: Some("Again".into()),
            enabled: None,
            settings: None,
        })
        .await
        .unwrap();
    assert_eq!(h.factory.contexts.lock().unwrap().len(), 2);
    assert!(
        std::fs::read_to_string(h.file_path())
            .unwrap()
            .contains("Again")
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn remove_with_purge_deletes_data_cursor_and_secrets() {
    let h = start(Opts::default()).await;
    let mut rx = h.ingest.subscribe();
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::SetSecret("oauth:fake-1:client".into(), "client-secret".into()),
        Step::Send(text_doc("a", "purge me", Some("1"))),
        Step::Return(json!("c")),
    ]);
    h.add("fake-1").await;
    h.add("fake-2").await;
    eventually("indexed", || h.cursor("fake-1") == Some(json!("c"))).await;
    h.ingest
        .set_password(&SourceId::new("fake-1"), "pw")
        .await
        .unwrap();
    h.ingest
        .set_password(&SourceId::new("fake-2"), "other")
        .await
        .unwrap();
    assert_eq!(
        h.ingest.get(&SourceId::new("fake-1")).await.unwrap().items,
        1
    );

    h.ingest
        .remove(&SourceId::new("fake-1"), true)
        .await
        .unwrap();

    assert!(h.item("fake-1", "a").is_none());
    assert_eq!(h.cursor("fake-1"), None);
    assert_eq!(h.store.search_text("purge", 10).unwrap().len(), 0);
    assert_eq!(h.secrets.accounts(), vec!["password:fake-2".to_owned()]);
    assert_eq!(
        h.secrets.get("password:fake-2").unwrap().as_deref(),
        Some("other")
    );
    let list = h.ingest.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id.as_str(), "fake-2");
    assert!(
        !std::fs::read_to_string(h.file_path())
            .unwrap()
            .contains("fake-1")
    );
    assert!(
        h.ingest
            .remove(&SourceId::new("fake-1"), true)
            .await
            .is_err()
    );

    let mut removed = false;
    while let Ok(ev) = rx.try_recv() {
        if ev == IngestEvent::SourceRemoved(SourceId::new("fake-1")) {
            removed = true;
        }
    }
    assert!(removed);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn remove_without_purge_keeps_indexed_data() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "keep me", Some("1"))),
        Step::Return(json!("c")),
    ]);
    h.add("fake-1").await;
    eventually("indexed", || h.cursor("fake-1") == Some(json!("c"))).await;
    h.ingest
        .remove(&SourceId::new("fake-1"), false)
        .await
        .unwrap();
    assert!(h.item("fake-1", "a").is_some());
    assert!(h.ingest.list().await.unwrap().is_empty());
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn unknown_kinds_in_the_file_are_reported_not_fatal() {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let cfg_dir = dir.path().join("config");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join(nugget_ingest::SOURCES_FILE),
        "version = 1\n[[sources]]\nid = 'old-1'\nkind = 'retired'\nname = 'Old'\n\n[[sources]]\nid = 'fake-1'\nkind = 'fake'\nname = 'F'\n[sources.settings]\nroot = '/x'\n",
    )
    .unwrap();
    let factory = Arc::new(FakeFactory::default());
    let h = start(Opts {
        dir: Some(dir),
        factory: Some(factory.clone()),
        ..Default::default()
    })
    .await;
    let old = h.ingest.get(&SourceId::new("old-1")).await.unwrap();
    assert_eq!(old.state, SourceState::Error);
    assert!(old.last_error.unwrap().contains("retired"));
    eventually("valid source runs", || factory.state("fake-1").calls() >= 1).await;
    let f = h.ingest.get(&SourceId::new("fake-1")).await.unwrap();
    assert_eq!(f.settings, json!({"root": "/x"}));
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn a_corrupt_sources_file_fails_start() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("config");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join(nugget_ingest::SOURCES_FILE), "[[[").unwrap();
    let r = nugget_ingest::Ingest::builder(
        cfg(),
        nugget_store::Store::open_in_memory().unwrap(),
        nugget_connector::Registry::new(),
        nugget_core::paths::Paths::rooted(dir.path().to_path_buf()),
    )
    .secrets(Arc::new(nugget_ingest::MemorySecrets::new()))
    .start()
    .await;
    assert!(r.is_err());
}
