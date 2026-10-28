use nugget_connector::Content;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::OneDriveFactory;

const DELTA: &str = "/v1.0/me/drive/root/delta";

fn drive(h: &Harness, endpoints: MicrosoftEndpoints, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &OneDriveFactory::new()
            .with_endpoints(endpoints)
            .with_retry(fast_retry(0)),
        settings,
    )
}

fn link(h: &Harness, q: &str) -> String {
    format!("{}{DELTA}?{q}", h.uri())
}

fn page(
    h: &Harness,
    items: Vec<Value>,
    next: Option<&str>,
    delta: Option<&str>,
) -> ResponseTemplate {
    let mut body = json!({"value": items});
    if let Some(n) = next {
        body["@odata.nextLink"] = link(h, n).into();
    }
    if let Some(d) = delta {
        body["@odata.deltaLink"] = link(h, d).into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn root() -> Value {
    json!({"id": "ROOT", "name": "root", "root": {}, "folder": {"childCount": 3}})
}

fn folder(id: &str, parent: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "folder": {"childCount": 1}, "parentReference": {"id": parent, "driveId": "d"}})
}

fn file(id: &str, parent: &str, name: &str, size: u64, download: Option<String>) -> Value {
    let mut v = json!({
        "id": id, "name": name, "size": size, "eTag": format!("\"{{{id}}},1\""), "cTag": format!("c-{id}"),
        "lastModifiedDateTime": "2026-10-01T12:00:00Z", "webUrl": format!("https://contoso-my.sharepoint.com/{id}"),
        "file": {"mimeType": "application/octet-stream"}, "parentReference": {"id": parent, "driveId": "d"},
    });
    if let Some(d) = download {
        v["@microsoft.graph.downloadUrl"] = d.into();
    }
    v
}

fn deleted(id: &str, is_folder: bool) -> Value {
    let mut v =
        json!({"id": id, "deleted": {"state": "deleted"}, "parentReference": {"id": "ROOT"}});
    if is_folder {
        v["folder"] = json!({});
    } else {
        v["file"] = json!({});
    }
    v
}

fn version(id: &str, parent: &str) -> String {
    format!("\"{{{id}}},1\"|{parent}")
}

async fn mount_initial(h: &Harness, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param_is_missing("token"))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

async fn mount_token(h: &Harness, token: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param("token", token))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

/// The download host: serves `/dl/<name>`.
async fn download_host() -> MockServer {
    let dl = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(|r: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_string(format!("content of {}", r.url.path()))
        })
        .mount(&dl)
        .await;
    dl
}

/// `/me/drive/items/<id>/content` answers with a redirect to `location`.
async fn mount_content(h: &Harness, id: &str, location: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/{id}/content")))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
        .mount(&h.server)
        .await;
}

fn assert_no_bearer(reqs: &[wiremock::Request]) {
    for r in reqs {
        assert!(
            r.headers.get("authorization").is_none(),
            "bearer leaked to {}",
            r.url
        );
    }
}

#[tokio::test]
async fn enumeration_applies_folder_filters_and_downloads_without_bearer() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    let d = |n: &str| Some(format!("{}/dl/{n}?tempauth=xyz", dl.uri()));
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                folder("DOCS", "ROOT", "Documents"),
                // A file listed before its folder still resolves.
                file("LATE", "WORK", "late.txt", 10, d("late")),
                folder("WORK", "DOCS", "Work"),
                folder("PRIV", "DOCS", "Private"),
            ],
            Some("token=p2"),
            None,
        ),
    )
    .await;
    mount_token(
        &h,
        "p2",
        page(
            &h,
            vec![
                folder("PICS", "ROOT", "Pictures"),
                file("A", "DOCS", "Notes.MD", 10, d("a")),
                file("R", "WORK", "report.pdf", 10, None), // no download URL: /content
                file("S", "PRIV", "secret.txt", 10, d("s")),
                file("P", "PICS", "p.txt", 10, d("p")),
                file("X", "DOCS", "app.exe", 10, d("x")),
                file("H", "DOCS", "huge.pdf", 3 * 1024 * 1024, d("h")),
                file("T", "ROOT", "top.txt", 10, d("t")),
            ],
            None,
            Some("token=d1"),
        ),
    )
    .await;
    mount_content(&h, "R", &format!("{}/dl/r?tempauth=fresh", dl.uri())).await;

    let conn = drive(
        &h,
        h.endpoints(),
        json!({
            "client_id": "cid", "include_folders": ["/documents"], "exclude_folders": ["/Documents/Private"], "max_file_mb": 2
        }),
    );
    let sink = RecSink::default();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+LATE", "+A", "+R"]);
    assert_eq!(cursor["delta_link"], link(&h, "token=d1"));
    assert_eq!(cursor["root"], "ROOT");
    assert_eq!(cursor["folders"]["WORK"], json!(["DOCS", "Work"]));
    assert!(cursor.get("full").is_none());
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["next_link"], link(&h, "token=p2"));
    assert_eq!(cps[0]["folders"]["DOCS"], json!(["ROOT", "Documents"]));

    let docs = sink.upserts();
    let a = &docs[1];
    assert_eq!(a.mime, "text/markdown");
    assert_eq!(a.title.as_deref(), Some("Notes.MD"));
    assert!(matches!(&a.content, Content::Bytes(b) if b == b"content of /dl/a"));
    assert_eq!(a.version.as_deref(), Some(version("A", "DOCS").as_str()));
    assert_eq!(a.metadata["path"], "/Documents/Notes.MD");
    assert_eq!(
        a.url.as_deref(),
        Some("https://contoso-my.sharepoint.com/A")
    );
    assert_eq!(docs[0].metadata["path"], "/Documents/Work/late.txt");
    assert_eq!(docs[2].mime, "application/pdf");
    assert!(matches!(&docs[2].content, Content::Bytes(b) if b == b"content of /dl/r"));

    // Downloads went to the download host without the token; only in-scope files were fetched.
    let got = dl.received_requests().await.unwrap();
    let mut paths: Vec<_> = got.iter().map(|r| r.url.path().to_string()).collect();
    paths.sort();
    assert_eq!(paths, vec!["/dl/a", "/dl/late", "/dl/r"]);
    assert_no_bearer(&got);
    // Graph requests did carry it.
    for r in h.server.received_requests().await.unwrap() {
        assert_eq!(r.headers.get("authorization").unwrap(), "Bearer at");
    }
    // One egress row per request on either host, all under `sync:onedrive`.
    let graph_requests = h.server.received_requests().await.unwrap().len();
    let events = h.egress.0.lock().unwrap();
    let graph_rows = events
        .iter()
        .filter(|e| e.host == host_of(&h.uri()))
        .count();
    let dl_rows = events
        .iter()
        .filter(|e| e.host == host_of(&dl.uri()))
        .count();
    assert_eq!(graph_rows, graph_requests);
    assert_eq!(dl_rows, 3);
    assert_eq!(events.len(), graph_rows + dl_rows);
    assert!(events.iter().all(|e| e.feature == "sync:onedrive"));
}

#[tokio::test]
async fn delta_round_handles_deletes_moves_and_unchanged_files() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    h.know("KEEP", &version("KEEP", "DOCS"));
    h.know("GONE", &version("GONE", "DOCS"));
    h.know("IN_OLD", &version("IN_OLD", "OLD"));
    h.know("IN_SUB", &version("IN_SUB", "SUB"));
    h.know("MOVED", &version("MOVED", "DOCS"));
    mount_token(
        &h,
        "d1",
        page(
            &h,
            vec![
                // Metadata-only touch: same eTag and parent, no download.
                file(
                    "KEEP",
                    "DOCS",
                    "keep.txt",
                    10,
                    Some(format!("{}/dl/keep", dl.uri())),
                ),
                deleted("GONE", false),
                // Graph reports only the folder: its indexed files (and a subfolder's) go too.
                deleted("OLD", true),
                // Moved to the root: no longer under /Documents.
                file(
                    "MOVED",
                    "ROOT",
                    "moved.txt",
                    10,
                    Some(format!("{}/dl/moved", dl.uri())),
                ),
                file(
                    "NEW",
                    "DOCS",
                    "new.csv",
                    10,
                    Some(format!("{}/dl/new", dl.uri())),
                ),
            ],
            None,
            Some("token=d2"),
        ),
    )
    .await;
    let conn = drive(
        &h,
        h.endpoints(),
        json!({"client_id": "cid", "include_folders": ["/Documents"]}),
    );
    let sink = RecSink::default();
    let prior = json!({"v": 1, "delta_link": link(&h, "token=d1"), "root": "ROOT", "folders": {
        "ROOT": ["", ""], "DOCS": ["ROOT", "Documents"], "OLD": ["DOCS", "Old"], "SUB": ["OLD", "Sub"]
    }});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["-IN_OLD", "-IN_SUB", "-GONE", "-MOVED", "+NEW"]
    );
    assert_eq!(out["delta_link"], link(&h, "token=d2"));
    assert!(out["folders"].get("OLD").is_none() && out["folders"].get("SUB").is_none());
    let got = dl.received_requests().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].url.path(), "/dl/new");
}

#[tokio::test]
async fn folder_rename_with_filters_reapplies_them() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    let d = |n: &str| Some(format!("{}/dl/{n}", dl.uri()));
    h.know("F1", &version("F1", "PROJ"));
    // "Projects" was renamed "Archive": it is no longer included.
    mount_token(
        &h,
        "d1",
        page(
            &h,
            vec![folder("PROJ", "ROOT", "Archive")],
            None,
            Some("token=d2"),
        ),
    )
    .await;
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                folder("PROJ", "ROOT", "Archive"),
                file("F1", "PROJ", "plan.md", 10, d("f1")),
                folder("NEWP", "ROOT", "Projects"),
                file("F2", "NEWP", "fresh.md", 10, d("f2")),
            ],
            None,
            Some("token=d3"),
        ),
    )
    .await;
    let conn = drive(
        &h,
        h.endpoints(),
        json!({"client_id": "cid", "include_folders": ["/Projects"]}),
    );
    let sink = RecSink::default();
    let prior = json!({"v": 1, "delta_link": link(&h, "token=d1"), "root": "ROOT", "folders": {
        "ROOT": ["", ""], "PROJ": ["ROOT", "Projects"]
    }});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-F1", "+F2"]);
    assert_eq!(out["delta_link"], link(&h, "token=d3"));
}

#[tokio::test]
async fn disallowed_download_hosts_are_refused() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    // Production rules on the download side: HTTPS only, Microsoft hosts only.
    let endpoints = MicrosoftEndpoints {
        allow_http_downloads: false,
        ..h.endpoints()
    };
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                // Plain HTTP (even to an allowed host) and a foreign host: both refused, then /content.
                file("A", "ROOT", "a.txt", 10, Some(format!("{}/dl/a", dl.uri()))),
                file(
                    "B",
                    "ROOT",
                    "b.txt",
                    10,
                    Some("https://evil.example/steal".into()),
                ),
            ],
            None,
            Some("token=d"),
        ),
    )
    .await;
    mount_content(&h, "A", "https://attacker.example/a").await;
    mount_content(&h, "B", &format!("{}/dl/b", dl.uri())).await;
    let conn = drive(&h, endpoints, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert!(sink.summary().is_empty(), "{:?}", sink.summary());
    assert!(dl.received_requests().await.unwrap().is_empty());
    // Only Graph was contacted.
    let events = h.egress.0.lock().unwrap();
    assert!(events.iter().all(|e| e.host == host_of(&h.uri())));
}

#[tokio::test]
async fn expired_download_url_falls_back_to_content_redirect() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dl/old"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&dl)
        .await;
    Mock::given(method("GET"))
        .and(path("/dl/new"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&dl)
        .await;
    Mock::given(method("GET"))
        .and(path("/dl/new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&dl)
        .await;
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                file(
                    "A",
                    "ROOT",
                    "a.txt",
                    5,
                    Some(format!("{}/dl/old", dl.uri())),
                ),
            ],
            None,
            Some("token=d"),
        ),
    )
    .await;
    mount_content(&h, "A", &format!("{}/dl/new", dl.uri())).await;
    let conn = h.build(
        &OneDriveFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(2)),
        json!({"client_id": "cid"}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+A"]);
    assert!(matches!(&sink.upserts()[0].content, Content::Bytes(b) if b == b"hello"));
    let got = dl.received_requests().await.unwrap();
    assert_eq!(got.len(), 3);
    assert_no_bearer(&got);
}

#[tokio::test]
async fn expired_delta_token_reconciles_the_whole_drive() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    h.know("STAYS", &version("STAYS", "ROOT"));
    h.know("VANISHED", &version("VANISHED", "ROOT"));
    mount_token(
        &h,
        "stale",
        ResponseTemplate::new(410).set_body_json(json!({
            "error": {"code": "resyncRequired", "message": "Resync required."}
        })),
    )
    .await;
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                file(
                    "STAYS",
                    "ROOT",
                    "stays.txt",
                    10,
                    Some(format!("{}/dl/stays", dl.uri())),
                ),
            ],
            None,
            Some("token=fresh"),
        ),
    )
    .await;
    let conn = drive(&h, h.endpoints(), json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let prior = json!({"v": 1, "delta_link": link(&h, "token=stale"), "folders": {}});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-VANISHED"]);
    assert_eq!(out["delta_link"], link(&h, "token=fresh"));
    // Unchanged: not downloaded again.
    assert!(dl.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn reset_cursor_reconciles_against_new_filters() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    h.know("W", &version("W", "WORK"));
    h.know("P", &version("P", "PICS"));
    mount_initial(
        &h,
        page(
            &h,
            vec![
                root(),
                folder("WORK", "ROOT", "Work"),
                folder("PICS", "ROOT", "Pictures"),
                file("W", "WORK", "w.txt", 10, Some(format!("{}/dl/w", dl.uri()))),
                file("P", "PICS", "p.txt", 10, Some(format!("{}/dl/p", dl.uri()))),
            ],
            Some("token=p2"),
            None,
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param("token", "p2"))
        .respond_with(page(&h, vec![], None, Some("token=d")))
        .mount(&h.server)
        .await;
    let conn = drive(
        &h,
        h.endpoints(),
        json!({"client_id": "cid", "exclude_folders": ["/Pictures"]}),
    );
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-P"]);
    assert_eq!(out["delta_link"], link(&h, "token=d"));
    assert!(dl.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn hostile_drive_payloads_are_contained() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let dl = download_host().await;
    let evil = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&evil)
        .await;
    mount_initial(&h, page(&h, vec![
        root(),
        json!("junk"),
        json!({"id": "../x", "name": "a.txt", "file": {}, "parentReference": {"id": "ROOT"}}),
        // A folder cycle: its files have no path.
        folder("C1", "C2", "a"),
        folder("C2", "C1", "b"),
        file("INCYCLE", "C1", "c.txt", 10, Some(format!("{}/dl/c", dl.uri()))),
        file("NEG", "ROOT", "neg.txt", 0, None),
        json!({"id": "SHARED", "name": "s.txt", "file": {}, "remoteItem": {"id": "x"}, "parentReference": {"id": "ROOT"}}),
        file("CTRL", "ROOT", "evil\u{0}\nname.txt", 10, Some(format!("{}/dl/ctrl", dl.uri()))),
    ], None, Some("token=d"))).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/NEG/content")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    // With a folder filter the cycle's file is left alone; files at the root are in scope.
    let conn = drive(
        &h,
        h.endpoints(),
        json!({"client_id": "cid", "exclude_folders": ["/Secret"]}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+CTRL"]);
    assert_eq!(sink.upserts()[0].title.as_deref(), Some("evil  name.txt"));

    // A next link to another host is refused.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        ResponseTemplate::new(200).set_body_json(json!({
            "value": [], "@odata.nextLink": format!("{}{DELTA}?token=x", evil.uri())
        })),
    )
    .await;
    let conn = drive(&h, h.endpoints(), json!({"client_id": "cid"}));
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::Invalid(_))
    ));
}
