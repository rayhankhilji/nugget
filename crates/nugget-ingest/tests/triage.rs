//! Background triage (QA GG2-6, GG2-7): items are searchable before triage runs, triage labels
//! never remove an item from search, small items share a request, triage yields to interactive
//! model use, and the sensitivity rules fail closed.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_core::ipc::TriageState;
use nugget_infer::Provider;
use nugget_ingest::{Foreground, IngestConfig};
use serde_json::json;

fn local_model() -> Arc<FakeProvider> {
    FakeProvider::new("local-chat", Locality::OnDevice)
}

async fn triage_settled(h: &Harness) {
    eventually_async("triage settled", || {
        let ingest = h.ingest.clone();
        async move {
            let t = ingest.status().await.unwrap().triage.unwrap();
            t.pending == 0
        }
    })
    .await;
}

/// GG2-6: items are searchable (and sensitive) before triage; GG2-7: labels never hide them.
#[tokio::test]
async fn items_are_searchable_before_triage_and_labels_never_remove_them() {
    let local = local_model();
    local.block_chat();
    let h = start(Opts {
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    let docs = [
        ("spam", "WIN BIG SPAM offer click now"),
        ("memo", "HUSHHUSH merger memo"),
        ("note", "the boiler engineer visits on Tuesday"),
    ];
    let steps = docs
        .iter()
        .map(|(k, t)| Step::Send(text_doc(k, t, Some("1"))));
    h.run("fake-1", steps.collect(), json!("done")).await;
    for (key, word) in [("spam", "offer"), ("memo", "merger"), ("note", "boiler")] {
        assert_eq!(h.store.search_text(word, 10).unwrap().len(), 1);
        let m = h.item("fake-1", key).unwrap().metadata;
        assert_eq!(m["triage"]["state"], json!("pending"));
        assert_eq!(m["sensitive"], json!(true), "fail closed");
    }
    local.unblock_chat();
    triage_settled(&h).await;
    for (key, junk, sensitive) in [
        ("spam", true, false),
        ("memo", false, true),
        ("note", false, false),
    ] {
        let it = h.item("fake-1", key).unwrap();
        assert_eq!(it.metadata["triage"]["state"], json!("done"), "{key}");
        assert_eq!(it.metadata["junk"], json!(junk), "{key}");
        assert_eq!(it.metadata["sensitive"], json!(sensitive), "{key}");
        assert!(it.text.is_some());
    }
    assert_eq!(h.store.search_text("offer", 10).unwrap().len(), 1);
    assert!(
        h.store
            .items_missing_embeddings(None, 10)
            .unwrap()
            .is_empty()
    );
    // Three small items share requests (normally one; a slow machine may split the gather).
    assert!(local.chats().len() < 3, "{:?}", local.chats());
    let status = h.ingest.status().await.unwrap();
    let t = status.triage.unwrap();
    assert_eq!(
        (t.pending, t.done, t.failed, t.state),
        (0, 3, 0, TriageState::Idle)
    );
    assert_eq!(status.triage_provider.unwrap().as_str(), "local-chat");
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn rule_hits_stay_sensitive_and_unchanged_content_keeps_its_labels() {
    let local = local_model();
    let h = start(Opts {
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![
            Step::Send(text_doc(
                "card",
                "SPAM charge 4111 1111 1111 1111 please",
                Some("1"),
            )),
            Step::Send(text_doc("note", "SPAM weekly deals", Some("1"))),
        ],
        json!("p1"),
    )
    .await;
    triage_settled(&h).await;
    // A PII hit is still triaged for the junk label, and stays sensitive whatever the
    // model says (it said not sensitive).
    let card = h.item("fake-1", "card").unwrap();
    assert_eq!(card.metadata["triage"]["state"], json!("done"));
    assert_eq!(card.metadata["triage"]["sensitive"], json!(false));
    assert_eq!(card.metadata["junk"], json!(true));
    assert_eq!(card.metadata["sensitive"], json!(true));
    assert_eq!(card.metadata["sensitive_rule"], json!("pii"));
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!(t.done, 2);
    let calls = local.chats().len();

    // Same content under a new version (e.g. a touched file): no new model call, same labels.
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("note", "SPAM weekly deals", Some("2"))),
        Step::Return(json!("p2")),
    ]);
    h.ingest.sync_now(None).unwrap();
    eventually("pass 2", || h.cursor("fake-1") == Some(json!("p2"))).await;
    triage_settled(&h).await;
    let note = h.item("fake-1", "note").unwrap();
    assert_eq!(note.version.as_deref(), Some("2"));
    assert_eq!(note.metadata["junk"], json!(true));
    assert_eq!(note.metadata["triage"]["state"], json!("done"));
    assert_eq!(local.chats().len(), calls);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn triage_waits_for_interactive_requests_and_abandons_its_own_when_one_starts() {
    let local = local_model();
    let fg = Arc::new(Foreground::new(Duration::from_millis(50)));
    let h = start(Opts {
        router: local_router(&local),
        foreground: Some(fg.clone()),
        ..Default::default()
    })
    .await;

    // An Ask is in flight: triage does not touch the model.
    let ask = fg.begin();
    h.run(
        "fake-1",
        vec![Step::Send(text_doc("a", "first note", Some("1")))],
        json!("p1"),
    )
    .await;
    assert_eq!(h.store.search_text("first", 10).unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        local.chats().is_empty(),
        "triage ran during an interactive request"
    );
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.state, t.pending), (TriageState::Yielding, 1));
    drop(ask);
    triage_settled(&h).await;
    assert_eq!(local.chats().len(), 1);

    // Triage is mid-request when an Ask starts: the request is dropped, and redone after.
    local.block_chat();
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("b", "second note", Some("1"))),
        Step::Return(json!("p2")),
    ]);
    h.ingest.sync_now(None).unwrap();
    eventually("triage request sent", || local.chats().len() == 2).await;
    let ask = fg.begin();
    eventually_async("yielding", || {
        let ingest = h.ingest.clone();
        async move { ingest.status().await.unwrap().triage.unwrap().state == TriageState::Yielding }
    })
    .await;
    local.unblock_chat();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        local.chats_completed.load(Ordering::SeqCst),
        1,
        "the abandoned request must not complete"
    );
    assert_eq!(
        h.item("fake-1", "b").unwrap().metadata["triage"]["state"],
        json!("pending")
    );
    drop(ask);
    triage_settled(&h).await;
    assert_eq!(
        h.item("fake-1", "b").unwrap().metadata["triage"]["state"],
        json!("done")
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn pending_items_are_triaged_after_a_restart() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let first = local_model();
    first.block_chat();
    let h = start(Opts {
        router: local_router(&first),
        store: Some(store.clone()),
        dir: Some(dir.clone()),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![Step::Send(text_doc("a", "alpha note", Some("1")))],
        json!("done"),
    )
    .await;
    h.ingest.shutdown().await;
    assert_eq!(
        h.item("fake-1", "a").unwrap().metadata["triage"]["state"],
        json!("pending")
    );

    let second = local_model();
    let h2 = start(Opts {
        router: local_router(&second),
        store: Some(store),
        dir: Some(dir),
        factory: Some(h.factory.clone()),
        ..Default::default()
    })
    .await;
    triage_settled(&h2).await;
    eventually("labelled", || {
        h2.item("fake-1", "a").unwrap().metadata["triage"]["state"] == json!("done")
    })
    .await;
    h2.ingest.shutdown().await;
}

#[tokio::test]
async fn triage_off_leaves_unflagged_items_unknown_and_sensitive() {
    for (assume_safe, sensitive) in [(false, true), (true, false)] {
        let local = local_model();
        let h = start(Opts {
            cfg: IngestConfig {
                triage: false,
                assume_unflagged_safe: assume_safe,
                ..cfg()
            },
            router: local_router(&local),
            ..Default::default()
        })
        .await;
        h.run(
            "fake-1",
            vec![Step::Send(text_doc("a", "HUSHHUSH alpha", Some("1")))],
            json!("done"),
        )
        .await;
        let a = h.item("fake-1", "a").unwrap();
        assert_eq!(a.metadata["triage"]["state"], json!("unknown"));
        assert_eq!(a.metadata["triage"]["reason"], json!("off"));
        assert_eq!(
            a.metadata["sensitive"],
            json!(sensitive),
            "assume_safe={assume_safe}"
        );
        assert_eq!(h.store.search_text("alpha", 10).unwrap().len(), 1);
        assert!(local.chats().is_empty());
        h.ingest.shutdown().await;
    }
}

#[tokio::test]
async fn items_wait_sensitive_for_a_triage_model_that_is_not_routable_yet() {
    // A cloud embedder only, no local triage model: the item is pending and sensitive, so it
    // never reaches the cloud, until a local model labels it.
    let cloud = FakeProvider::new("cloud", Locality::Cloud);
    let local = local_model();
    let r = router(&[
        (cloud.clone(), vec![Tier::Embedding]),
        (local.clone(), vec![Tier::Triage]),
    ]);
    // Not routable for triage until its capabilities are known.
    r.set_capabilities(local.id(), Default::default());
    let h = start(Opts {
        cfg: IngestConfig {
            allow_cloud_embeddings: true,
            ..cfg()
        },
        router: Some(r.clone()),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![
            Step::Send(text_doc("a", "lunch plans for friday", Some("1"))),
            Step::Send(text_doc("secret", "HUSHHUSH quarterly letter", Some("1"))),
        ],
        json!("done"),
    )
    .await;
    let a = h.item("fake-1", "a").unwrap();
    assert_eq!(a.metadata["triage"]["state"], json!("pending"));
    assert_eq!(a.metadata["sensitive"], json!(true));
    assert_eq!(h.store.search_text("lunch", 10).unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(cloud.embedded().is_empty());
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.state, t.pending), (TriageState::Off, 2));

    // The local model becomes ready.
    r.set_capabilities(
        local.id(),
        nugget_core::capability::Capabilities {
            streaming: true,
            json_schema: true,
            ..Default::default()
        },
    );
    h.ingest.triage_now();
    eventually("labelled", || {
        h.item("fake-1", "a").unwrap().metadata["sensitive"] == json!(false)
    })
    .await;
    eventually("cloud embedded", || {
        cloud.embedded().iter().any(|t| t.contains("lunch"))
    })
    .await;
    // The item triage found sensitive never reaches the cloud, not even by re-embedding.
    h.ingest.reembed_now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!cloud.embedded().iter().any(|t| t.contains("quarterly")));
    assert_eq!(
        h.item("fake-1", "secret").unwrap().metadata["sensitive"],
        json!(true)
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn items_from_before_background_triage_are_relabelled_and_old_junk_reindexed() {
    // A store written by the old pipeline: a model-cleared health note (sensitive: false,
    // old triage keys), an ordinary note, and a junk item stored without chunks or text.
    let store = nugget_store::Store::open_in_memory().unwrap();
    let old = |key: &str, text: Option<&str>, meta: serde_json::Value| {
        let mut new = nugget_store::NewItem::new("fake-1", key, format!("h-{key}"))
            .with_metadata(meta)
            .with_version("1");
        new.text = text.map(str::to_owned);
        store.upsert_item(&new).unwrap();
        if let Some(t) = text {
            store
                .replace_chunks(&new.item, &[nugget_store::NewChunk::text(t)])
                .unwrap();
        }
    };
    let triage_old =
        json!({"keep": true, "junk": false, "sensitive": false, "summary": "s", "title": "t"});
    old(
        "gp",
        Some("Prescription: ferrous sulfate twice a day"),
        json!({"sensitive": false, "junk": false, "triage": triage_old}),
    );
    old(
        "note",
        Some("grocery list: eggs"),
        json!({"sensitive": false, "junk": false, "triage_error": "timed out"}),
    );
    old(
        "boiler",
        None,
        json!({"sensitive": false, "junk": true, "triage": {"keep": false, "junk": true}}),
    );
    let local = local_model();
    let h = start(Opts {
        router: local_router(&local),
        store: Some(store),
        ..Default::default()
    })
    .await;
    eventually("migrated and triaged", || {
        ["gp", "note"]
            .iter()
            .all(|k| h.item("fake-1", k).unwrap().metadata["triage"]["state"] == json!("done"))
    })
    .await;
    let gp = h.item("fake-1", "gp").unwrap();
    assert_eq!(gp.metadata["sensitive"], json!(true), "{}", gp.metadata);
    assert_eq!(gp.metadata["sensitive_rule"], json!("health"));
    assert_eq!(gp.metadata["triage"]["state"], json!("done"));
    let note = h.item("fake-1", "note").unwrap();
    assert_eq!(note.metadata["triage"]["state"], json!("done"));
    assert_eq!(note.metadata["sensitive"], json!(false));
    assert!(note.metadata.get("triage_error").is_none());
    // The junk item's version was forgotten: the next pass of its source re-indexes it.
    assert_eq!(h.item("fake-1", "boiler").unwrap().version, None);
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc(
            "boiler",
            "the boiler engineer visits Tuesday",
            Some("1"),
        )),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("re-indexed", || {
        h.store
            .search_text("engineer", 10)
            .map(|v| v.len())
            .unwrap_or(0)
            == 1
    })
    .await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn labels_leave_updated_at_alone_and_survive_a_concurrent_metadata_write() {
    // Review: a triage write-back must not look like a content change to consumers that fingerprint
    // items by content_hash:updated_at (knowledge extraction), and a label must not be dropped
    // because another writer (the re-embed job) bumped updated_at meanwhile.
    let local = local_model();
    local.block_chat();
    let h = start(Opts {
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![Step::Send(text_doc("a", "SPAM alpha offer", Some("1")))],
        json!("done"),
    )
    .await;
    eventually("triage request sent", || local.chats().len() == 1).await;

    // Another writer rewrites the item (same content) while triage waits for the model.
    let before = h.item("fake-1", "a").unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let mut meta = before.metadata.clone();
    meta["other_writer"] = json!(true);
    let mut new =
        nugget_store::NewItem::new("fake-1", "a", before.content_hash.clone()).with_metadata(meta);
    new.title = before.title.clone();
    new.mime = before.mime.clone();
    new.text = before.text.clone();
    new.version = before.version.clone();
    new.source_modified_at = before.source_modified_at;
    h.store.upsert_item(&new).unwrap();
    let bumped = h.item("fake-1", "a").unwrap();
    assert_ne!(bumped.updated_at, before.updated_at);

    local.unblock_chat();
    eventually("labelled", || {
        h.item("fake-1", "a").unwrap().metadata["triage"]["state"] == json!("done")
    })
    .await;
    let after = h.item("fake-1", "a").unwrap();
    assert_eq!(after.metadata["junk"], json!(true));
    assert_eq!(after.metadata["other_writer"], json!(true));
    assert_eq!(
        after.updated_at, bumped.updated_at,
        "labels must not bump updated_at"
    );
    assert_eq!(local.chats().len(), 1, "labelled once, not retried");
    h.ingest.shutdown().await;
}

/// Two items whose triage fails on a model that replies with garbage.
async fn two_failed_items(cfg: IngestConfig) -> (Harness, Arc<FakeProvider>) {
    let local = local_model();
    local.chat_invalid.store(true, Ordering::SeqCst);
    let h = start(Opts {
        cfg,
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![
            Step::Send(text_doc("a", "alpha note", Some("1"))),
            Step::Send(text_doc("b", "bravo note", Some("1"))),
        ],
        json!("done"),
    )
    .await;
    eventually_async("given up", || {
        let ingest = h.ingest.clone();
        async move { ingest.status().await.unwrap().triage.unwrap().failed == 2 }
    })
    .await;
    // Failed items stay searchable and sensitive.
    for (key, word) in [("a", "alpha"), ("b", "bravo")] {
        let it = h.item("fake-1", key).unwrap();
        assert_eq!(it.metadata["sensitive"], json!(true), "{key}");
        assert_eq!(h.store.search_text(word, 10).unwrap().len(), 1);
    }
    // The first try was a batch; the retries went one item per request.
    let chats = local.chats();
    assert_eq!(chats[0].matches("<document n=").count(), 2);
    assert!(
        chats[1..]
            .iter()
            .all(|c| c.matches("<document n=").count() == 1)
    );
    (h, local)
}

/// JJ2: failed items are triaged again with backoff once the model answers again, and leave
/// the `failed` count while they are retried.
#[tokio::test]
async fn failed_triage_is_retried_once_the_model_answers_again() {
    let (h, local) = two_failed_items(IngestConfig {
        triage_retry_ms: 100,
        ..cfg()
    })
    .await;
    for key in ["a", "b"] {
        let t = &h.item("fake-1", key).unwrap().metadata["triage"];
        assert!(t["failures"].as_u64().unwrap() >= 1, "{t}");
        assert!(t["failed_at"].as_i64().unwrap() > 0, "{t}");
    }
    // The outage ends.
    local.chat_invalid.store(false, Ordering::SeqCst);
    eventually("retried and labelled", || {
        ["a", "b"]
            .iter()
            .all(|k| h.item("fake-1", k).unwrap().metadata["triage"]["state"] == json!("done"))
    })
    .await;
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.failed, t.done, t.pending), (0, 2, 0), "{t:?}");
    for key in ["a", "b"] {
        let it = h.item("fake-1", key).unwrap();
        assert_eq!(it.metadata["sensitive"], json!(false), "{key}");
    }
    h.ingest.shutdown().await;
}

/// Retries stop after `triage_max_failures`, each item is counted as failed once, and the
/// job does not keep calling the model.
#[tokio::test]
async fn triage_retries_stop_after_the_maximum_failures() {
    let (h, local) = two_failed_items(IngestConfig {
        triage_retry_ms: 20,
        triage_max_failures: 2,
        ..cfg()
    })
    .await;
    eventually("failed for good", || {
        ["a", "b"].iter().all(|k| {
            let t = h.item("fake-1", k).unwrap().metadata["triage"].clone();
            t["state"] == json!("failed") && t["failures"] == json!(2)
        })
    })
    .await;
    triage_settled(&h).await;
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.failed, t.done), (2, 0), "{t:?}");
    let chats = local.chats().len();
    // Well past the retry wait (20 ms): nothing is tried again.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(local.chats().len(), chats);
    for key in ["a", "b"] {
        let it = h.item("fake-1", key).unwrap();
        assert_eq!(it.metadata["triage"]["failures"], json!(2));
        assert_eq!(it.metadata["sensitive"], json!(true));
    }
    // Even an explicit wake does not retry them.
    h.ingest.triage_now();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(local.chats().len(), chats);
    h.ingest.shutdown().await;
}

/// JJ2 review: an unreachable server (an outage) never uses up an item's tries; once it
/// answers again, the failed items are labelled, one per request.
#[tokio::test]
async fn an_outage_does_not_use_up_triage_retries() {
    let local = local_model();
    local.chat_down.store(true, Ordering::SeqCst);
    let h = start(Opts {
        cfg: IngestConfig {
            triage_retry_ms: 20,
            triage_max_failures: 2,
            ..cfg()
        },
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    h.run(
        "fake-1",
        vec![
            Step::Send(text_doc("a", "alpha note", Some("1"))),
            Step::Send(text_doc("b", "bravo note", Some("1"))),
        ],
        json!("done"),
    )
    .await;
    eventually("both failed", || {
        ["a", "b"]
            .iter()
            .all(|k| h.item("fake-1", k).unwrap().metadata["triage"]["state"] == json!("failed"))
    })
    .await;
    // The server stays down while the job keeps probing: no try is used up.
    let probes = local.chats().len();
    eventually("probed again", || local.chats().len() > probes + 3).await;
    for key in ["a", "b"] {
        let t = &h.item("fake-1", key).unwrap().metadata["triage"];
        assert_eq!(t["failures"], json!(0), "{key}: {t}");
    }
    local.chat_down.store(false, Ordering::SeqCst);
    let before = local.chats().len();
    eventually("labelled after the outage", || {
        ["a", "b"]
            .iter()
            .all(|k| h.item("fake-1", k).unwrap().metadata["triage"]["state"] == json!("done"))
    })
    .await;
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.failed, t.done), (0, 2), "{t:?}");
    assert!(
        local.chats()[before..]
            .iter()
            .all(|c| c.matches("<document n=").count() == 1),
        "retried items go one per request"
    );
    h.ingest.shutdown().await;
}

/// Index `keys` with triage off into a fresh store, then set each item's `triage` metadata
/// (and `sensitive: true`). Returns the store, data dir and factory for the next start.
async fn seeded(
    keys: &[(&str, serde_json::Value)],
) -> (
    nugget_store::Store,
    Arc<tempfile::TempDir>,
    Arc<FakeFactory>,
) {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let h = start(Opts {
        cfg: IngestConfig {
            triage: false,
            ..cfg()
        },
        store: Some(store.clone()),
        dir: Some(dir.clone()),
        ..Default::default()
    })
    .await;
    let mut steps: Vec<Step> = keys
        .iter()
        .map(|(k, _)| Step::Send(text_doc(k, &format!("{k} note"), Some("1"))))
        .collect();
    steps.push(Step::Return(json!("done")));
    h.factory.state("fake-1").script(steps);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    h.ingest.shutdown().await;
    for (k, triage) in keys {
        let it = h.item("fake-1", k).unwrap();
        let mut m = it.metadata.clone();
        m["triage"] = triage.clone();
        m["sensitive"] = json!(true);
        assert!(
            store
                .set_item_metadata(&it.item, &it.content_hash, &m)
                .unwrap()
        );
    }
    (store, dir, h.factory.clone())
}

/// An item whose triage failed before stays sensitive when triage is turned off before a
/// model labelled it, even with `assume_unflagged_safe`.
#[tokio::test]
async fn a_retried_item_stays_sensitive_when_triage_is_turned_off() {
    let (store, dir, factory) = seeded(&[
        ("retried", json!({"state": "pending", "failures": 1})),
        ("plain", json!({"state": "pending"})),
    ])
    .await;
    let h = start(Opts {
        cfg: IngestConfig {
            triage: false,
            assume_unflagged_safe: true,
            ..cfg()
        },
        store: Some(store),
        dir: Some(dir),
        factory: Some(factory),
        ..Default::default()
    })
    .await;
    eventually("resolved", || {
        ["retried", "plain"]
            .iter()
            .all(|k| h.item("fake-1", k).unwrap().metadata["triage"]["state"] == json!("unknown"))
    })
    .await;
    assert_eq!(
        h.item("fake-1", "retried").unwrap().metadata["sensitive"],
        json!(true)
    );
    assert_eq!(
        h.item("fake-1", "plain").unwrap().metadata["sensitive"],
        json!(false)
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn requests_are_sized_by_the_measured_prefill_rate_and_long_items_go_in_parts() {
    // A model that takes 1 ms per prompt character before its first token, and a 200 ms target:
    // after the first request, no request may carry much more than the 1 000-character floor, and a
    // long item is labelled in parts (sensitive if any part is).
    let local = local_model();
    local
        .prefill_us_per_char
        .store(1_000, std::sync::atomic::Ordering::SeqCst);
    let h = start(Opts {
        cfg: IngestConfig {
            triage_request_ms: 200,
            ..cfg()
        },
        router: local_router(&local),
        ..Default::default()
    })
    .await;
    let mut long = "an ordinary paragraph about the garden. ".repeat(80);
    long.push_str("HUSHHUSH at the very end");
    let mut script: Vec<Step> = (0..6)
        .map(|i| {
            Step::Send(text_doc(
                &format!("s{i}"),
                &format!(
                    "short note number {i} with some words to make it longer than fifty characters"
                ),
                Some("1"),
            ))
        })
        .collect();
    script.push(Step::Send(text_doc("long", &long, Some("1"))));
    script.push(Step::Return(json!("done")));
    h.factory.state("fake-1").script(script);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    eventually_async("triage settled", || {
        let ingest = h.ingest.clone();
        async move {
            let t = ingest.status().await.unwrap().triage.unwrap();
            t.pending == 0 && t.done == 7
        }
    })
    .await;
    let chats = local.chats();
    // Every request after the first fits the measured budget (document text plus markup: the
    // request's preamble naming its tag code, and each document's tags).
    for c in &chats[1..] {
        let docs = c.matches("<document n=").count();
        assert!(
            c.chars().count() <= 1_000 + 220 + 120 * docs,
            "{} chars: {c}",
            c.len()
        );
    }
    let parts: Vec<&String> = chats.iter().filter(|c| c.contains("(part ")).collect();
    assert!(parts.len() >= 3, "{chats:#?}");
    let long = h.item("fake-1", "long").unwrap();
    assert_eq!(long.metadata["triage"]["state"], json!("done"));
    assert_eq!(
        long.metadata["sensitive"],
        json!(true),
        "a sensitive last part marks the whole item"
    );
    h.ingest.shutdown().await;
}
