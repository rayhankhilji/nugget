//! Smaller IPC surfaces against a real daemon: `search.page` and `egress.page` (LL1),
//! `egress.record`, `models.fit` and an external `[retrieve.reranker]`.

mod common;

use std::collections::HashSet;

use common::*;
use nugget_core::egress::EgressEvent;
use nugget_core::ipc::{ModelInfo, Page, SearchHit, method};
use nugget_ipc::code;
use nugget_models::{GIB, GpuKind, Hardware};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use nuggetd::RuntimeOverrides;
use serde_json::{Value, json};
use wiremock::matchers::{method as http_method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const PLAIN: &str = "[runtime]\nenabled = false\n\n[knowledge]\nenabled = false\n\n[vault]\nenabled = false\n\n[mcp]\nenabled = false\n";

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_pages_cover_the_ranking_once_to_its_depth() {
    let mut h = Harness::start(PLAIN).await;
    let store = &h.store;
    {
        for i in 0..23 {
            // Every fourth item is sensitive: pages treat them exactly as `search` does.
            put(
                store,
                &format!("heron-{i:02}"),
                &format!("Heron note {i}"),
                &format!(
                    "Herons wading in the marsh, note {i}. heron {}",
                    "heron ".repeat(i % 5)
                ),
                i % 4 == 0,
            );
        }
        put(store, "swan", "Swan note", "A swan on the lake.", false);
        for i in 0..520 {
            let key = format!("kite-{i:03}");
            put(
                store,
                &key,
                &key,
                &format!("Kite festival entry {i}."),
                false,
            );
        }
    }

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
    // Paging stops at its depth: 500 hits at most.
    let pages = all_pages(&h, "kite", 200).await;
    let sizes: Vec<usize> = pages.iter().map(|p| p.items.len()).collect();
    assert_eq!(sizes, vec![200, 200, 100]);
    let all: Vec<SearchHit> = pages.into_iter().flat_map(|p| p.items).collect();
    assert_eq!(keys(&all).into_iter().collect::<HashSet<_>>().len(), 500);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn egress_pages_neither_skip_nor_repeat_while_rows_arrive() {
    let mut h = Harness::start(PLAIN).await;
    for i in 0..23 {
        h.store.record_egress(&egress(i)).unwrap();
    }

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
        json!("e1.-1"),
        json!("s1.5.0011223344556677"),
        json!("e1.99999999999999999999999"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn egress_record_takes_client_requests_only() {
    let mut h = Harness::start(NO_RUNTIME).await;
    let r: Value = h
        .call(
            method::EGRESS_RECORD,
            json!({"host": "github.com", "feature": "update_check", "bytes_received": 2048}),
        )
        .await;
    assert_eq!(r, Value::Null);
    // Queued in the app for an hour, then sent: the row keeps when the request was made.
    let queued = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let at = queued
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let _: Value = h
        .call(
            method::EGRESS_RECORD,
            json!({"at": at, "host": "objects.example.com", "feature": "update_download",
                   "bytes_sent": 0, "bytes_received": 9_000_000}),
        )
        .await;
    // Refused: a daemon feature, a URL instead of a host, missing fields.
    for bad in [
        json!({"host": "api.example.com", "feature": "chat"}),
        json!({"host": "https://github.com/x", "feature": "update_check"}),
        json!({"feature": "update_check"}),
    ] {
        assert_eq!(
            h.call_err(method::EGRESS_RECORD, bad).await.0,
            code::INVALID_PARAMS
        );
    }
    let events: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
    assert_eq!(events.len(), 2, "{events:?}");
    let check = events.iter().find(|e| e.feature == "update_check").unwrap();
    assert_eq!(
        (check.host.as_str(), check.bytes_received),
        ("github.com", 2048)
    );
    let download = events
        .iter()
        .find(|e| e.feature == "update_download")
        .unwrap();
    assert_eq!(download.at.unix_timestamp(), queued.unix_timestamp());
    h.stop().await;
}

/// On a given machine every manifest model is explained, and the recommended set is exactly
/// `models.list`'s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_fit_explains_the_recommendation() {
    let mut h = Harness::start_with(NO_RUNTIME, |o| {
        o.runtime = RuntimeOverrides {
            hardware: Some(Hardware {
                total_ram: 8 * GIB * 95 / 100,
                available_ram: 4 * GIB,
                cpu_cores: 4,
                gpu: GpuKind::Unknown,
            }),
            ..RuntimeOverrides::default()
        }
    })
    .await;
    let fit: Value = h.call("models.fit", Value::Null).await;
    assert_eq!(fit["ram_gb"], 8, "{fit}");
    assert_eq!(fit["hardware"]["gpu"], "unknown");
    let models = fit["models"].as_array().unwrap();
    let small = models.iter().find(|m| m["id"] == "qwen3-4b-q4km").unwrap();
    assert_eq!(
        (small["recommended"].as_bool(), small["accel"].as_str()),
        (Some(true), Some("cpu"))
    );
    assert_eq!(
        small["reasons"],
        json!(["ram_fits_largest", "cpu_only_slower"])
    );
    let large = models.iter().find(|m| m["id"] == "qwen3-8b-q4km").unwrap();
    assert_eq!(large["reasons"], json!(["ram_too_small"]));
    let list: Vec<ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    for info in list {
        let f = models.iter().find(|m| m["id"] == info.id.as_str()).unwrap();
        assert_eq!(f["recommended"], info.recommended, "{}", info.id);
    }
    h.stop().await;
}

fn first_is_repair(hits: &[SearchHit]) -> bool {
    hits.first()
        .is_some_and(|h| h.snippet.contains("electrode"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_external_reranker_reorders_search_results() {
    let reranker = MockServer::start().await;
    let config = format!(
        "[runtime]\nenabled = false\n[retrieve]\nmmr = false\n\
         [retrieve.reranker]\nbase_url = \"{}\"\n",
        reranker.uri()
    );
    let mut h = Harness::start(&config).await;
    h.add_files(
        "docs",
        &[
            ("a.md", "Boiler boiler boiler: the boiler manual."),
            (
                "b.md",
                "Boiler repair: fault F28, ignition electrode replaced.",
            ),
        ],
    )
    .await;
    eventually("both notes searchable", || async {
        h.search("boiler").await.len() == 2
    })
    .await;

    // The reranker answers 404 (nothing mounted): first-stage order.
    let hits = h.search("boiler").await;
    assert_eq!(hits.len(), 2);
    assert!(!first_is_repair(&hits), "{hits:?}");

    // Now it prefers the passage about the electrode.
    Mock::given(http_method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(|req: &Request| {
            let v: Value = serde_json::from_slice(&req.body).unwrap();
            let results: Vec<Value> = v["documents"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let s = if d.as_str().unwrap().contains("electrode") {
                        0.95
                    } else {
                        0.05
                    };
                    json!({"index": i, "relevance_score": s})
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"results": results}))
        })
        .mount(&reranker)
        .await;
    let hits = h.search("boiler").await;
    assert_eq!(hits.len(), 2);
    assert!(first_is_repair(&hits), "{hits:?}");
    h.stop().await;
}
