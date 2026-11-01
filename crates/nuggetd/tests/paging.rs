//! LL1: `search.page` and `egress.page` through IPC, against a real daemon whose store is seeded
//! directly. The unpaged `search` and `egress.recent` keep answering as before.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use nugget_core::egress::EgressEvent;
use nugget_core::ipc::{Page, SearchHit, method};
use nugget_core::paths::Paths;
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::{Client, IpcError, code};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const CALL: Duration = Duration::from_secs(60);

const CONFIG: &str = "[runtime]\nenabled = false\n\n[knowledge]\nenabled = false\n\n[vault]\nenabled = false\n\n[mcp]\nenabled = false\n";

struct Harness {
    _tmp: tempfile::TempDir,
    client: Client,
    store: Store,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start(seed: impl FnOnce(&Store)) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-ll1-")
            .tempdir()
            .unwrap();
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), CONFIG).unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([11; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.power = Some(Arc::new(NoPowerPolicy));
        opts.home = Some(tmp.path().join("home"));
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let store = daemon.store();
        seed(&store);
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            _tmp: tmp,
            client,
            store,
            stop: Some(tx),
            server: Some(server),
        }
    }

    async fn call<R: DeserializeOwned>(&self, m: &str, params: Value) -> R {
        let v = tokio::time::timeout(CALL, self.client.call::<Value, Value>(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} {params} failed: {e}"));
        serde_json::from_value(v).unwrap()
    }

    async fn call_err(&self, m: &str, params: Value) -> (i64, String) {
        match self.client.call_value(m, params).await {
            Err(IpcError::Rpc(e)) => (e.code, e.message),
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }
}

fn put(store: &Store, key: &str, title: &str, body: &str, sensitive: bool) {
    let item = NewItem::new("files", key, content_hash(key))
        .with_title(title)
        .with_metadata(json!({ "sensitive": sensitive }))
        .with_text(body);
    store
        .upsert_item_with_chunks(&item, &[NewChunk::text(body)])
        .unwrap();
}

fn egress(i: u64) -> EgressEvent {
    EgressEvent {
        at: time::OffsetDateTime::from_unix_timestamp(1_760_000_000 + i as i64).unwrap(),
        host: format!("h{i}.example.test"),
        feature: "chat".into(),
        bytes_sent: i,
        bytes_received: 0,
        items: Vec::new(),
    }
}

fn keys(hits: &[SearchHit]) -> Vec<String> {
    hits.iter()
        .map(|h| format!("{}/{}", h.item.source.as_str(), h.item.key.as_str()))
        .collect()
}

/// Pages a search to its end; returns every page.
async fn all_pages(h: &Harness, query: &str, limit: u32) -> Vec<Page<SearchHit>> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = json!({ "query": query, "limit": limit });
        if let Some(c) = &cursor {
            params["cursor"] = json!(c);
        }
        let page: Page<SearchHit> = h.call(method::SEARCH_PAGE, params).await;
        cursor = page.next_cursor.clone();
        pages.push(page);
        if cursor.is_none() {
            return pages;
        }
        assert!(pages.len() < 100, "search.page never ended");
    }
}

#[tokio::test]
async fn search_pages_cover_the_ranking_once_and_refuse_foreign_cursors() {
    let h = Harness::start(|store| {
        for i in 0..23 {
            // Every fourth item is sensitive: pages treat them exactly as `search` does.
            put(
                store,
                &format!("heron-{i:02}"),
                &format!("Heron note {i}"),
                &format!("Herons wading in the marsh, note {i}. heron {}", "heron ".repeat(i % 5)),
                i % 4 == 0,
            );
        }
        put(store, "swan", "Swan note", "A swan on the lake.", false);
    })
    .await;

    // The unpaged ranking, for comparison.
    let full: Vec<SearchHit> = h
        .call(method::SEARCH, json!({"query": "heron", "limit": 200}))
        .await;
    assert_eq!(full.len(), 23, "{:?}", keys(&full));

    let pages = all_pages(&h, "heron", 10).await;
    let sizes: Vec<usize> = pages.iter().map(|p| p.items.len()).collect();
    assert_eq!(sizes, vec![10, 10, 3]);
    let paged: Vec<SearchHit> = pages.into_iter().flat_map(|p| p.items).collect();
    let paged_keys = keys(&paged);
    assert_eq!(
        paged_keys.iter().collect::<HashSet<_>>().len(),
        paged_keys.len(),
        "a hit listed twice: {paged_keys:?}"
    );
    let full_keys: HashSet<String> = keys(&full).into_iter().collect();
    assert_eq!(
        paged_keys.iter().cloned().collect::<HashSet<_>>(),
        full_keys,
        "the pages hold the same hits as `search` (sensitive ones included)"
    );
    for k in ["files/heron-00", "files/heron-04", "files/heron-20"] {
        assert!(paged_keys.iter().any(|p| p == k), "sensitive {k} missing");
    }

    // Without a cursor, the params of `search` work as is; the page fits exactly: no cursor.
    let one: Page<SearchHit> = h
        .call(method::SEARCH_PAGE, json!({"query": "swan", "limit": 5}))
        .await;
    assert_eq!(keys(&one.items), vec!["files/swan".to_owned()]);
    assert_eq!(one.next_cursor, None);
    let empty: Page<SearchHit> = h
        .call(method::SEARCH_PAGE, json!({"query": "  ", "limit": 5}))
        .await;
    assert!(empty.items.is_empty() && empty.next_cursor.is_none());
    let empty: Page<SearchHit> = h
        .call(method::SEARCH_PAGE, json!({"query": "heron", "limit": 0}))
        .await;
    assert!(empty.items.is_empty() && empty.next_cursor.is_none());

    // A cursor is only good for its own query; anything else is invalid params.
    let first: Page<SearchHit> = h
        .call(method::SEARCH_PAGE, json!({"query": "heron", "limit": 5}))
        .await;
    let cursor = first.next_cursor.expect("more than 5 herons");
    let (c, msg) = h
        .call_err(
            method::SEARCH_PAGE,
            json!({"query": "swan", "limit": 5, "cursor": cursor}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS, "{msg}");
    for bad in [json!(""), json!("garbage"), json!("s1.0.00"), json!(5)] {
        let (c, msg) = h
            .call_err(
                method::SEARCH_PAGE,
                json!({"query": "heron", "limit": 5, "cursor": bad}),
            )
            .await;
        assert_eq!(c, code::INVALID_PARAMS, "{bad}: {msg}");
    }
    // An egress cursor is not a search cursor.
    let e: Page<EgressEvent> = h.call(method::EGRESS_PAGE, json!({"limit": 1})).await;
    assert!(e.items.is_empty() && e.next_cursor.is_none());

    h.stop().await;
}

#[tokio::test]
async fn search_paging_stops_at_its_depth() {
    let h = Harness::start(|store| {
        for i in 0..520 {
            put(
                store,
                &format!("kite-{i:03}"),
                &format!("Kite {i}"),
                &format!("Kite festival entry {i}."),
                false,
            );
        }
    })
    .await;
    let pages = all_pages(&h, "kite", 200).await;
    let sizes: Vec<usize> = pages.iter().map(|p| p.items.len()).collect();
    assert_eq!(sizes, vec![200, 200, 100], "500 hits at most");
    let all: Vec<SearchHit> = pages.into_iter().flat_map(|p| p.items).collect();
    let distinct: HashSet<String> = keys(&all).into_iter().collect();
    assert_eq!(distinct.len(), 500);
    h.stop().await;
}

#[tokio::test]
async fn egress_pages_neither_skip_nor_repeat_while_rows_arrive() {
    let h = Harness::start(|store| {
        for i in 0..23 {
            store.record_egress(&egress(i)).unwrap();
        }
    })
    .await;

    // The first page is what `egress.recent` returns.
    let recent: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 10})).await;
    let first: Page<EgressEvent> = h.call(method::EGRESS_PAGE, json!({"limit": 10})).await;
    assert_eq!(first.items, recent);

    let mut seen: Vec<u64> = first.items.iter().map(|e| e.bytes_sent).collect();
    let mut cursor = first.next_cursor;
    let mut sizes = vec![first.items.len()];
    let mut arrived = 1000;
    while let Some(c) = cursor {
        // A new request lands in the ledger between two pages.
        h.store.record_egress(&egress(arrived)).unwrap();
        arrived += 1;
        let page: Page<EgressEvent> = h
            .call(method::EGRESS_PAGE, json!({"limit": 10, "cursor": c}))
            .await;
        sizes.push(page.items.len());
        seen.extend(page.items.iter().map(|e| e.bytes_sent));
        cursor = page.next_cursor;
    }
    assert_eq!(sizes, vec![10, 10, 3]);
    assert_eq!(seen, (0..23u64).rev().collect::<Vec<_>>());
    // The rows that arrived are on a fresh first page.
    let fresh: Page<EgressEvent> = h.call(method::EGRESS_PAGE, json!({"limit": 2})).await;
    assert_eq!(fresh.items[0].bytes_sent, arrived - 1);
    assert!(fresh.next_cursor.is_some());

    // A page that ends exactly at the oldest row has no cursor.
    let exact: Page<EgressEvent> = h.call(method::EGRESS_PAGE, json!({"limit": 1000})).await;
    assert_eq!(exact.items.len(), 23 + (arrived - 1000) as usize);
    assert_eq!(exact.next_cursor, None);
    let none: Page<EgressEvent> = h.call(method::EGRESS_PAGE, json!({"limit": 0})).await;
    assert!(none.items.is_empty() && none.next_cursor.is_none());

    for bad in [
        json!(""),
        json!("e1.0"),
        json!("e1.-1"),
        json!("e1.abc"),
        json!("s1.5.0011223344556677"),
        json!("e1.99999999999999999999999"),
        json!(42),
    ] {
        let (c, msg) = h
            .call_err(method::EGRESS_PAGE, json!({"limit": 10, "cursor": bad}))
            .await;
        assert_eq!(c, code::INVALID_PARAMS, "{bad}: {msg}");
    }
    let (c, _) = h.call_err(method::EGRESS_PAGE, json!({})).await;
    assert_eq!(c, code::INVALID_PARAMS, "limit is required");

    h.stop().await;
}
