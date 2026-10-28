use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::NotionFactory;
use crate::text::{now_unix, rfc3339};

const ME: &str = "11111111-1111-1111-1111-111111111111";
const OTHER: &str = "22222222-2222-2222-2222-222222222222";

fn notion(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &NotionFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(2)),
        settings,
    )
}

fn ago(secs: i64) -> String {
    rfc3339(now_unix() - secs).unwrap()
}

async fn mount_me(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/notion/v1/users/me"))
        .and(header("notion-version", "2022-06-28"))
        .and(header("authorization", "Bearer ntn_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "user", "id": "bot", "type": "bot",
            "bot": {"owner": {"type": "user", "user": {"id": ME, "name": "Ada"}}}
        })))
        .mount(server)
        .await;
}

async fn mount_search(
    server: &MockServer,
    cursor: Option<&str>,
    results: Value,
    next: Option<&str>,
) {
    let mut m = Mock::given(method("POST")).and(path("/notion/v1/search"));
    m = match cursor {
        Some(c) => m.and(body_partial_json(json!({"start_cursor": c}))),
        None => m,
    };
    m.respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "object": "list", "results": results, "has_more": next.is_some(), "next_cursor": next
    })))
    .with_priority(if cursor.is_some() { 1 } else { 5 })
    .mount(server)
    .await;
}

async fn mount_children(server: &MockServer, id: &str, blocks: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/notion/v1/blocks/{id}/children")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "results": blocks, "has_more": false, "next_cursor": null
        })))
        .mount(server)
        .await;
}

fn page(id: &str, edited: &str, title: &str, creator: &str) -> Value {
    json!({
        "object": "page", "id": id, "last_edited_time": edited, "created_time": edited,
        "created_by": {"object": "user", "id": creator}, "last_edited_by": {"object": "user", "id": creator},
        "archived": false, "in_trash": false, "url": format!("https://www.notion.so/{id}"),
        "parent": {"type": "workspace", "workspace": true},
        "properties": {"title": {"id": "title", "type": "title", "title": [{"plain_text": title}]}}
    })
}

fn text_block(id: &str, ty: &str, text: &str, has_children: bool) -> Value {
    json!({"object": "block", "id": id, "type": ty, "has_children": has_children,
           ty: {"rich_text": [{"plain_text": text}]}})
}

#[tokio::test]
async fn sync_without_token_is_unauthorized() {
    let h = Harness::new().await;
    let conn = notion(&h, json!({}));
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());

    h.paste("ntn_bad");
    Mock::given(method("GET"))
        .and(path("/notion/v1/users/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "unauthorized"})))
        .mount(&h.server)
        .await;
    let r = run(&conn, None, &h.sink()).await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn full_sync_renders_pages_databases_and_rows() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    Mock::given(method("GET"))
        .and(path(format!("/notion/v1/users/{OTHER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": OTHER, "name": "Bob"})))
        .expect(1)
        .mount(&h.server)
        .await;
    let edited = ago(3600);
    let mut row = page("r1", &edited, "Ship it", OTHER);
    row["parent"] = json!({"type": "database_id", "database_id": "db1"});
    row["properties"]["Status"] = json!({"type": "status", "status": {"name": "In progress"}});
    row["properties"]["Tags"] =
        json!({"type": "multi_select", "multi_select": [{"name": "infra"}, {"name": "q4"}]});
    row["properties"]["Due"] = json!({"type": "date", "date": {"start": "2026-11-01"}});
    let mut archived = page("a1", &edited, "Old", ME);
    archived["archived"] = json!(true);
    mount_search(
        &h.server,
        None,
        json!([
            page("p1", &edited, "Plan", ME),
            {"object": "database", "id": "db1", "last_edited_time": edited, "url": "https://www.notion.so/db1",
             "created_by": {"id": ME}, "last_edited_by": {"id": ME}, "parent": {"type": "workspace"},
             "title": [{"plain_text": "Tasks"}], "description": [{"plain_text": "Team tasks"}],
             "properties": {"Name": {"type": "title", "title": {}},
                            "Status": {"type": "status", "status": {"options": [{"name": "Todo"}, {"name": "Done"}]}}}},
        ]),
        Some("c2"),
    )
    .await;
    mount_search(
        &h.server,
        Some("c2"),
        json!([row, archived, {"object": "page", "id": "../evil"}]),
        None,
    )
    .await;
    mount_children(
        &h.server,
        "p1",
        json!([
            text_block("b1", "heading_1", "Goals", false),
            {"object": "block", "id": "b2", "type": "paragraph", "has_children": false,
             "paragraph": {"rich_text": [{"plain_text": "Be "}, {"plain_text": "fast", "annotations": {"bold": true}}]}},
            text_block("b3", "bulleted_list_item", "outer", true),
            {"object": "block", "id": "b4", "type": "to_do", "has_children": false,
             "to_do": {"rich_text": [{"plain_text": "done thing"}], "checked": true}},
            {"object": "block", "id": "b5", "type": "code", "has_children": false,
             "code": {"rich_text": [{"plain_text": "fn main() {}"}], "language": "rust"}},
            {"object": "block", "id": "b6", "type": "table", "has_children": true,
             "table": {"has_column_header": true, "table_width": 2}},
            {"object": "block", "id": "b7", "type": "child_page", "has_children": true,
             "child_page": {"title": "Sub page"}},
        ]),
    )
    .await;
    mount_children(
        &h.server,
        "b3",
        json!([text_block("b31", "bulleted_list_item", "inner", false)]),
    )
    .await;
    mount_children(
        &h.server,
        "b6",
        json!([
            {"object": "block", "id": "r1x", "type": "table_row", "table_row": {"cells": [[{"plain_text": "k"}], [{"plain_text": "v"}]]}},
            {"object": "block", "id": "r2x", "type": "table_row", "table_row": {"cells": [[{"plain_text": "a|b"}], []]}},
        ]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/notion/v1/blocks/b7/children"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .expect(0)
        .mount(&h.server)
        .await;
    mount_children(
        &h.server,
        "r1",
        json!([text_block("rb", "paragraph", "row body", false)]),
    )
    .await;

    let conn = notion(&h, json!({}));
    let sink = h.sink();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["+page/p1", "+database/db1", "+page/r1", "-page/a1"]
    );
    let md = sink.text("page/p1");
    for want in [
        "# Plan\n",
        "## Goals",
        "Be **fast**",
        "- outer\n  - inner",
        "- [x] done thing",
        "```rust\nfn main() {}\n```",
        "| k | v |\n| --- | --- |\n| a\\|b |  |",
        "[Page: Sub page]",
    ] {
        assert!(md.contains(want), "missing {want:?} in\n{md}");
    }
    let p1 = sink.doc("page/p1");
    assert_eq!(p1.mime, "text/markdown");
    assert_eq!(p1.version.as_deref(), Some(edited.as_str()));
    assert_eq!(p1.url.as_deref(), Some("https://www.notion.so/p1"));
    assert_eq!(p1.metadata["from_me"], json!(true));
    assert_eq!(p1.metadata["author"], json!("Ada"));
    assert_eq!(p1.metadata["me"], json!("Ada"));

    let db = sink.text("database/db1");
    assert!(
        db.contains("# Tasks")
            && db.contains("Team tasks")
            && db.contains("**Status** (status): Todo, Done"),
        "{db}"
    );

    let r = sink.doc("page/r1");
    let rmd = sink.text("page/r1");
    assert!(
        rmd.contains("- **Status:** In progress") && rmd.contains("- **Tags:** infra, q4"),
        "{rmd}"
    );
    assert!(rmd.contains("- **Due:** 2026-11-01") && rmd.contains("row body"));
    assert_eq!(r.metadata["database_id"], json!("db1"));
    assert_eq!(r.metadata["author"], json!("Bob"));
    assert_eq!(r.metadata["from_me"], json!(false));
    assert_eq!(r.metadata["labels"], json!(["In progress", "infra", "q4"]));

    assert_eq!(cursor["since"], json!(edited));
    assert!(cursor["reconciled_at"].as_i64().unwrap() > 0);
    h.assert_egress("sync:notion").await;
}

#[tokio::test]
async fn incremental_stops_at_since_and_skips_unchanged_pages() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let since = ago(7200);
    let newer = ago(3600);
    h.know("page/same", &since);
    h.know("page/gone", "x");
    mount_search(
        &h.server,
        None,
        json!([
            page("new", &newer, "New", ME),
            page("same", &since, "Same", ME),
            page("old", &ago(90_000), "Old", ME),
            page("older", &ago(99_000), "Older", ME),
        ]),
        None,
    )
    .await;
    mount_children(
        &h.server,
        "new",
        json!([text_block("n1", "paragraph", "hello", false)]),
    )
    .await;
    for id in ["same", "old", "older"] {
        Mock::given(method("GET"))
            .and(path(format!("/notion/v1/blocks/{id}/children")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .expect(0)
            .mount(&h.server)
            .await;
    }
    let conn = notion(&h, json!({}));
    let sink = h.sink();
    let cursor = json!({"v": 1, "since": since, "reconciled_at": now_unix()});
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    // Not a reconciling pass: `page/gone` stays.
    assert_eq!(sink.summary(), vec!["+page/new"]);
    assert_eq!(next["since"], json!(newer));
    h.server.verify().await;
}

#[tokio::test]
async fn reconcile_deletes_unlisted_pages_and_respects_max_pages() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let e = ago(3600);
    h.know("page/a", &e);
    h.know("page/b", &e);
    h.know("page/unshared", &e);
    mount_search(
        &h.server,
        None,
        json!([page("a", &e, "A", ME), page("b", &ago(4000), "B", ME)]),
        None,
    )
    .await;
    let conn = notion(&h, json!({"max_pages": 1}));
    let sink = h.sink();
    // Periodic reconcile: an old reconciled_at.
    let cursor = json!({"v": 1, "since": e, "reconciled_at": 1});
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    let mut s = sink.summary();
    s.sort();
    assert_eq!(s, vec!["-page/b", "-page/unshared"]);
    assert!(next["reconciled_at"].as_i64().unwrap() > 1);
}

#[tokio::test]
async fn recent_edits_get_a_provisional_version_and_vanished_pages_are_deleted() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let just_now = ago(20);
    mount_search(
        &h.server,
        None,
        json!([
            page("fresh", &just_now, "F", ME),
            page("vanished", &ago(5000), "V", ME)
        ]),
        None,
    )
    .await;
    mount_children(&h.server, "fresh", json!([])).await;
    Mock::given(method("GET"))
        .and(path("/notion/v1/blocks/vanished/children"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code": "object_not_found"})))
        .mount(&h.server)
        .await;
    h.know("page/vanished", "old");
    let conn = notion(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+page/fresh", "-page/vanished"]);
    assert_eq!(sink.doc("page/fresh").version, Some(format!("{just_now}~")));
}

#[tokio::test]
async fn deep_and_huge_block_trees_are_capped() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let e = ago(3600);
    mount_search(
        &h.server,
        None,
        json!([page("deep", &e, "Deep", ME), page("wide", &e, "Wide", ME)]),
        None,
    )
    .await;
    // A chain nested 50 levels deep: only max_depth levels are fetched.
    mount_children(
        &h.server,
        "deep",
        json!([text_block("d0", "toggle", "level 0", true)]),
    )
    .await;
    for i in 0..50 {
        mount_children(
            &h.server,
            &format!("d{i}"),
            json!([text_block(
                &format!("d{}", i + 1),
                "toggle",
                &format!("level {}", i + 1),
                true
            )]),
        )
        .await;
    }
    // One page of 3 500 blocks with large text.
    let big = "x".repeat(400);
    let blocks: Vec<Value> = (0..3_500)
        .map(|i| text_block(&format!("w{i}"), "paragraph", &big, false))
        .collect();
    mount_children(&h.server, "wide", Value::Array(blocks)).await;
    let conn = notion(&h, json!({"max_depth": 3}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    let deep = sink.text("page/deep");
    assert!(
        deep.contains("level 2") && !deep.contains("level 3"),
        "{deep}"
    );
    assert!(deep.contains("nested content was not included"));
    let reqs = h.server.received_requests().await.unwrap();
    let deep_fetches = reqs
        .iter()
        .filter(|r| {
            r.url.path().contains("/blocks/d") || r.url.path().ends_with("/blocks/deep/children")
        })
        .count();
    assert_eq!(deep_fetches, 3);
    let wide = sink.text("page/wide");
    assert!(wide.len() <= crate::notion::MAX_DOC_CHARS + 100);
    assert!(wide.contains("nested content was not included") || wide.ends_with('…'));
}

#[tokio::test]
async fn rate_limits_retry_and_redirects_are_refused() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    Mock::given(method("POST"))
        .and(path("/notion/v1/search"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    mount_search(&h.server, None, json!([]), None).await;
    let conn = notion(&h, json!({}));
    run(&conn, None, &h.sink()).await.unwrap();
    let searches = h
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/search"))
        .count();
    assert_eq!(searches, 2);

    let h = Harness::new().await;
    h.paste("ntn_1");
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/x", elsewhere.uri())),
        )
        .mount(&h.server)
        .await;
    let conn = notion(&h, json!({}));
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn malformed_search_is_an_error_and_bad_blocks_are_skipped() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let e = ago(3600);
    mount_search(&h.server, None, json!([page("p", &e, "P", ME)]), None).await;
    Mock::given(method("GET"))
        .and(path("/notion/v1/blocks/p/children"))
        .and(query_param("page_size", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[[[[not json"))
        .mount(&h.server)
        .await;
    let conn = notion(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert!(sink.text("page/p").contains("not included"));

    h.server.reset().await;
    mount_me(&h.server).await;
    Mock::given(method("POST"))
        .and(path("/notion/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"results\": 5"))
        .mount(&h.server)
        .await;
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

/// Ids come from API responses and are interpolated into URL paths: a `..` id must not walk up
/// to another endpoint (`blocks/../children` → `/v1/children`, `users/..` → `/v1/`).
#[tokio::test]
async fn dot_segment_ids_never_reach_the_url_path() {
    let h = Harness::new().await;
    h.paste("ntn_1");
    mount_me(&h.server).await;
    let edited = ago(3600);
    let mut p = page("p1", &edited, "Plan", "..");
    p["last_edited_by"] = json!({"object": "user", "id": ".."});
    mount_search(
        &h.server,
        None,
        json!([p, page("..", &edited, "Up", ME)]),
        None,
    )
    .await;
    mount_children(
        &h.server,
        "p1",
        json!([
            text_block("..", "bulleted_list_item", "outer", true),
            text_block(".", "bulleted_list_item", "dot", true),
        ]),
    )
    .await;

    let conn = notion(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+page/p1"]);
    assert!(sink.text("page/p1").contains("outer"));
    let paths: Vec<String> = h
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    for p in &paths {
        assert!(
            p == "/notion/v1/users/me"
                || p == "/notion/v1/search"
                || p == "/notion/v1/blocks/p1/children",
            "unexpected request to {p} (all: {paths:?})"
        );
    }
}
