//! The Microsoft 365 and SaaS connectors through the real daemon: for each of `outlook`,
//! `outlook-calendar`, `onedrive`, `slack`, `notion`, `github` and `linear`, `sources.add` →
//! credential (`sources.secret.set`, or a seeded OAuth token set) → `sources.sync` → search
//! finds the mocked document. Every endpoint is a wiremock server; nothing leaves the machine.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::{Registry, SecretStore};
use nugget_connector_microsoft::{
    MicrosoftEndpoints, OneDriveFactory, OutlookCalendarFactory, OutlookFactory,
};
use nugget_connector_saas::{Endpoints, GitHubFactory, LinearFactory, NotionFactory, SlackFactory};
use nugget_core::ids::SourceId;
use nugget_core::ipc::{SearchHit, SourceInfo, SourceState, method};
use nugget_core::paths::Paths;
use nugget_core::secrets::oauth_account;
use nugget_ingest::MemorySecrets;
use nugget_ipc::Client;
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use wiremock::matchers::{body_string_contains, method as http_method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CALL: Duration = Duration::from_secs(20);
const EVENTUALLY: Duration = Duration::from_secs(45);
const NO_RUNTIME: &str = "[runtime]\nenabled = false\n";
const ENTRA_APP: &str = "00000000-1111-2222-3333-444444444444";

/// The shipped registry's new kinds, pointed at `base` with fast retries.
fn mocked_registry(base: &str) -> Registry {
    let ms = MicrosoftEndpoints::with_base(base);
    let ms_retry = nugget_connector_microsoft::RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(50),
    };
    let saas = Endpoints::with_base(base);
    let saas_retry = nugget_connector_saas::RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(50),
    };
    let mut r = Registry::new();
    r.register(Arc::new(
        OutlookFactory::new()
            .with_endpoints(ms.clone())
            .with_retry(ms_retry),
    ));
    r.register(Arc::new(
        OutlookCalendarFactory::new()
            .with_endpoints(ms.clone())
            .with_retry(ms_retry),
    ));
    r.register(Arc::new(
        OneDriveFactory::new()
            .with_endpoints(ms)
            .with_retry(ms_retry),
    ));
    r.register(Arc::new(
        SlackFactory::new()
            .with_endpoints(saas.clone())
            .with_retry(saas_retry),
    ));
    r.register(Arc::new(
        NotionFactory::new()
            .with_endpoints(saas.clone())
            .with_retry(saas_retry),
    ));
    r.register(Arc::new(
        GitHubFactory::new()
            .with_endpoints(saas.clone())
            .with_retry(saas_retry),
    ));
    r.register(Arc::new(
        LinearFactory::new()
            .with_endpoints(saas)
            .with_retry(saas_retry),
    ));
    r
}

fn rfc3339_ago(secs: i64) -> String {
    (OffsetDateTime::now_utc() - time::Duration::seconds(secs))
        .format(&Rfc3339)
        .unwrap()
}

fn json_ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

async fn get(server: &MockServer, p: &str, body: Value) {
    Mock::given(http_method("GET"))
        .and(path(p))
        .respond_with(json_ok(body))
        .mount(server)
        .await;
}

/// Microsoft Graph: one message, one event, one file, each with a delta link back to itself.
async fn mount_microsoft(server: &MockServer) {
    let uri = server.uri();
    let inbox = "/v1.0/me/mailFolders/inbox/messages/delta";
    get(
        server,
        inbox,
        json!({
            "value": [{
                "id": "m1", "subject": "Dessert plans", "receivedDateTime": rfc3339_ago(3600),
                "lastModifiedDateTime": rfc3339_ago(3500),
                "from": {"emailAddress": {"name": "Alice", "address": "alice@example.com"}},
                "conversationId": "conv-m1", "internetMessageId": "<m1@example.com>",
                "isDraft": false, "webLink": "https://outlook.office365.com/owa/?ItemID=m1"
            }],
            "@odata.deltaLink": format!("{uri}{inbox}?$deltatoken=d1")
        }),
    )
    .await;
    Mock::given(http_method("GET"))
        .and(path("/v1.0/me/messages/m1/$value"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(
            b"From: Alice <alice@example.com>\r\nSubject: Dessert plans\r\nContent-Type: text/plain\r\n\r\nBring the marzipan to the picnic.\r\n".to_vec(),
        ))
        .mount(server)
        .await;

    let cal = "/v1.0/me/calendarView/delta";
    get(
        server,
        cal,
        json!({
            "value": [{
                "id": "e1", "changeKey": "ck-e1", "subject": "Zeppelin launch review",
                "start": {"dateTime": "2026-11-03T18:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2026-11-03T19:00:00.0000000", "timeZone": "UTC"},
                "isAllDay": false, "isCancelled": false,
                "lastModifiedDateTime": rfc3339_ago(3600),
                "webLink": "https://outlook.office365.com/owa/?itemid=e1"
            }],
            "@odata.deltaLink": format!("{uri}{cal}?$deltatoken=d1")
        }),
    )
    .await;

    let drive = "/v1.0/me/drive/root/delta";
    get(
        server,
        drive,
        json!({
            "value": [
                {"id": "ROOT", "name": "root", "root": {}, "folder": {"childCount": 1}},
                {
                    "id": "f1", "name": "notes.txt", "size": 40, "eTag": "\"{f1},1\"", "cTag": "c-f1",
                    "lastModifiedDateTime": rfc3339_ago(3600),
                    "webUrl": "https://contoso-my.sharepoint.com/f1",
                    "file": {"mimeType": "text/plain"},
                    "parentReference": {"id": "ROOT", "driveId": "d"},
                    "@microsoft.graph.downloadUrl": format!("{uri}/dl/notes.txt?tempauth=x")
                }
            ],
            "@odata.deltaLink": format!("{uri}{drive}?token=d1")
        }),
    )
    .await;
    Mock::given(http_method("GET"))
        .and(path("/dl/notes.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("Buy kumquat seedlings for the garden.\n"),
        )
        .mount(server)
        .await;
}

/// Slack, Notion, GitHub and Linear: one document each.
async fn mount_saas(server: &MockServer) {
    // Slack: one channel with one message from a few minutes ago.
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let ts = format!("{}.000100", now - 300);
    get(
        server,
        "/slack/auth.test",
        json!({"ok": true, "url": "https://acme.slack.com/", "team": "Acme", "user": "ada", "user_id": "U1"}),
    )
    .await;
    get(
        server,
        "/slack/users.info",
        json!({"ok": true, "user": {"name": "ada", "profile": {"display_name": "Ada"}}}),
    )
    .await;
    get(
        server,
        "/slack/users.conversations",
        json!({"ok": true, "channels": [{"id": "C1", "name": "general", "is_channel": true}],
               "response_metadata": {"next_cursor": ""}}),
    )
    .await;
    get(
        server,
        "/slack/conversations.history",
        json!({"ok": true, "has_more": false, "response_metadata": {"next_cursor": ""},
               "messages": [{"ts": ts, "user": "U1", "text": "the trombone section rehearses at noon"}]}),
    )
    .await;
    get(
        server,
        "/slack/conversations.replies",
        json!({"ok": true, "has_more": false, "messages": []}),
    )
    .await;

    // Notion: one page with one paragraph.
    let page_id = "33333333-3333-3333-3333-333333333333";
    let me = "11111111-1111-1111-1111-111111111111";
    get(
        server,
        "/notion/v1/users/me",
        json!({"object": "user", "id": "bot", "type": "bot",
               "bot": {"owner": {"type": "user", "user": {"id": me, "name": "Ada"}}}}),
    )
    .await;
    let edited = rfc3339_ago(3600);
    Mock::given(http_method("POST"))
        .and(path("/notion/v1/search"))
        .respond_with(json_ok(json!({
            "object": "list", "has_more": false, "next_cursor": null,
            "results": [{
                "object": "page", "id": page_id, "last_edited_time": edited, "created_time": edited,
                "created_by": {"object": "user", "id": me}, "last_edited_by": {"object": "user", "id": me},
                "archived": false, "in_trash": false, "url": format!("https://www.notion.so/{page_id}"),
                "parent": {"type": "workspace", "workspace": true},
                "properties": {"title": {"id": "title", "type": "title", "title": [{"plain_text": "Telescope notes"}]}}
            }]
        })))
        .mount(server)
        .await;
    get(
        server,
        &format!("/notion/v1/blocks/{page_id}/children"),
        json!({"object": "list", "has_more": false, "next_cursor": null, "results": [
            {"object": "block", "id": "b1", "type": "paragraph", "has_children": false,
             "paragraph": {"rich_text": [{"plain_text": "The quasar survey starts in spring."}]}}
        ]}),
    )
    .await;

    // GitHub: the viewer, then every search answers with the same issue.
    Mock::given(http_method("POST"))
        .and(path("/github/graphql"))
        .and(body_string_contains("viewer { login }"))
        .respond_with(json_ok(json!({"data": {"viewer": {"login": "ada"}}})))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(http_method("POST"))
        .and(path("/github/graphql"))
        .respond_with(json_ok(json!({"data": {
            "rateLimit": {"remaining": 4000, "resetAt": rfc3339_ago(-3600)},
            "search": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": [{
                "__typename": "Issue", "id": "I_1", "number": 1, "title": "Pelican migration tracker",
                "url": "https://github.com/octo/hello/issues/1", "state": "OPEN",
                "body": "Track the pelican migration.", "createdAt": rfc3339_ago(90_000),
                "updatedAt": rfc3339_ago(3600), "author": {"login": "ada"},
                "repository": {"nameWithOwner": "octo/hello"},
                "labels": {"nodes": []}, "assignees": {"nodes": []},
                "comments": {"totalCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
            }]}
        }})))
        .with_priority(5)
        .mount(server)
        .await;

    // Linear: the viewer, then every issues query answers with the same issue.
    Mock::given(http_method("POST"))
        .and(path("/linear/graphql"))
        .and(body_string_contains("viewer {"))
        .respond_with(json_ok(json!({"data": {
            "viewer": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"}
        }})))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(http_method("POST"))
        .and(path("/linear/graphql"))
        .respond_with(json_ok(json!({"data": {"issues": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{
                "id": "i1", "identifier": "ENG-1", "title": "Calibrate the gyroscope",
                "description": "The gyroscope drifts.", "url": "https://linear.app/acme/issue/ENG-1",
                "priorityLabel": "High", "createdAt": rfc3339_ago(90_000), "updatedAt": rfc3339_ago(3600),
                "archivedAt": null, "trashed": null,
                "state": {"name": "In Progress", "type": "started"},
                "team": {"key": "ENG", "name": "Engineering"}, "project": null,
                "assignee": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"},
                "creator": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"},
                "labels": {"nodes": []},
                "comments": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
            }]
        }}})))
        .with_priority(5)
        .mount(server)
        .await;

    // Anything else is a test bug: make it loud.
    Mock::given(path_regex(".*"))
        .respond_with(ResponseTemplate::new(418))
        .with_priority(10)
        .mount(server)
        .await;
}

struct Daemonised {
    client: Client,
    secrets: MemorySecrets,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<nugget_core::Result<()>>,
    _tmp: tempfile::TempDir,
}

async fn start(base: &str) -> Daemonised {
    let tmp = tempfile::Builder::new()
        .prefix("nugget-connectors-")
        .tempdir()
        .unwrap();
    let paths = Paths::rooted(tmp.path().join("home"));
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), NO_RUNTIME).unwrap();
    let secrets = MemorySecrets::new();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes([7; 32]));
    opts.source_secrets = Some(Arc::new(secrets.clone()));
    opts.connectors = Some(mocked_registry(base));
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();
    Daemonised {
        client,
        secrets,
        stop,
        server,
        _tmp: tmp,
    }
}

impl Daemonised {
    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(CALL, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    async fn search(&self, q: &str) -> Vec<SearchHit> {
        self.call(method::SEARCH, json!({"query": q, "limit": 10}))
            .await
    }

    async fn sources(&self) -> Vec<SourceInfo> {
        self.call(method::SOURCES_LIST, json!({})).await
    }

    /// A valid OAuth token set, as `sources.auth.begin` would have stored it.
    fn seed_oauth_token(&self, id: &SourceId) {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let t = json!({"access_token": "at", "refresh_token": "rt", "expires_at": now + 3600});
        self.secrets
            .set(&oauth_account(id.as_str()), &t.to_string())
            .unwrap();
    }
}

/// How each kind gets its credential.
enum Credential {
    /// Microsoft: an OAuth token set (from the sign-in flow) and, optionally, a client secret
    /// through `sources.secret.set` for confidential app registrations.
    OAuth { client_secret: Option<&'static str> },
    /// SaaS: a pasted token through `sources.secret.set` (`password:<id>`).
    Pasted(&'static str),
}

struct Case {
    kind: &'static str,
    settings: Value,
    credential: Credential,
    word: &'static str,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_new_kind_syncs_mocked_documents_into_search() {
    let mock = MockServer::start().await;
    mount_microsoft(&mock).await;
    mount_saas(&mock).await;
    let d = start(&mock.uri()).await;

    // The settings are what the desktop wizard sends, blank optional fields included.
    let cases = [
        Case {
            kind: "outlook",
            settings: json!({"client_id": ENTRA_APP, "tenant": "", "folders": ["inbox"]}),
            credential: Credential::OAuth {
                client_secret: Some("entra-client-secret"),
            },
            word: "marzipan",
        },
        Case {
            kind: "outlook-calendar",
            settings: json!({"client_id": ENTRA_APP, "tenant": "organizations"}),
            credential: Credential::OAuth {
                client_secret: None,
            },
            word: "zeppelin",
        },
        Case {
            kind: "onedrive",
            settings: json!({"client_id": ENTRA_APP, "include_folders": [], "exclude_folders": []}),
            credential: Credential::OAuth {
                client_secret: None,
            },
            word: "kumquat",
        },
        Case {
            kind: "slack",
            settings: json!({"token_env": "", "conversations": []}),
            credential: Credential::Pasted("xoxp-test-token"),
            word: "trombone",
        },
        Case {
            kind: "notion",
            settings: json!({"include_databases": false}),
            credential: Credential::Pasted("ntn_test_token"),
            word: "quasar",
        },
        Case {
            kind: "github",
            settings: json!({"client_id": "", "repos": [], "include_discussions": false}),
            credential: Credential::Pasted("github_pat_test"),
            word: "pelican",
        },
        Case {
            kind: "linear",
            settings: json!({"client_id": "", "include": [], "teams": []}),
            credential: Credential::Pasted("lin_api_test"),
            word: "gyroscope",
        },
    ];

    let mut added = Vec::new();
    for case in &cases {
        let info: SourceInfo = d
            .call(
                method::SOURCES_ADD,
                json!({"kind": case.kind, "name": case.kind, "settings": case.settings}),
            )
            .await;
        assert_eq!(info.kind, case.kind);
        // Blank form fields were dropped before the connector saw them.
        let stored = info.settings.as_object().unwrap();
        assert!(
            stored.values().all(|v| v != "" && v != &json!([])),
            "{}: {}",
            case.kind,
            info.settings
        );
        match &case.credential {
            Credential::OAuth { client_secret } => {
                if let Some(cs) = client_secret {
                    let () = d
                        .call(
                            method::SOURCES_SECRET_SET,
                            json!({"id": info.id, "secret": cs}),
                        )
                        .await;
                    assert_eq!(
                        d.secrets
                            .get(&format!("oauth_client_secret:{}", info.id))
                            .unwrap()
                            .as_deref(),
                        Some(*cs)
                    );
                }
                d.seed_oauth_token(&info.id);
            }
            Credential::Pasted(token) => {
                let () = d
                    .call(
                        method::SOURCES_SECRET_SET,
                        json!({"id": info.id, "secret": token}),
                    )
                    .await;
                assert_eq!(
                    d.secrets
                        .get(&format!("password:{}", info.id))
                        .unwrap()
                        .as_deref(),
                    Some(*token)
                );
            }
        }
        let () = d.call(method::SOURCES_SYNC, json!({"id": info.id})).await;
        added.push((info.id.clone(), case));
    }

    for (id, case) in &added {
        let deadline = tokio::time::Instant::now() + EVENTUALLY;
        loop {
            let hits = d.search(case.word).await;
            if hits.iter().any(|h| h.item.source == *id) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let state: Vec<_> = d
                    .sources()
                    .await
                    .into_iter()
                    .map(|s| format!("{} {:?} {:?}", s.id, s.state, s.last_error))
                    .collect();
                panic!(
                    "{}: `{}` never became searchable; sources: {state:#?}",
                    case.kind, case.word
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Every source finished its pass without needing more credentials.
    for s in d.sources().await {
        assert_ne!(s.state, SourceState::NeedsAuth, "{}", s.id);
    }
    // Pasted tokens and the client secret reached the APIs (and nowhere else).
    let requests = mock.received_requests().await.unwrap();
    let auth_of = |prefix: &str| {
        requests
            .iter()
            .filter(|r| r.url.path().starts_with(prefix))
            .filter_map(|r| r.headers.get("authorization"))
            .map(|v| v.to_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert!(auth_of("/slack/").contains(&"Bearer xoxp-test-token".to_string()));
    assert!(auth_of("/notion/").contains(&"Bearer ntn_test_token".to_string()));
    assert!(auth_of("/github/").contains(&"Bearer github_pat_test".to_string()));
    assert!(auth_of("/linear/").contains(&"lin_api_test".to_string()));
    assert!(auth_of("/v1.0/").iter().all(|a| a == "Bearer at"));
    assert!(
        requests
            .iter()
            .filter(|r| r.url.path().starts_with("/dl/"))
            .all(|r| r.headers.get("authorization").is_none()),
        "OneDrive downloads go without the bearer token"
    );
    assert!(
        !requests.iter().any(|r| r.url.path().contains("oauth2")),
        "a valid token set needs no refresh"
    );

    let _ = d.stop.send(());
    tokio::time::timeout(CALL, d.server)
        .await
        .expect("daemon stops promptly")
        .unwrap()
        .unwrap();
}
