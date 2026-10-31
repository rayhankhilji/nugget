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

#[tokio::test]
async fn junk_and_sensitive_labels_never_remove_items_from_search() {
    let local = local_model();
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
    triage_settled(&h).await;

    // GG2-7: a junk verdict labels the item; it stays chunked, embedded and searchable.
    let spam = h.item("fake-1", "spam").unwrap();
    assert_eq!(spam.metadata["junk"], json!(true));
    assert_eq!(spam.metadata["triage"]["state"], json!("done"));
    assert_eq!(spam.metadata["triage"]["junk"], json!(true));
    assert_eq!(spam.metadata["sensitive"], json!(false));
    assert!(spam.text.is_some());
    assert_eq!(h.store.search_text("offer", 10).unwrap().len(), 1);
    assert!(local.embedded().iter().any(|t| t.contains("SPAM")));

    let memo = h.item("fake-1", "memo").unwrap();
    assert_eq!(memo.metadata["sensitive"], json!(true));
    assert_eq!(memo.metadata["junk"], json!(false));
    assert_eq!(h.store.search_text("merger", 10).unwrap().len(), 1);

    let note = h.item("fake-1", "note").unwrap();
    assert_eq!(note.metadata["sensitive"], json!(false));
    assert_eq!(note.metadata["triage"]["provider"], json!("local-chat"));

    // Three small items: one request.
    assert_eq!(local.chats().len(), 1, "{:?}", local.chats());
    let status = h.ingest.status().await.unwrap();
    let t = status.triage.unwrap();
    assert_eq!((t.pending, t.done, t.failed), (0, 3, 0));
    assert_eq!(t.state, TriageState::Idle);
    assert_eq!(
        status.triage_provider.as_ref().map(|p| p.as_str()),
        Some("local-chat")
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn items_are_searchable_before_triage_and_count_as_sensitive_until_labelled() {
    let local = local_model();
    local.block_chat();
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc(
            "boiler",
            "the boiler engineer visits on Tuesday",
            Some("1"),
        )),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    // GG2-6: found while the model has not answered yet.
    assert_eq!(h.store.search_text("boiler", 10).unwrap().len(), 1);
    let item = h.item("fake-1", "boiler").unwrap();
    assert_eq!(item.metadata["triage"]["state"], json!("pending"));
    assert_eq!(item.metadata["sensitive"], json!(true), "fail closed");
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!(t.pending, 1);

    local.unblock_chat();
    triage_settled(&h).await;
    let item = h.item("fake-1", "boiler").unwrap();
    assert_eq!(item.metadata["triage"]["state"], json!("done"));
    assert_eq!(item.metadata["sensitive"], json!(false));
    // Labelling rewrote metadata only: the chunk and its vector are still there.
    assert_eq!(h.store.search_text("engineer", 10).unwrap().len(), 1);
    assert!(
        h.store
            .items_missing_embeddings(None, 10)
            .unwrap()
            .is_empty()
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn pending_items_never_reach_cloud_embedders_until_triage_clears_them() {
    let local = local_model();
    let cloud = FakeProvider::new("cloud", Locality::Cloud);
    local.block_chat();
    let h = start(Opts {
        cfg: IngestConfig {
            allow_cloud_embeddings: true,
            ..cfg()
        },
        router: Some(router(&[
            (local.clone(), vec![Tier::Triage]),
            (cloud.clone(), vec![Tier::Embedding]),
        ])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("plain", "lunch menu for friday", Some("1"))),
        Step::Send(text_doc(
            "secret",
            "CONFIDENTIAL diagnosis letter",
            Some("1"),
        )),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    // Indexed text-only: searchable, nothing sent to the cloud before triage.
    assert_eq!(h.store.search_text("lunch", 10).unwrap().len(), 1);
    assert!(cloud.embedded().is_empty());

    local.unblock_chat();
    triage_settled(&h).await;
    // The item triage cleared gets cloud vectors from the re-embed job; the sensitive one never.
    eventually("plain embedded", || {
        cloud.embedded().iter().any(|t| t.contains("lunch menu"))
    })
    .await;
    h.ingest.reembed_now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !cloud.embedded().iter().any(|t| t.contains("diagnosis")),
        "a sensitive item reached the cloud"
    );
    assert_eq!(
        h.item("fake-1", "secret").unwrap().metadata["sensitive"],
        json!(true)
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn items_with_pii_skip_the_model_and_unchanged_content_keeps_its_labels() {
    let local = local_model();
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc(
            "card",
            "charge 4111 1111 1111 1111 please",
            Some("1"),
        )),
        Step::Send(text_doc("note", "SPAM weekly deals", Some("1"))),
        Step::Return(json!("p1")),
    ]);
    h.add("fake-1").await;
    eventually("pass 1", || h.cursor("fake-1") == Some(json!("p1"))).await;
    triage_settled(&h).await;
    let card = h.item("fake-1", "card").unwrap();
    assert_eq!(card.metadata["triage"]["state"], json!("skipped"));
    assert_eq!(card.metadata["sensitive"], json!(true));
    assert_eq!(local.chats().len(), 1);
    assert!(!local.chats()[0].contains("4111"));
    let t = h.ingest.status().await.unwrap().triage.unwrap();
    assert_eq!((t.done, t.skipped), (1, 1));

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
    assert_eq!(local.chats().len(), 1);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn small_items_are_batched_within_the_count_and_character_budgets() {
    let local = local_model();
    local.block_chat();
    let h = start(Opts {
        cfg: IngestConfig {
            triage_batch: 4,
            triage_batch_chars: 1_000,
            ..cfg()
        },
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    let mut script: Vec<Step> = (0..10)
        .map(|i| {
            Step::Send(text_doc(
                &format!("s{i}"),
                &format!("short note {i}"),
                Some("1"),
            ))
        })
        .collect();
    script.push(Step::Send(text_doc(
        "long",
        &"long text ".repeat(150),
        Some("1"),
    )));
    script.push(Step::Return(json!("done")));
    h.factory.state("fake-1").script(script);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    local.unblock_chat();
    triage_settled(&h).await;
    let chats = local.chats();
    // 10 short items in batches of at most 4 (4 + 4 + 2), the long one alone.
    let docs: Vec<usize> = chats
        .iter()
        .map(|c| c.matches("<document n=").count())
        .collect();
    assert!(docs.iter().all(|&n| n <= 4), "{docs:?}");
    assert_eq!(docs.iter().sum::<usize>(), 11, "{docs:?}");
    assert!(chats.len() <= 5, "{docs:?}");
    let long = chats.iter().find(|c| c.contains("long text")).unwrap();
    assert_eq!(long.matches("<document n=").count(), 1);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn triage_waits_for_interactive_requests_and_abandons_its_own_when_one_starts() {
    let local = local_model();
    let fg = Arc::new(Foreground::new(Duration::from_millis(50)));
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        foreground: Some(fg.clone()),
        ..Default::default()
    })
    .await;

    // An Ask is in flight: triage does not touch the model.
    let ask = fg.begin();
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "first note", Some("1"))),
        Step::Return(json!("p1")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("p1"))).await;
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
async fn failed_triage_keeps_items_searchable_and_sensitive() {
    let local = local_model();
    local.chat_invalid.store(true, Ordering::SeqCst);
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "alpha note", Some("1"))),
        Step::Send(text_doc("b", "bravo note", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    eventually_async("given up", || {
        let ingest = h.ingest.clone();
        async move { ingest.status().await.unwrap().triage.unwrap().failed == 2 }
    })
    .await;
    for (key, word) in [("a", "alpha"), ("b", "bravo")] {
        let it = h.item("fake-1", key).unwrap();
        assert_eq!(it.metadata["triage"]["state"], json!("failed"), "{key}");
        assert!(it.metadata["triage"]["error"].is_string());
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
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn pending_items_are_triaged_after_a_restart() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let first = local_model();
    first.block_chat();
    let h = start(Opts {
        router: Some(router(&[(
            first.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        store: Some(store.clone()),
        dir: Some(dir.clone()),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "alpha note", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    h.ingest.shutdown().await;
    assert_eq!(
        h.item("fake-1", "a").unwrap().metadata["triage"]["state"],
        json!("pending")
    );

    let second = local_model();
    let h2 = start(Opts {
        router: Some(router(&[(
            second.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
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
async fn triage_off_labels_by_pii_only() {
    let local = local_model();
    let h = start(Opts {
        cfg: IngestConfig {
            triage: false,
            ..cfg()
        },
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "CONFIDENTIAL alpha", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;
    let a = h.item("fake-1", "a").unwrap();
    assert!(a.metadata.get("triage").is_none());
    assert_eq!(a.metadata["sensitive"], json!(false));
    assert!(local.chats().is_empty());
    h.ingest.shutdown().await;
}
