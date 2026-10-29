//! `gdrive` sync against a mock Drive (plus the Sheets and Slides APIs for content).

use nugget_connector::{Content, Document};
use serde_json::json;
use wiremock::matchers::{header, method, path, path_regex, query_param, query_param_is_missing};
use wiremock::{Mock, Request, ResponseTemplate};

use super::*;
use crate::DriveFactory;

const API: &str = "/drive/v3";
const DOC: &str = "application/vnd.google-apps.document";
const SHEET: &str = "application/vnd.google-apps.spreadsheet";
const SLIDES: &str = "application/vnd.google-apps.presentation";
const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
const MIB: u64 = 1024 * 1024;

fn gdrive(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    gdrive_with(h, settings, fast_retry(2))
}

fn gdrive_with(h: &Harness, settings: Value, retry: RetryPolicy) -> Arc<dyn Connector> {
    h.build(
        &DriveFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(retry),
        settings,
    )
}

fn mine() -> Value {
    json!({"client_id": "cid"})
}

fn link(id: &str) -> String {
    format!("https://drive.google.com/file/d/{id}/view")
}

/// A file owned by the user, outside shared drives.
fn file(id: &str, name: &str, mime: &str, version: &str) -> Value {
    json!({
        "id": id, "name": name, "mimeType": mime, "version": version,
        "modifiedTime": "2025-10-01T12:00:00.000Z", "createdTime": "2025-09-01T12:00:00.000Z",
        "trashed": false, "parents": ["root"], "ownedByMe": true, "shared": false,
        "webViewLink": link(id),
        "owners": [{"displayName": "Me", "emailAddress": "me@example.com"}],
    })
}

fn sized(mut f: Value, size: u64) -> Value {
    f["size"] = json!(size.to_string());
    f
}

fn with(mut f: Value, key: &str, v: Value) -> Value {
    f[key] = v;
    f
}

fn files_page(files: Vec<Value>, next: Option<&str>) -> ResponseTemplate {
    let mut body = json!({"kind": "drive#fileList", "files": files});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn changes_page(
    changes: Vec<Value>,
    next: Option<&str>,
    new_start: Option<&str>,
) -> ResponseTemplate {
    let mut body = json!({"kind": "drive#changeList", "changes": changes});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    if let Some(s) = new_start {
        body["newStartPageToken"] = s.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn file_change(f: Value) -> Value {
    json!({"changeType": "file", "removed": false, "fileId": f["id"].clone(), "file": f})
}

fn removed(id: &str) -> Value {
    json!({"changeType": "file", "removed": true, "fileId": id})
}

async fn mount_start(h: &Harness, token: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes/startPageToken")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"startPageToken": token})))
        .mount(&h.server)
        .await;
}

/// `files.list`: the first page (no `page_token`) or a later one.
async fn mount_files(h: &Harness, page_token: Option<&str>, resp: ResponseTemplate) {
    let m = Mock::given(method("GET")).and(path(format!("{API}/files")));
    let m = match page_token {
        Some(p) => m.and(query_param("pageToken", p)),
        None => m.and(query_param_is_missing("pageToken")),
    };
    m.respond_with(resp).mount(&h.server).await;
}

async fn mount_changes(h: &Harness, token: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .and(query_param("pageToken", token))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

async fn mount_export(h: &Harness, id: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/files/{id}/export")))
        .and(query_param("mimeType", "text/markdown"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&h.server)
        .await;
}

async fn mount_download(h: &Harness, id: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/files/{id}")))
        .and(query_param("alt", "media"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(&h.server)
        .await;
}

async fn requests(h: &Harness) -> Vec<Request> {
    h.server.received_requests().await.unwrap()
}

/// Requests whose path is exactly `p`.
async fn hits(h: &Harness, p: &str) -> Vec<Request> {
    requests(h)
        .await
        .into_iter()
        .filter(|r| r.url.path() == p)
        .collect()
}

fn query(r: &Request) -> HashMap<String, String> {
    r.url.query_pairs().into_owned().collect()
}

fn doc<'a>(docs: &'a [Document], key: &str) -> &'a Document {
    docs.iter()
        .find(|d| d.key.as_str() == key)
        .unwrap_or_else(|| panic!("no upsert for {key}"))
}

fn text(d: &Document) -> &str {
    match &d.content {
        Content::Text(t) => t,
        other => panic!("text content expected, got {other:?}"),
    }
}

fn checkpoints(sink: &RecSink) -> Vec<Value> {
    sink.checkpoints.lock().unwrap().clone()
}

#[tokio::test]
async fn sync_without_tokens_is_unauthorized() {
    let h = Harness::new().await;
    let conn = gdrive(&h, mine());
    assert!(is_unauthorized(
        &run(&conn, None, &RecSink::default()).await
    ));
    assert!(requests(&h).await.is_empty());
}

#[tokio::test]
async fn first_sync_lists_pages_fetches_content_and_continues_into_changes() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        files_page(vec![file("d1", "Plan", DOC, "5")], Some("p2")),
    )
    .await;
    mount_files(
        &h,
        Some("p2"),
        files_page(
            vec![
                sized(file("f2", "Report.pdf", "application/pdf", "3"), 9),
                sized(file("i3", "Photo.png", "image/png", "2"), 2048),
                sized(file("b4", "Huge.pdf", "application/pdf", "7"), 30 * MIB),
                file("s5", "Budget", SHEET, "4"),
                file("l6", "Deck", SLIDES, "6"),
                file("fo", "Folder", FOLDER, "1"),
                file("sc", "Shortcut", SHORTCUT, "1"),
            ],
            None,
        ),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    mount_export(&h, "d1", "# Plan\n\nShip it.").await;
    mount_download(&h, "f2", b"%PDF-1.4").await;
    Mock::given(method("GET"))
        .and(path("/sheets/v4/spreadsheets/s5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sheets": [{"properties": {"title": "Q1", "sheetType": "GRID"}}]
        })))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/sheets/v4/spreadsheets/s5/values/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"values": [["item", "cost"], ["rent", "1,000"]]})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slides/v1/presentations/l6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"slides": [{
            "pageElements": [{"shape": {"text": {"textElements": [
                {"textRun": {"content": "Welcome\n"}}
            ]}}}]
        }]})))
        .mount(&h.server)
        .await;

    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    assert_eq!(out, json!({"v": 1, "changes_token": "101"}));
    assert_eq!(
        sink.summary(),
        vec!["+d1", "+f2", "+i3", "+b4", "+s5", "+l6"],
        "folders and shortcuts are not documents"
    );
    // The start token is saved before listing, then every page, then the Changes pass.
    assert_eq!(
        checkpoints(&sink),
        vec![
            json!({"v": 1, "full": true, "start_token": "100"}),
            json!({"v": 1, "full": true, "start_token": "100", "page_token": "p2"}),
            json!({"v": 1, "changes_token": "100"}),
            json!({"v": 1, "changes_token": "101"}),
        ]
    );

    let docs = sink.upserts();
    let d1 = doc(&docs, "d1");
    assert_eq!(d1.mime, "text/markdown");
    assert_eq!(text(d1), "# Plan\n\nShip it.");
    assert_eq!(d1.title.as_deref(), Some("Plan"));
    assert_eq!(d1.url.as_deref(), Some(link("d1").as_str()));
    assert_eq!(d1.version.as_deref(), Some("5|"));
    assert_eq!(d1.modified_at.unwrap().year(), 2025);
    assert_eq!(d1.metadata["file_id"], "d1");
    assert_eq!(d1.metadata["mime_type"], DOC);
    assert_eq!(d1.metadata["via"], "export:markdown");
    assert_eq!(d1.metadata["parents"], json!(["root"]));
    assert_eq!(d1.metadata["owned_by_me"], true);
    assert_eq!(d1.metadata["owners"][0]["email"], "me@example.com");
    assert!(d1.metadata["drive_id"].is_null());
    assert!(d1.metadata.get("skipped").is_none());

    let f2 = doc(&docs, "f2");
    assert_eq!(f2.mime, "application/pdf");
    assert_eq!(f2.content, Content::Bytes(b"%PDF-1.4".to_vec()));
    assert_eq!(f2.metadata["via"], "download");
    assert_eq!(f2.metadata["size"], 9);
    assert_eq!(f2.version.as_deref(), Some("3|"));

    let i3 = doc(&docs, "i3");
    assert_eq!(i3.mime, "text/plain");
    assert_eq!(
        text(i3),
        "Photo.png\n\nNot indexed: Images are not indexed."
    );
    assert_eq!(i3.metadata["skipped"], "Images are not indexed.");
    assert!(i3.metadata["via"].is_null());
    assert_eq!(i3.title.as_deref(), Some("Photo.png"));
    assert_eq!(i3.url.as_deref(), Some(link("i3").as_str()));
    assert_eq!(i3.version.as_deref(), Some("2||skip25"));

    let b4 = doc(&docs, "b4");
    assert_eq!(b4.mime, "text/plain");
    assert_eq!(b4.metadata["skipped"], "Larger than the 25 MB limit.");
    assert_eq!(
        text(b4),
        "Huge.pdf\n\nNot indexed: Larger than the 25 MB limit."
    );
    assert_eq!(b4.version.as_deref(), Some("7||skip25"));

    let s5 = doc(&docs, "s5");
    assert_eq!(s5.mime, "text/markdown");
    assert_eq!(s5.metadata["via"], "sheets:csv");
    assert_eq!(text(s5), "## Q1\n\nitem,cost\nrent,\"1,000\"\n\n");

    let l6 = doc(&docs, "l6");
    assert_eq!(l6.mime, "text/markdown");
    assert_eq!(l6.metadata["via"], "slides");
    assert!(text(l6).contains("## Slide 1\n\nWelcome"), "{}", text(l6));

    // Nothing was downloaded for skipped files, folders or shortcuts.
    for id in ["i3", "b4", "fo", "sc"] {
        assert!(
            hits(&h, &format!("{API}/files/{id}")).await.is_empty(),
            "{id}"
        );
    }
    // Only my files, in my corpus.
    let lists = hits(&h, &format!("{API}/files")).await;
    assert_eq!(lists.len(), 2);
    for r in &lists {
        let q = query(r);
        assert_eq!(q["q"], "trashed=false and 'me' in owners");
        assert_eq!(q["corpora"], "user");
        assert!(!q.contains_key("includeItemsFromAllDrives"));
        assert!(!q.contains_key("supportsAllDrives"));
        assert!(q["fields"].contains("nextPageToken"));
    }
    let changes = hits(&h, &format!("{API}/changes")).await;
    assert_eq!(changes.len(), 1);
    let q = query(&changes[0]);
    assert_eq!(q["includeRemoved"], "true");
    assert_eq!(q["spaces"], "drive");
    assert!(!q.contains_key("includeItemsFromAllDrives"));
    // Every request is authenticated and in the ledger.
    assert!(requests(&h).await.iter().all(|r| {
        r.headers
            .get("authorization")
            .is_some_and(|v| v == "Bearer at")
    }));
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn raising_the_size_cap_fetches_stubbed_files_again() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    // Stubbed at 25 MB; now the cap is 50 MB.
    h.know("b4", "7||skip25");
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        files_page(
            vec![sized(
                file("b4", "Huge.pdf", "application/pdf", "7"),
                30 * MIB,
            )],
            None,
        ),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    mount_download(&h, "b4", b"%PDF").await;
    let conn = gdrive(&h, json!({"client_id": "cid", "max_file_mb": 50}));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+b4"]);
    let b4 = &sink.upserts()[0];
    assert_eq!(b4.mime, "application/pdf");
    assert_eq!(b4.version.as_deref(), Some("7|"));
}

#[tokio::test]
async fn unchanged_known_versions_fetch_no_content() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("d1", "5|");
    h.know("i3", "2||skip25");
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        files_page(
            vec![
                file("d1", "Plan", DOC, "5"),
                sized(file("i3", "Photo.png", "image/png", "2"), 10),
            ],
            None,
        ),
    )
    .await;
    mount_changes(
        &h,
        "100",
        changes_page(
            vec![file_change(file("d1", "Plan", DOC, "5"))],
            None,
            Some("101"),
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/files/d1/export")))
        .respond_with(ResponseTemplate::new(200).set_body_string("x"))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    assert!(sink.changes.lock().unwrap().is_empty());
    assert_eq!(out["changes_token"], "101");
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn incremental_changes_modify_rename_trash_and_remove() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    for k in ["m1", "r1", "t1", "x1", "o1"] {
        h.know(k, "1|");
    }
    let renamed = with(
        sized(file("r1", "New name.txt", "text/plain", "2"), 5),
        "parents",
        json!(["folderB"]),
    );
    let theirs = with(
        file("o1", "Now theirs", DOC, "2"),
        "ownedByMe",
        json!(false),
    );
    mount_changes(
        &h,
        "200",
        changes_page(
            vec![
                file_change(file("m1", "Notes", DOC, "2")),
                file_change(renamed),
                file_change(with(file("t1", "Old", DOC, "2"), "trashed", json!(true))),
            ],
            Some("201"),
            None,
        ),
    )
    .await;
    mount_changes(
        &h,
        "201",
        changes_page(
            vec![
                removed("x1"),
                removed("never-known"),
                file_change(file("fo", "A folder", FOLDER, "3")),
                // No longer owned by me (and "shared with me" is off): out of scope.
                file_change(theirs),
            ],
            None,
            Some("300"),
        ),
    )
    .await;
    mount_export(&h, "m1", "new notes").await;
    mount_download(&h, "r1", b"hello").await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let out = run(&conn, Some(json!({"v": 1, "changes_token": "200"})), &sink)
        .await
        .unwrap();
    assert_eq!(out, json!({"v": 1, "changes_token": "300"}));
    assert_eq!(sink.summary(), vec!["+m1", "+r1", "-t1", "-x1", "-o1"]);
    assert_eq!(
        checkpoints(&sink),
        vec![
            json!({"v": 1, "changes_token": "201"}),
            json!({"v": 1, "changes_token": "300"}),
        ]
    );
    let docs = sink.upserts();
    assert_eq!(text(doc(&docs, "m1")), "new notes");
    assert_eq!(doc(&docs, "m1").version.as_deref(), Some("2|"));
    let r1 = doc(&docs, "r1");
    assert_eq!(r1.title.as_deref(), Some("New name.txt"));
    assert_eq!(r1.metadata["name"], "New name.txt");
    assert_eq!(r1.metadata["parents"], json!(["folderB"]));
    assert_eq!(r1.mime, "text/plain");
    assert_eq!(r1.content, Content::Bytes(b"hello".to_vec()));
    // An incremental pass neither lists nor takes a new start token.
    assert!(hits(&h, &format!("{API}/files")).await.is_empty());
    assert!(
        hits(&h, &format!("{API}/changes/startPageToken"))
            .await
            .is_empty()
    );
    assert!(hits(&h, &format!("{API}/files/o1/export")).await.is_empty());
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn removed_shared_drive_deletes_its_files() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("s1", "1|0AD");
    h.know("s2", "4|0AD|skip25");
    h.know("s3", "1|0OTHER");
    h.know("m1", "1|");
    mount_changes(
        &h,
        "200",
        changes_page(
            vec![
                json!({"changeType": "drive", "driveId": "0OTHER", "removed": false,
                       "drive": {"id": "0OTHER", "name": "Renamed"}}),
                json!({"changeType": "drive", "driveId": "0AD", "removed": true}),
                json!({"changeType": "drive", "driveId": "../bad", "removed": true}),
            ],
            None,
            Some("201"),
        ),
    )
    .await;
    let conn = gdrive(
        &h,
        json!({"client_id": "cid", "include_shared_drives": true}),
    );
    let sink = RecSink::default();
    let out = run(&conn, Some(json!({"v": 1, "changes_token": "200"})), &sink)
        .await
        .unwrap();
    assert_eq!(out["changes_token"], "201");
    assert_eq!(sink.summary(), vec!["-s1", "-s2"]);
    let q = query(&hits(&h, &format!("{API}/changes")).await[0]);
    assert_eq!(q["includeItemsFromAllDrives"], "true");
    assert_eq!(q["supportsAllDrives"], "true");
    assert_eq!(q["includeRemoved"], "true");
}

/// A listing with one file of mine, one shared with me and one in a shared drive.
async fn mount_mixed_listing(h: &Harness) {
    mount_start(h, "100").await;
    mount_files(
        h,
        None,
        files_page(
            vec![
                sized(file("mine", "Mine.txt", "text/plain", "1"), 4),
                sized(
                    with(
                        file("theirs", "Theirs.txt", "text/plain", "1"),
                        "ownedByMe",
                        json!(false),
                    ),
                    4,
                ),
                sized(
                    with(
                        with(
                            file("team", "Team.txt", "text/plain", "1"),
                            "driveId",
                            json!("0AD"),
                        ),
                        "ownedByMe",
                        json!(false),
                    ),
                    4,
                ),
            ],
            None,
        ),
    )
    .await;
    mount_changes(h, "100", changes_page(vec![], None, Some("101"))).await;
    for id in ["mine", "theirs", "team"] {
        mount_download(h, id, b"text").await;
    }
}

#[tokio::test]
async fn shared_files_are_out_of_scope_by_default() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_mixed_listing(&h).await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+mine"]);
    assert!(hits(&h, &format!("{API}/files/theirs")).await.is_empty());
    assert!(hits(&h, &format!("{API}/files/team")).await.is_empty());
}

#[tokio::test]
async fn shared_with_me_only_when_opted_in() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_mixed_listing(&h).await;
    let conn = gdrive(
        &h,
        json!({"client_id": "cid", "include_shared_with_me": true}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+mine", "+theirs"]);
    let q = query(&hits(&h, &format!("{API}/files")).await[0]);
    assert_eq!(q["q"], "trashed=false");
    assert_eq!(q["corpora"], "user");
    assert!(!q.contains_key("includeItemsFromAllDrives"));
    assert!(!q.contains_key("supportsAllDrives"));
}

#[tokio::test]
async fn shared_drives_only_when_opted_in() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_mixed_listing(&h).await;
    let conn = gdrive(
        &h,
        json!({"client_id": "cid", "include_shared_drives": true, "include_shared_with_me": true}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+mine", "+theirs", "+team"]);
    let q = query(&hits(&h, &format!("{API}/files")).await[0]);
    assert_eq!(q["q"], "trashed=false");
    assert_eq!(q["corpora"], "allDrives");
    assert_eq!(q["includeItemsFromAllDrives"], "true");
    assert_eq!(q["supportsAllDrives"], "true");
    let changes = query(&hits(&h, &format!("{API}/changes")).await[0]);
    assert_eq!(changes["includeItemsFromAllDrives"], "true");
    assert_eq!(changes["supportsAllDrives"], "true");
    // Shared-drive downloads say so; others do not need to.
    let team = query(&hits(&h, &format!("{API}/files/team")).await[0]);
    assert_eq!(team["supportsAllDrives"], "true");
    let docs = sink.upserts();
    let team_doc = doc(&docs, "team");
    assert_eq!(team_doc.version.as_deref(), Some("1|0AD"));
    assert_eq!(team_doc.metadata["drive_id"], "0AD");

    // Shared drives without "shared with me": files shared with me stay out.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_mixed_listing(&h).await;
    let conn = gdrive(
        &h,
        json!({"client_id": "cid", "include_shared_drives": true}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+mine", "+team"]);
}

#[tokio::test]
async fn resumes_a_listing_from_its_page_token() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes/startPageToken")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"startPageToken": "999"})))
        .expect(0)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/files")))
        .and(query_param_is_missing("pageToken"))
        .respond_with(files_page(vec![], None))
        .expect(0)
        .mount(&h.server)
        .await;
    mount_files(
        &h,
        Some("p2"),
        files_page(vec![file("d2", "Second", DOC, "1")], None),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    mount_export(&h, "d2", "two").await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let cursor = json!({"v": 1, "full": true, "start_token": "100", "page_token": "p2"});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+d2"]);
    assert_eq!(out, json!({"v": 1, "changes_token": "101"}));
}

#[tokio::test]
async fn an_interrupted_reconciling_listing_restarts_from_the_first_page() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("d1", "1|");
    h.know("d2", "1|");
    h.know("gone", "1|");
    Mock::given(method("GET"))
        .and(path(format!("{API}/files")))
        .and(query_param("pageToken", "p2"))
        .respond_with(files_page(vec![file("d2", "Second", DOC, "1")], None))
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    mount_files(
        &h,
        None,
        files_page(vec![file("d1", "First", DOC, "1")], Some("p2")),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let cursor =
        json!({"v": 1, "full": true, "reconcile": true, "start_token": "100", "page_token": "p2"});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    // d1 (listed on page 1) is kept: only the missing file goes.
    assert_eq!(sink.summary(), vec!["-gone"]);
    assert_eq!(out["changes_token"], "101");
    let lists = hits(&h, &format!("{API}/files")).await;
    assert_eq!(lists.len(), 2);
    assert!(!query(&lists[0]).contains_key("pageToken"));
    assert_eq!(
        checkpoints(&sink)[0],
        json!({"v": 1, "full": true, "reconcile": true, "start_token": "100", "page_token": "p2"})
    );
}

#[tokio::test]
async fn expired_listing_page_token_starts_a_reconciling_listing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("keep", "1|");
    h.know("gone", "1|");
    mount_files(&h, Some("p2"), ResponseTemplate::new(410)).await;
    mount_start(&h, "500").await;
    mount_files(
        &h,
        None,
        files_page(vec![file("keep", "Keep", DOC, "1")], None),
    )
    .await;
    mount_changes(&h, "500", changes_page(vec![], None, Some("501"))).await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let cursor = json!({"v": 1, "full": true, "start_token": "100", "page_token": "p2"});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(out, json!({"v": 1, "changes_token": "501"}));
    assert_eq!(sink.summary(), vec!["-gone"]);
    // The old start token is dropped with the expired listing.
    assert!(
        hits(&h, &format!("{API}/changes"))
            .await
            .iter()
            .all(|r| query(r)["pageToken"] == "500")
    );
}

#[tokio::test]
async fn reconciling_pass_deletes_files_no_longer_listed() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("keep", "1|");
    h.know("gone-a", "1|");
    h.know("gone-b", "3||skip25");
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        files_page(vec![file("keep", "Keep", DOC, "1")], None),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-gone-a", "-gone-b"]);
    assert_eq!(
        checkpoints(&sink)[0],
        json!({"v": 1, "full": true, "reconcile": true, "start_token": "100"})
    );
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn incomplete_search_keeps_unlisted_files() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("keep", "1|");
    h.know("maybe", "1|");
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        ResponseTemplate::new(200).set_body_json(json!({
            "files": [file("keep", "Keep", DOC, "1")], "incompleteSearch": true
        })),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert!(sink.summary().is_empty());
}

#[tokio::test]
async fn expired_changes_token_starts_a_reconciling_listing() {
    for status in [404u16, 410] {
        let h = Harness::new().await;
        h.seed_tokens("at", false);
        h.know("keep", "1|");
        h.know("gone", "1|");
        mount_changes(&h, "stale", ResponseTemplate::new(status)).await;
        mount_start(&h, "500").await;
        mount_files(
            &h,
            None,
            files_page(
                vec![file("keep", "Keep", DOC, "1"), file("new", "New", DOC, "1")],
                None,
            ),
        )
        .await;
        mount_changes(&h, "500", changes_page(vec![], None, Some("501"))).await;
        mount_export(&h, "new", "fresh").await;
        let conn = gdrive(&h, mine());
        let sink = RecSink::default();
        let out = run(
            &conn,
            Some(json!({"v": 1, "changes_token": "stale"})),
            &sink,
        )
        .await
        .unwrap();
        assert_eq!(out, json!({"v": 1, "changes_token": "501"}), "{status}");
        assert_eq!(sink.summary(), vec!["+new", "-gone"], "{status}");
        assert_eq!(
            checkpoints(&sink)[0],
            json!({"v": 1, "full": true, "reconcile": true, "start_token": "500"})
        );
        h.assert_egress("sync:gdrive").await;
    }
}

#[tokio::test]
async fn tokens_that_keep_expiring_end_the_pass() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "500").await;
    mount_files(&h, None, files_page(vec![], None)).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .respond_with(ResponseTemplate::new(410))
        .mount(&h.server)
        .await;
    let conn = gdrive(&h, mine());
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            &conn,
            Some(json!({"v": 1, "changes_token": "stale"})),
            &RecSink::default(),
        ),
    )
    .await
    .expect("the pass never ended");
    assert!(
        matches!(r, Err(Error::Other(ref m)) if m.contains("expiring")),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_repeating_page_token_stops_the_listing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    mount_files(&h, None, files_page(vec![], Some("p2"))).await;
    mount_files(&h, Some("p2"), files_page(vec![], Some("p2"))).await;
    let conn = gdrive(&h, mine());
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        run(&conn, None, &RecSink::default()),
    )
    .await
    .expect("listing never ended");
    assert!(
        matches!(&r, Err(Error::Network(m)) if m.contains("repeated a page token")),
        "{r:?}"
    );
    assert!(requests(&h).await.len() <= 4);

    // Same for changes.list.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_changes(&h, "200", changes_page(vec![], Some("200"), None)).await;
    let conn = gdrive(&h, mine());
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            &conn,
            Some(json!({"v": 1, "changes_token": "200"})),
            &RecSink::default(),
        ),
    )
    .await
    .expect("changes never ended");
    assert!(
        matches!(&r, Err(Error::Network(m)) if m.contains("repeated a page token")),
        "{r:?}"
    );
}

#[tokio::test]
async fn rate_limit_with_retry_after_is_retried() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/files")))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/files")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "User rate limit exceeded",
            "errors": [{"reason": "userRateLimitExceeded", "domain": "usageLimits"}]
        }})))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&h.server)
        .await;
    mount_files(
        &h,
        None,
        files_page(vec![file("d1", "Plan", DOC, "1")], None),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    mount_export(&h, "d1", "plan").await;
    let conn = gdrive_with(&h, mine(), fast_retry(3));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+d1"]);
    assert_eq!(hits(&h, &format!("{API}/files")).await.len(), 3);
    // Every attempt is in the ledger.
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn server_errors_are_retried() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.server)
        .await;
    mount_changes(
        &h,
        "200",
        changes_page(vec![removed("x1")], None, Some("201")),
    )
    .await;
    h.know("x1", "1|");
    let conn = gdrive_with(&h, mine(), fast_retry(2));
    let sink = RecSink::default();
    let out = run(&conn, Some(json!({"v": 1, "changes_token": "200"})), &sink)
        .await
        .unwrap();
    assert_eq!(out["changes_token"], "201");
    assert_eq!(sink.summary(), vec!["-x1"]);
    assert_eq!(hits(&h, &format!("{API}/changes")).await.len(), 3);

    // Persistent 5xx: a network error after the retries, the cursor untouched.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&h.server)
        .await;
    let conn = gdrive_with(&h, mine(), fast_retry(2));
    let sink = RecSink::default();
    let r = run(&conn, Some(json!({"v": 1, "changes_token": "200"})), &sink).await;
    assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
    assert!(checkpoints(&sink).is_empty());
}

#[tokio::test]
async fn daily_quota_is_rate_limited_with_a_long_hint() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "Daily Limit Exceeded",
            "errors": [{"reason": "dailyLimitExceeded", "domain": "usageLimits"}]
        }})))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gdrive_with(&h, mine(), fast_retry(3));
    let r = run(
        &conn,
        Some(json!({"v": 1, "changes_token": "200"})),
        &RecSink::default(),
    )
    .await;
    assert!(
        matches!(
            r,
            Err(Error::RateLimited {
                retry_after_ms: Some(3_600_000)
            })
        ),
        "{r:?}"
    );
}

#[tokio::test]
async fn api_401_refreshes_the_token_once_then_succeeds() {
    let h = Harness::new().await;
    h.seed_tokens("stale", false);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/changes")))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(changes_page(vec![], None, Some("201")))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gdrive(&h, mine());
    let out = run(
        &conn,
        Some(json!({"v": 1, "changes_token": "200"})),
        &RecSink::default(),
    )
    .await
    .unwrap();
    assert_eq!(out["changes_token"], "201");
    // The refresh is metered under the same feature.
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn revoked_refresh_token_is_unauthorized() {
    // Expired access token: the refresh happens before any Drive request.
    let h = Harness::new().await;
    h.seed_tokens("old", true);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&h.server)
        .await;
    let conn = gdrive(&h, mine());
    let r = run(&conn, None, &RecSink::default()).await;
    assert!(is_unauthorized(&r), "{r:?}");
    assert!(
        requests(&h)
            .await
            .iter()
            .all(|r| !r.url.path().starts_with(API))
    );
    h.assert_egress("sync:gdrive").await;

    // A 401 mid-pass whose refresh is refused.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.server)
        .await;
    let conn = gdrive(&h, mine());
    let r = run(
        &conn,
        Some(json!({"v": 1, "changes_token": "200"})),
        &RecSink::default(),
    )
    .await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn malformed_responses_are_errors_not_panics() {
    // files.list that is not JSON.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        ResponseTemplate::new(200).set_body_string("<html>nope"),
    )
    .await;
    let conn = gdrive(&h, mine());
    let r = run(&conn, None, &RecSink::default()).await;
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    // No usable start token.
    for body in [
        json!({}),
        json!({"startPageToken": "has space"}),
        json!([1, 2]),
    ] {
        let h = Harness::new().await;
        h.seed_tokens("at", false);
        Mock::given(method("GET"))
            .and(path(format!("{API}/changes/startPageToken")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .mount(&h.server)
            .await;
        let conn = gdrive(&h, mine());
        let r = run(&conn, None, &RecSink::default()).await;
        assert!(matches!(r, Err(Error::Invalid(_))), "{body}: {r:?}");
    }

    // A changes page with neither token, or with an unusable one.
    for body in [
        json!({"changes": []}),
        json!({"changes": [], "newStartPageToken": "bad token"}),
        json!({"changes": "not a list"}),
    ] {
        let h = Harness::new().await;
        h.seed_tokens("at", false);
        mount_changes(
            &h,
            "200",
            ResponseTemplate::new(200).set_body_json(body.clone()),
        )
        .await;
        let conn = gdrive(&h, mine());
        let sink = RecSink::default();
        let r = run(&conn, Some(json!({"v": 1, "changes_token": "200"})), &sink).await;
        assert!(matches!(r, Err(Error::Invalid(_))), "{body}: {r:?}");
        assert!(checkpoints(&sink).is_empty());
    }

    // A bad page token in files.list.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    mount_files(&h, None, files_page(vec![], Some("bad\ntoken"))).await;
    let conn = gdrive(&h, mine());
    let r = run(&conn, None, &RecSink::default()).await;
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[tokio::test]
async fn hostile_file_entries_are_contained() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    let long_name = "n".repeat(5000);
    mount_files(
        &h,
        None,
        files_page(
            vec![
                json!("not an object"),
                json!(42),
                json!({"id": 7, "name": "numeric id"}),
                file("../escape", "Traversal", DOC, "1"),
                file("a/b", "Slash", DOC, "1"),
                file("", "Empty", DOC, "1"),
                file(&"x".repeat(300), "Long id", DOC, "1"),
                // Unsafe links are dropped; the file is still indexed.
                with(
                    file("js", "Script link", DOC, "1"),
                    "webViewLink",
                    json!("javascript:alert(1)"),
                ),
                with(
                    file("http", "Plain http", DOC, "1"),
                    "webViewLink",
                    json!("http://drive.google.com/x"),
                ),
                with(
                    file("ws", "Spacey", DOC, "1"),
                    "webViewLink",
                    json!("https://drive.google.com/a b"),
                ),
                // A parents entry that is not a string: the whole entry is unreadable.
                with(
                    file("badparents", "Bad parents", DOC, "1"),
                    "parents",
                    json!(["ok", 5]),
                ),
                file("long", &long_name, DOC, "1"),
            ],
            None,
        ),
    )
    .await;
    mount_changes(
        &h,
        "100",
        changes_page(
            vec![
                json!("junk"),
                json!({"changeType": "file", "removed": true, "fileId": "../escape"}),
                json!({"changeType": "file", "file": "not an object"}),
                json!({"changeType": 5}),
            ],
            None,
            Some("101"),
        ),
    )
    .await;
    for id in ["js", "http", "ws", "long"] {
        mount_export(&h, id, "ok").await;
    }
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    assert_eq!(out["changes_token"], "101");
    assert_eq!(sink.summary(), vec!["+js", "+http", "+ws", "+long"]);
    let docs = sink.upserts();
    for id in ["js", "http", "ws"] {
        assert_eq!(doc(&docs, id).url, None, "{id}");
    }
    let long = doc(&docs, "long");
    assert_eq!(long.title.as_ref().unwrap().chars().count(), 400);
    // Nothing outside the Drive file paths was requested.
    for r in requests(&h).await {
        let p = r.url.path().to_string();
        assert!(!p.contains(".."), "{p}");
        assert!(!p.contains("escape"), "{p}");
    }
}

#[tokio::test]
async fn untrusted_names_and_versions_are_cleaned() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_start(&h, "100").await;
    mount_files(
        &h,
        None,
        files_page(
            vec![with(
                with(
                    file("ctl", "a\u{0}b\nc\u{202E}d", DOC, "1|9"),
                    "parents",
                    json!(["ok", "../bad"]),
                ),
                "modifiedTime",
                json!("not a time"),
            )],
            None,
        ),
    )
    .await;
    mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    mount_export(&h, "ctl", "ok").await;
    let conn = gdrive(&h, mine());
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    let docs = sink.upserts();
    let d = doc(&docs, "ctl");
    assert_eq!(d.title.as_deref(), Some("a b c d"));
    assert_eq!(d.metadata["name"], "a b c d");
    assert_eq!(d.metadata["parents"], json!(["ok"]));
    // `|` cannot leak into the version (it separates the drive id).
    assert_eq!(d.version.as_deref(), Some("19|"));
    assert!(d.modified_at.is_none());
}

#[tokio::test]
async fn unusable_cursors_start_over() {
    for cursor in [
        json!({"v": 2, "changes_token": "200"}),
        json!({"v": 1}),
        json!({"v": 1, "changes_token": "with space"}),
        json!("garbage"),
    ] {
        let h = Harness::new().await;
        h.seed_tokens("at", false);
        mount_start(&h, "100").await;
        mount_files(&h, None, files_page(vec![], None)).await;
        mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
        let conn = gdrive(&h, mine());
        let out = run(&conn, Some(cursor.clone()), &RecSink::default())
            .await
            .unwrap();
        assert_eq!(out, json!({"v": 1, "changes_token": "101"}), "{cursor}");
        assert_eq!(hits(&h, &format!("{API}/files")).await.len(), 1);
    }
}

#[tokio::test]
async fn cancelled_sync_stops_before_requests() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let conn = gdrive(&h, mine());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = conn
        .sync(
            Some(json!({"v": 1, "changes_token": "200"})),
            &RecSink::default(),
            &cancel,
        )
        .await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(requests(&h).await.is_empty());
}

#[test]
fn factory_validates_settings() {
    let f = DriveFactory::new();
    assert!(f.validate(&json!({"client_id": "x"})).is_ok());
    assert!(
        f.validate(&json!({
            "client_id": "x", "client_secret": "s", "account_hint": "a@b.c",
            "include_shared_with_me": true, "include_shared_drives": true, "max_file_mb": 200
        }))
        .is_ok()
    );
    assert!(
        f.validate(&json!({"client_id": "x", "max_file_mb": 1}))
            .is_ok()
    );
    assert!(
        f.validate(&json!({"client_id": "x", "max_file_mb": 0}))
            .is_err()
    );
    assert!(
        f.validate(&json!({"client_id": "x", "max_file_mb": 201}))
            .is_err()
    );
    assert!(
        f.validate(&json!({"client_id": "x", "max_file_mb": -1}))
            .is_err()
    );
    assert!(f.validate(&json!({"client_id": "x", "bogus": 1})).is_err());
    assert!(
        f.validate(&json!({"client_id": "x", "window_days": 30}))
            .is_err()
    );
    assert!(
        f.validate(&json!({"client_id": "x", "include_shared_drives": "yes"}))
            .is_err()
    );
    assert!(f.validate(&json!({})).is_err());
    assert!(f.validate(&Value::Null).is_err());
    assert!(f.validate(&json!({"client_id": " "})).is_err());
    assert!(f.validate(&json!({"client_id": "a b"})).is_err());
}

#[test]
fn factory_schema_has_drive_fields() {
    let info = DriveFactory::new().info();
    assert_eq!(info.kind, "gdrive");
    assert_eq!(info.display_name, "Google Drive");
    assert_eq!(info.auth, nugget_connector::AuthKind::OAuth);
    let s = &info.settings_schema;
    assert_eq!(s["required"], json!(["client_id"]));
    assert_eq!(s["additionalProperties"], json!(false));
    let p = &s["properties"];
    for k in ["client_id", "client_secret", "account_hint"] {
        assert!(p[k].is_object(), "{k}");
    }
    assert_eq!(p["include_shared_with_me"]["type"], "boolean");
    assert_eq!(p["include_shared_with_me"]["default"], false);
    assert_eq!(p["include_shared_drives"]["type"], "boolean");
    assert_eq!(p["include_shared_drives"]["default"], false);
    assert_eq!(p["max_file_mb"]["type"], "integer");
    assert_eq!(p["max_file_mb"]["minimum"], 1);
    assert_eq!(p["max_file_mb"]["maximum"], 200);
    assert_eq!(p["max_file_mb"]["default"], 25);
    assert!(p.get("window_days").is_none());
}

#[tokio::test]
async fn kind_poll_interval_and_consent_scope() {
    let h = Harness::new().await;
    let conn = h.build(
        &DriveFactory::new(),
        json!({"client_id": "cid", "account_hint": "me@example.com"}),
    );
    assert_eq!(conn.kind(), "gdrive");
    assert_eq!(conn.poll_interval(), Duration::from_secs(300));
    let flow = conn.begin_auth().await.unwrap();
    assert!(flow.url.starts_with(nugget_oauth::google::AUTH_URL));
    let q: HashMap<String, String> = url::Url::parse(&flow.url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(q["scope"], "https://www.googleapis.com/auth/drive.readonly");
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["login_hint"], "me@example.com");
    drop(flow);
}
