//! Late embeddings: items indexed text-only get vectors once an embedder is routable, under
//! the same privacy rules; NaN embeddings never leave a chunkless item.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_infer::{Provider, Router};
use nugget_ingest::IngestConfig;
use serde_json::json;

/// A router whose only member is `p` in the embedding tier, probed **without** embeddings: it
/// is routable for nothing until [`enable`] (like a local runtime slot before its server is
/// ready).
fn dormant_router(p: &Arc<FakeProvider>) -> Arc<Router> {
    let r = Router::new(
        vec![p.clone() as Arc<dyn Provider>],
        HashMap::from([(Tier::Embedding, vec![p.id().clone()])]),
    );
    r.set_capabilities(p.id(), Capabilities::default());
    Arc::new(r)
}

fn enable(r: &Router, p: &FakeProvider) {
    r.set_capabilities(
        p.id(),
        Capabilities {
            embeddings: true,
            ..Default::default()
        },
    );
}

fn no_periodic() -> IngestConfig {
    IngestConfig {
        reembed_interval_ms: 0,
        ..cfg()
    }
}

#[tokio::test]
async fn items_indexed_before_an_embedder_is_routable_are_embedded_later() {
    let embedder = FakeProvider::new("local-embed", Locality::OnDevice);
    let r = dormant_router(&embedder);
    let h = start(Opts {
        cfg: no_periodic(),
        router: Some(r.clone()),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "Pelicans glide over the harbour", Some("1"))),
        Step::Send(text_doc(
            "card",
            "Please charge 4111 1111 1111 1111 for the order",
            Some("1"),
        )),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    // Indexed text-only: nothing could embed it.
    assert!(embedder.embedded().is_empty());
    assert_eq!(h.store.embedding_dim().unwrap(), None);
    assert!(
        h.item("fake-1", "a")
            .unwrap()
            .metadata
            .get("embedding_provider")
            .is_none()
    );
    assert_eq!(h.store.items_missing_embeddings(None, 10).unwrap().len(), 2);

    // The embedder becomes routable (e.g. the local server is ready) and the job is poked.
    enable(&r, &embedder);
    h.ingest.reembed_now();
    eventually("item a in vector search", || {
        h.store
            .search_vector(&[1.0; 4], 10)
            .unwrap()
            .iter()
            .any(|hit| hit.item.key.as_str() == "a")
    })
    .await;
    eventually("everything embedded", || {
        h.store
            .items_missing_embeddings(None, 10)
            .unwrap()
            .is_empty()
    })
    .await;
    let a = h.item("fake-1", "a").unwrap();
    assert_eq!(a.metadata["embedding_provider"], json!("local-embed"));
    assert_eq!(a.version.as_deref(), Some("1"), "the version is kept");
    // Sensitive items may use local embedders.
    assert!(embedder.embedded().iter().any(|t| t.contains("4111")));
    assert_eq!(h.ingest.reembed_stats().embedded, 2);
    // Still searchable by text, with the same single chunk.
    assert_eq!(h.store.search_text("pelicans", 10).unwrap().len(), 1);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn reembedding_never_sends_sensitive_items_to_cloud_embedders() {
    let cloud = FakeProvider::new("cloud-embed", Locality::Cloud);
    let r = dormant_router(&cloud);
    let h = start(Opts {
        cfg: IngestConfig {
            allow_cloud_embeddings: true,
            ..no_periodic()
        },
        router: Some(r.clone()),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("public", "Lunch menu for friday", Some("1"))),
        Step::Send(text_doc(
            "private",
            "employee ssn 123-45-6789 on file",
            Some("1"),
        )),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    enable(&r, &cloud);
    h.ingest.reembed_now();
    eventually("public item embedded", || {
        h.item("fake-1", "public")
            .unwrap()
            .metadata
            .get("embedding_provider")
            .is_some()
    })
    .await;
    // Another run changes nothing for the sensitive item.
    h.ingest.reembed_now();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !cloud.embedded().join("\n").contains("6789"),
        "a sensitive item reached a cloud embedder"
    );
    assert!(
        h.item("fake-1", "private")
            .unwrap()
            .metadata
            .get("embedding_provider")
            .is_none()
    );
    assert!(h.ingest.reembed_stats().skipped >= 1);
    // GG8-6: the re-embed request names the item it carried.
    let items = cloud.embed_items.lock().unwrap().clone();
    assert!(!items.is_empty());
    assert!(
        items.iter().all(|i| i.key.as_str() == "public"),
        "{items:?}"
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn nan_embeddings_fall_back_to_text_only_and_are_retried() {
    let embedder = FakeProvider::new("local-embed", Locality::OnDevice);
    embedder.nan.store(true, Ordering::SeqCst);
    let r = router(&[(embedder.clone(), vec![Tier::Embedding])]);
    let h = start(Opts {
        cfg: no_periodic(),
        router: Some(r),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("n", "Nightjars hunt at dusk", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!("done"))).await;

    // The item is complete and searchable by text; the bad vectors were not stored.
    let n = h.item("fake-1", "n").expect("item stored");
    assert_eq!(n.version.as_deref(), Some("1"));
    assert!(
        n.metadata["embed_error"].as_str().unwrap().contains("NaN"),
        "{}",
        n.metadata
    );
    assert_eq!(h.store.search_text("nightjars", 10).unwrap().len(), 1);
    assert_eq!(h.store.embedding_dim().unwrap(), None);
    assert!(
        h.ingest
            .errors(&nugget_core::ids::SourceId::new("fake-1"))
            .unwrap()
            .is_empty(),
        "not an item error"
    );

    // Once the embedder behaves, the job embeds it.
    embedder.nan.store(false, Ordering::SeqCst);
    h.ingest.reembed_now();
    eventually("embedded later", || {
        h.item("fake-1", "n")
            .unwrap()
            .metadata
            .get("embedding_provider")
            .is_some()
    })
    .await;
    assert!(
        h.item("fake-1", "n")
            .unwrap()
            .metadata
            .get("embed_error")
            .is_none()
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn a_dropped_send_releases_its_queue_depth() {
    let embedder = FakeProvider::new("local", Locality::OnDevice);
    embedder.block("BLOCK");
    let h = start(Opts {
        cfg: IngestConfig {
            queue_capacity: 1,
            ..no_periodic()
        },
        router: Some(router(&[(embedder.clone(), vec![Tier::Embedding])])),
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Send(text_doc("a", "first BLOCK", Some("1"))),
        Step::Send(text_doc("b", "second", Some("1"))),
        Step::Send(text_doc("c", "third", Some("1"))),
        Step::Send(text_doc("d", "fourth", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;
    // `a` is stuck in the worker, `b` fills the queue, `c` waits in `send`.
    eventually("worker blocked", || {
        embedder.embedded().iter().any(|t| t.contains("BLOCK"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(st.sent.load(Ordering::SeqCst), 2);
    assert_eq!(h.ingest.status().await.unwrap().queue_depth, 3);

    // Stopping the source drops the connector's pending `send`.
    h.ingest
        .remove(&nugget_core::ids::SourceId::new("fake-1"), true)
        .await
        .unwrap();
    assert_eq!(
        h.ingest.status().await.unwrap().queue_depth,
        0,
        "the gauge must not leak changes that never reached the queue"
    );
    embedder.unblock();
    h.ingest.shutdown().await;
}
