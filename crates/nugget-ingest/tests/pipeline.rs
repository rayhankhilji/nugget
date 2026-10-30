//! Pipeline semantics: ordering, checkpoints, version skip, deletes, privacy, triage, errors,
//! backpressure and metrics.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use nugget_connector::{Change, Content, Document};
use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::SourceId;
use nugget_ingest::IngestConfig;
use serde_json::json;

#[tokio::test]
async fn changes_apply_in_order_and_the_returned_cursor_is_committed_last() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "first version of a", Some("1"))),
        Step::Send(delete("a")),
        Step::Send(text_doc("a", "second version of a", Some("2"))),
        Step::Send(text_doc("b", "bravo body", Some("1"))),
        Step::Send(text_doc("c", "charlie body", Some("1"))),
        Step::Send(delete("c")),
        Step::Checkpoint(json!("c1")),
        Step::Return(json!("c2")),
    ]);
    h.add("fake-1").await;
    eventually("cursor c2", || h.cursor("fake-1") == Some(json!("c2"))).await;

    let a = h.item("fake-1", "a").expect("a exists");
    assert_eq!(a.text.as_deref(), Some("second version of a"));
    assert_eq!(a.version.as_deref(), Some("2"));
    assert!(h.item("fake-1", "b").is_some());
    assert!(h.item("fake-1", "c").is_none(), "delete after upsert wins");
    assert_eq!(h.store.search_text("charlie", 10).unwrap().len(), 0);
    assert_eq!(h.store.search_text("second", 10).unwrap().len(), 1);
    assert_eq!(
        *h.factory.state("fake-1").cursors_seen.lock().unwrap(),
        vec![None]
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn checkpoint_waits_for_prior_changes_and_a_crash_never_skips_data() {
    let embedder = FakeProvider::new("local-embed", Locality::OnDevice);
    embedder.block("BLOCK");
    let store = nugget_store::Store::open_in_memory().unwrap();
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let script = vec![
        Step::Send(text_doc("1", "one", Some("v"))),
        Step::Send(text_doc("2", "two", Some("v"))),
        Step::Send(text_doc("3", "three BLOCK", Some("v"))),
        Step::Send(text_doc("4", "four", Some("v"))),
        Step::Checkpoint(json!("c1")),
        Step::Return(json!("c2")),
    ];

    let h = start(Opts {
        router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
        store: Some(store.clone()),
        dir: Some(dir.clone()),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(script.clone());
    h.add("fake-1").await;

    // Item 3 is stuck in embedding; 1 and 2 are applied; the checkpoint behind 3 and 4 is not.
    eventually("item 3 reaches the embedder", || {
        embedder.embedded().iter().any(|t| t.contains("BLOCK"))
    })
    .await;
    assert!(h.item("fake-1", "1").is_some());
    assert!(h.item("fake-1", "2").is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.item("fake-1", "3").is_none());
    assert!(h.item("fake-1", "4").is_none());
    assert_eq!(
        h.cursor("fake-1"),
        None,
        "checkpoint must wait for item 3 and 4"
    );

    // Crash: stop everything while item 3 is in flight.
    tokio::time::timeout(Duration::from_secs(5), h.ingest.shutdown())
        .await
        .expect("shutdown is not blocked by a stuck provider");
    assert_eq!(h.cursor("fake-1"), None);
    assert!(h.item("fake-1", "3").is_none());

    // Restart on the same store and config: the pass is replayed from the last cursor (none).
    embedder.unblock();
    let factory = Arc::new(FakeFactory::default());
    let st = factory.state("fake-1");
    st.script(script);
    let h2 = start(Opts {
        router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
        store: Some(store.clone()),
        dir: Some(dir.clone()),
        factory: Some(factory),
        ..Default::default()
    })
    .await;
    eventually("cursor c2 after restart", || {
        h2.cursor("fake-1") == Some(json!("c2"))
    })
    .await;
    for k in ["1", "2", "3", "4"] {
        assert!(h2.item("fake-1", k).is_some(), "item {k}");
    }
    assert_eq!(*st.cursors_seen.lock().unwrap(), vec![None]);
    // Re-sent items 1 and 2 were version-skipped: embedded only once each.
    let one = embedder
        .embedded()
        .iter()
        .filter(|t| t.as_str() == "one")
        .count();
    assert_eq!(one, 1);
    h2.ingest.shutdown().await;
}

#[tokio::test]
async fn unchanged_versions_are_skipped() {
    let embedder = FakeProvider::new("local-embed", Locality::OnDevice);
    let h = start(Opts {
        router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Send(text_doc("a", "original text", Some("v1"))),
        Step::Return(json!(1)),
    ]);
    st.script(vec![
        // Same version, different content: skipped without reading the content.
        Step::Send(text_doc("a", "sneaky change", Some("v1"))),
        Step::Send(text_doc("b", "new item", Some("v1"))),
        Step::Return(json!(2)),
    ]);
    st.script(vec![
        Step::Send(text_doc("a", "real change", Some("v2"))),
        Step::Return(json!(3)),
    ]);
    h.add("fake-1").await;
    eventually("pass 1", || h.cursor("fake-1") == Some(json!(1))).await;
    let committed = h.ingest.committed();
    let mut commits = h.ingest.subscribe_committed();
    assert_eq!(*commits.borrow_and_update(), committed);

    h.ingest.sync_now(Some(&SourceId::new("fake-1"))).unwrap();
    // Commits wake watchers (knowledge extraction waits on this instead of polling).
    tokio::time::timeout(std::time::Duration::from_secs(10), commits.changed())
        .await
        .expect("a commit is signalled")
        .expect("ingest is running");
    eventually("pass 2", || h.cursor("fake-1") == Some(json!(2))).await;
    assert_eq!(
        h.item("fake-1", "a").unwrap().text.as_deref(),
        Some("original text")
    );
    assert!(!embedder.embedded().iter().any(|t| t.contains("sneaky")));
    assert_eq!(h.ingest.committed(), committed + 1, "only b was applied");

    h.ingest.sync_now(None).unwrap();
    eventually("pass 3", || h.cursor("fake-1") == Some(json!(3))).await;
    assert_eq!(
        h.item("fake-1", "a").unwrap().text.as_deref(),
        Some("real change")
    );
    assert_eq!(h.store.search_text("original", 10).unwrap().len(), 0);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn deletes_remove_items_and_chunks() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Send(text_doc("doomed", "zebra crossing", Some("1"))),
        Step::Send(text_doc("kept", "yak shaving", Some("1"))),
        Step::Return(json!(1)),
    ]);
    st.script(vec![
        Step::Send(delete("doomed")),
        Step::Send(delete("never-existed")),
        Step::Return(json!(2)),
    ]);
    h.add("fake-1").await;
    eventually("pass 1", || h.cursor("fake-1") == Some(json!(1))).await;
    assert_eq!(h.store.search_text("zebra", 10).unwrap().len(), 1);
    h.ingest.sync_now(None).unwrap();
    eventually("pass 2", || h.cursor("fake-1") == Some(json!(2))).await;
    assert!(h.item("fake-1", "doomed").is_none());
    assert!(h.item("fake-1", "kept").is_some());
    assert_eq!(h.store.search_text("zebra", 10).unwrap().len(), 0);
    let info = h.ingest.get(&SourceId::new("fake-1")).await.unwrap();
    assert_eq!(info.items, 1);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn sensitive_items_never_reach_cloud_providers() {
    let cloud = FakeProvider::new("cloud", Locality::Cloud);
    let local = FakeProvider::new("local", Locality::OnDevice);
    // The cloud provider is first for every tier: only the privacy rules keep data away.
    let r = router(&[
        (cloud.clone(), vec![Tier::Triage, Tier::Embedding]),
        (local.clone(), vec![Tier::Embedding]),
    ]);
    let h = start(Opts {
        cfg: IngestConfig {
            allow_cloud_embeddings: true,
            ..cfg()
        },
        router: Some(r),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc(
            "card",
            "Please charge 4111 1111 1111 1111 for the order",
            Some("1"),
        )),
        Step::Send(text_doc(
            "ssn",
            "employee ssn 123-45-6789 on file",
            Some("1"),
        )),
        Step::Send(text_doc(
            "key",
            "export OPENAI=sk-proj-abcdefABCDEF0123456789xyz",
            Some("1"),
        )),
        Step::Send(text_doc(
            "iban",
            "wire to DE89 3704 0044 0532 0130 00",
            Some("1"),
        )),
        Step::Send(text_doc("clean", "lunch menu for friday", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    // Cloud triage is never used, not even for clean items.
    assert!(
        cloud.chats().is_empty(),
        "cloud triage must never be called"
    );
    let cloud_seen = cloud.embedded().join("\n");
    for secret in ["4111", "123-45-6789", "sk-proj", "DE89"] {
        assert!(!cloud_seen.contains(secret), "{secret} reached the cloud");
    }
    assert!(
        cloud_seen.contains("lunch menu"),
        "clean items may use cloud embeddings"
    );
    let local_seen = local.embedded().join("\n");
    assert!(local_seen.contains("4111"), "sensitive items embed locally");

    for (key, field) in [
        ("card", "card"),
        ("ssn", "ssn"),
        ("key", "secret"),
        ("iban", "iban"),
    ] {
        let item = h.item("fake-1", key).unwrap();
        assert_eq!(item.metadata["sensitive"], json!(true), "{key}");
        assert_eq!(item.metadata["pii"][field], json!(1), "{key}");
        // Counts only: the metadata never holds the matched value.
        let meta = item.metadata.to_string();
        assert!(!meta.contains("4111") && !meta.contains("6789"), "{meta}");
    }
    let clean = h.item("fake-1", "clean").unwrap();
    assert_eq!(clean.metadata["sensitive"], json!(false));
    assert!(clean.metadata.get("pii").is_none());
    assert_eq!(clean.metadata["embedding_provider"], json!("cloud"));
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn cloud_embeddings_are_off_by_default() {
    let cloud = FakeProvider::new("cloud", Locality::Cloud);
    let h = start(Opts {
        router: Some(router(&[(
            cloud.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("clean", "nothing secret here", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    assert!(cloud.embedded().is_empty());
    assert!(cloud.chats().is_empty());
    // Still searchable, text-only.
    assert_eq!(h.store.search_text("secret", 10).unwrap().len(), 1);
    let status = h.ingest.status().await.unwrap();
    assert_eq!(status.embedding_provider, None);
    assert_eq!(status.triage_provider, None);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn local_triage_marks_junk_and_sensitive_items() {
    let local = FakeProvider::new("lan", Locality::LocalNetwork);
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("spam", "WIN BIG SPAM offer click now", Some("1"))),
        Step::Send(text_doc("memo", "CONFIDENTIAL merger memo", Some("1"))),
        Step::Send(text_doc("note", "grocery list: eggs, milk", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    let spam = h
        .item("fake-1", "spam")
        .expect("junk is stored metadata-only");
    assert_eq!(spam.metadata["junk"], json!(true));
    assert_eq!(spam.metadata["triage"]["junk"], json!(true));
    assert_eq!(spam.text, None);
    assert_eq!(
        h.store.search_text("offer", 10).unwrap().len(),
        0,
        "junk is not chunked"
    );
    assert!(!local.embedded().iter().any(|t| t.contains("SPAM")));

    let memo = h.item("fake-1", "memo").unwrap();
    assert_eq!(memo.metadata["sensitive"], json!(true));
    assert_eq!(memo.metadata["junk"], json!(false));

    let note = h.item("fake-1", "note").unwrap();
    assert_eq!(
        note.metadata["triage"]["summary"],
        json!("A short summary of the document.")
    );
    assert_eq!(h.store.search_text("eggs", 10).unwrap().len(), 1);
    assert_eq!(local.chats().len(), 3);
    let status = h.ingest.status().await.unwrap();
    assert_eq!(
        status.triage_provider.as_ref().map(|p| p.as_str()),
        Some("lan")
    );
    assert_eq!(
        status.embedding_provider.as_ref().map(|p| p.as_str()),
        Some("lan")
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn bad_items_are_recorded_and_never_stop_the_source() {
    let h = start(Opts::default()).await;
    let mut unsupported = Document::new(
        "blob.bin",
        "application/x-unknown",
        Content::Bytes(vec![0, 1, 2, 3]),
    );
    unsupported.title = Some("Mystery blob".into());
    unsupported.version = Some("1".into());
    h.factory.state("fake-1").script(vec![
        // Rejected by the store (empty key): recorded, skipped.
        Step::Send(text_doc("", "no key", Some("1"))),
        // Not extractable: indexed by title only, error in metadata.
        Step::Send(Change::Upsert(unsupported)),
        Step::Send(text_doc("after", "still indexed afterwards", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    assert!(h.item("fake-1", "after").is_some());
    let blob = h.item("fake-1", "blob.bin").unwrap();
    assert!(
        blob.metadata["extract_error"]
            .as_str()
            .unwrap()
            .contains("extract")
    );
    assert_eq!(
        h.store.search_text("Mystery", 10).unwrap().len(),
        1,
        "title chunk"
    );
    let errors = h.ingest.errors(&SourceId::new("fake-1")).unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("store rejected"));
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn extraction_timeout_indexes_the_title_only() {
    let h = start(Opts {
        cfg: IngestConfig {
            // Far less than extracting and chunking a few megabytes takes.
            extract_timeout_ms: 1,
            ..cfg()
        },
        ..Default::default()
    })
    .await;
    let big = "lorem ipsum dolor sit amet, consectetur adipiscing elit.\n\n".repeat(60_000);
    let mut d = Document::new("big.md", "text/markdown", Content::Text(big));
    d.title = Some("Pipe dream".into());
    d.version = Some("1".into());
    h.factory.state("fake-1").script(vec![
        Step::Send(Change::Upsert(d)),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    let item = h.item("fake-1", "big.md").unwrap();
    assert!(
        item.metadata["extract_error"]
            .as_str()
            .unwrap()
            .contains("timed out"),
        "{}",
        item.metadata
    );
    assert_eq!(item.title.as_deref(), Some("Pipe dream"));
    assert_eq!(h.store.search_text("dream", 10).unwrap().len(), 1);
    assert!(h.store.search_text("lorem", 10).unwrap().is_empty());
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn send_applies_backpressure_when_the_queue_is_full() {
    let embedder = FakeProvider::new("local", Locality::OnDevice);
    embedder.block("");
    let h = start(Opts {
        cfg: IngestConfig {
            queue_capacity: 2,
            ..cfg()
        },
        router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    let mut steps: Vec<Step> = (0..10)
        .map(|i| Step::Send(text_doc(&format!("k{i}"), &format!("item {i}"), Some("1"))))
        .collect();
    steps.push(Step::Return(json!("done")));
    st.script(steps);
    h.add("fake-1").await;

    eventually("worker blocked on first item", || {
        !embedder.embedded().is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // One item in the worker, two in the queue; the connector is parked in `send`.
    let sent = st.sent.load(Ordering::SeqCst);
    assert!(
        sent <= 3,
        "send must block when the queue is full (sent {sent})"
    );
    let status = h.ingest.status().await.unwrap();
    assert!(
        status.queue_depth >= 2,
        "queue depth {}",
        status.queue_depth
    );

    embedder.unblock();
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    assert_eq!(st.sent.load(Ordering::SeqCst), 10);
    assert_eq!(h.ingest.status().await.unwrap().queue_depth, 0);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn freshness_is_measured_for_sync_and_live_changes() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.watch.store(true, Ordering::SeqCst);
    st.script(vec![
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!("c")),
    ]);
    let empty = h.ingest.status().await.unwrap();
    assert_eq!(empty.freshness_p50_ms, None);

    h.add("fake-1").await;
    h.wait_state("fake-1", nugget_core::ipc::SourceState::Watching)
        .await;
    st.live(text_doc("live", "live wire change", Some("1")));
    eventually("live change searchable", || {
        h.store.search_text("wire", 10).unwrap().len() == 1
    })
    .await;
    st.live(delete("a"));
    eventually("live delete", || h.item("fake-1", "a").is_none()).await;

    let status = h.ingest.status().await.unwrap();
    let p50 = status.freshness_p50_ms.expect("p50");
    let p95 = status.freshness_p95_ms.expect("p95");
    assert!(p50 <= p95);
    assert!(p95 < 5_000, "p95 {p95} ms");
    assert_eq!(status.queue_depth, 0);
    assert_eq!(h.ingest.committed(), 3);
    assert_eq!(status.sources.len(), 1);
    assert_eq!(status.sources[0].items, 1);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn files_changed_since_the_scan_are_skipped_not_stored() {
    let h = start(Opts::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.md");
    std::fs::write(&path, "first draft about penguins").unwrap();
    let scanned = nugget_extract::FileExpect::of(&std::fs::metadata(&path).unwrap());
    // The file changes after the connector scanned it.
    std::fs::write(&path, "a much longer second draft about walruses").unwrap();

    let mut stale = Document::new("note.md", "text/markdown", Content::File(path.clone()));
    stale.version = Some("v1".into());
    let check = |e: nugget_extract::FileExpect| {
        let mut m = serde_json::Map::new();
        m.insert(nugget_extract::FILE_CHECK_KEY.to_owned(), e.to_metadata());
        serde_json::Value::Object(m)
    };
    stale.metadata = check(scanned);
    let mut fresh = Document::new("fresh.md", "text/markdown", Content::File(path.clone()));
    fresh.version = Some("v2".into());
    fresh.metadata = check(nugget_extract::FileExpect::of(
        &std::fs::metadata(&path).unwrap(),
    ));
    h.factory.state("fake-1").script(vec![
        Step::Send(Change::Upsert(stale)),
        Step::Send(Change::Upsert(fresh)),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    // Nothing stored for the stale scan (no title-only item, no version), so the next pass
    // resends it; the item whose scan still matches is indexed normally.
    assert!(h.item("fake-1", "note.md").is_none());
    assert!(h.item("fake-1", "fresh.md").is_some());
    assert_eq!(h.store.search_text("walruses", 10).unwrap().len(), 1);
    let errors = h.ingest.errors(&SourceId::new("fake-1")).unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].message.contains("skipped until the next pass"),
        "{errors:?}"
    );
    h.ingest.shutdown().await;
}

/// A fresh index adopts the embedder's task prefixes and embeds documents with them; an index
/// that already holds vectors embedded without prefixes keeps embedding without them, so its
/// stored vectors and new ones (and the retriever's queries) stay on one scheme.
#[tokio::test]
async fn documents_follow_the_index_prefix_scheme() {
    let nomic = ("search_query: ".to_owned(), "search_document: ".to_owned());
    for legacy in [false, true] {
        let embedder = FakeProvider::new("local-embed", Locality::OnDevice);
        *embedder.prefixes.lock().unwrap() = nomic.clone();
        let store = nugget_store::Store::open_in_memory().unwrap();
        if legacy {
            let it = nugget_store::NewItem::new("old", "x", nugget_store::content_hash("x"))
                .with_text("x");
            store
                .upsert_item_with_chunks(
                    &it,
                    &[nugget_store::NewChunk::embedded("x", vec![1.0; 4])],
                )
                .unwrap();
        }
        let h = start(Opts {
            router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
            store: Some(store.clone()),
            ..Default::default()
        })
        .await;
        h.factory.state("fake-1").script(vec![
            Step::Send(text_doc("1", "hello world", Some("v"))),
            Step::Return(json!(1)),
        ]);
        h.add("fake-1").await;
        eventually("item 1 indexed", || h.item("fake-1", "1").is_some()).await;
        let sent = embedder.embedded();
        assert!(!sent.is_empty());
        if legacy {
            assert!(sent.iter().all(|t| !t.starts_with("search_")), "{sent:?}");
            assert_eq!(
                store.embedding_prefixes().unwrap(),
                Some((String::new(), String::new()))
            );
        } else {
            assert!(
                sent.iter().all(|t| t.starts_with("search_document: ")),
                "{sent:?}"
            );
            assert_eq!(store.embedding_prefixes().unwrap(), Some(nomic.clone()));
        }
        h.ingest.shutdown().await;
    }
}
