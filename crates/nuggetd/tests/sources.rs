//! Sources end to end: a files source indexed, searched and removed, duplicate folders, and
//! source secrets kept out of every response, file and log.

mod common;

use common::*;
use nugget_connector::SecretStore;
use nugget_core::ipc::{
    ContextResponse, IngestProgress, IngestStatus, SourceInfo, SourceState, method, notification,
};
use nugget_ipc::{Client, Notification, code};
use serde_json::{Value, json};
use std::sync::Mutex;

fn source_changes(notes: &Mutex<Vec<Notification>>) -> Vec<Value> {
    notes
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.method == notification::SOURCES_CHANGED)
        .map(|n| n.params.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_source_is_searchable_fresh_and_removable() {
    let mut h = Harness::start(NO_RUNTIME).await;
    // A second connection that never calls anything still gets the broadcasts.
    let watcher = Client::connect(&h.paths).await.unwrap();
    let seen = collect(&watcher);
    let caller_seen = collect(&h.client);

    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let info: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            // `include: []` is what the desktop sends for "default types".
            json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs], "include": [], "exclude": []}}),
        )
        .await;
    assert_eq!(info.id.as_str(), "files-1");
    assert_eq!(info.settings, json!({"roots": [docs], "exclude": []}));

    // Create → searchable, with a file URL; freshness is reported.
    let file = docs.join("alpha.md");
    std::fs::write(&file, "# Alpha\n\nThe zebra ate a quartzite pebble.\n").unwrap();
    eventually("new file searchable", || async {
        !h.search("quartzite").await.is_empty()
    })
    .await;
    let hits = h.search("quartzite").await;
    let url = hits[0].url.as_deref().expect("file url");
    assert!(
        url.starts_with("file://") && url.ends_with("alpha.md"),
        "{url}"
    );
    eventually("freshness reported", || async {
        let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
        st.freshness_p50_ms.is_some() && st.freshness_p95_ms.is_some() && st.queue_depth == 0
    })
    .await;

    // Edit → the new text replaces the old.
    std::fs::write(&file, "# Alpha\n\nNow it is about marmalade.\n").unwrap();
    eventually("edit reflected", || async {
        !h.search("marmalade").await.is_empty() && h.search("quartzite").await.is_empty()
    })
    .await;

    // retrieve.context packs it with a citation.
    let ctx: ContextResponse = h
        .call(
            method::RETRIEVE_CONTEXT,
            json!({"query": "marmalade", "budget_tokens": 300}),
        )
        .await;
    assert_eq!(ctx.citations.len(), 1, "{ctx:?}");
    assert!(ctx.text.contains("[1]") && ctx.text.contains("marmalade"));

    // Delete → gone.
    std::fs::remove_file(&file).unwrap();
    eventually("deleted file leaves the index", || async {
        h.search("marmalade").await.is_empty()
    })
    .await;

    // Remove with purge clears everything the source indexed.
    std::fs::write(docs.join("beta.txt"), "kumquat orchard notes").unwrap();
    eventually("second file searchable", || async {
        !h.search("kumquat").await.is_empty()
    })
    .await;
    let _: Value = h
        .call(
            method::SOURCES_REMOVE,
            json!({"id": "files-1", "purge": true}),
        )
        .await;
    assert!(h.search("kumquat").await.is_empty());
    let list: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
    assert!(list.is_empty());
    let (c, _) = h
        .call_err(method::SOURCES_SYNC, json!({"id": "files-1"}))
        .await;
    assert_eq!(c, code::NOT_FOUND);

    // Both connections got the broadcasts (each once): SourceInfo payloads, progress, removal.
    eventually("removal broadcast", || async {
        source_changes(&seen).contains(&json!({"id": "files-1", "removed": true}))
    })
    .await;
    for notes in [&seen, &caller_seen] {
        let changes = source_changes(notes);
        assert!(
            changes
                .iter()
                .any(|v| serde_json::from_value::<SourceInfo>(v.clone()).is_ok()),
            "{changes:?}"
        );
        let progress: Vec<IngestProgress> = notes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::INGEST_PROGRESS)
            .map(|n| n.params_as().unwrap())
            .collect();
        assert!(!progress.is_empty() && progress.iter().all(|p| p.source.as_str() == "files-1"));
        let removed = changes.iter().filter(|v| v.get("removed").is_some());
        assert_eq!(removed.count(), 1);
    }

    let actions: Vec<String> = h.audit().into_iter().map(|a| a.0).collect();
    assert!(
        actions.iter().any(|a| a == "source.add") && actions.iter().any(|a| a == "source.remove")
    );
    h.stop().await;
}

/// GG6-13 / GG6-2: the same folder twice is refused with the existing source's name, and an
/// empty folder settles with a finished scan report and `nothing_indexable` health.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_folders_are_refused_and_empty_folders_settle() {
    let h = Harness::start(NO_RUNTIME).await;
    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(docs.join("inner")).unwrap();
    let info: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs]}}),
        )
        .await;
    for (roots, want) in [
        (json!([docs]), "This folder is already added as \"Docs\"."),
        (
            json!([docs.join("inner")]),
            "This folder is inside \"Docs\", which is already added.",
        ),
        (json!([h.tmp.path()]), "This folder contains \"Docs\""),
    ] {
        let (c, msg) = h
            .call_err(
                method::SOURCES_ADD,
                json!({"kind": "files", "name": "Again", "settings": {"roots": roots}}),
            )
            .await;
        assert_eq!(c, code::INVALID_PARAMS, "{msg}");
        assert!(msg.starts_with(want), "{msg}");
    }
    let all: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
    assert_eq!(all.len(), 1);

    eventually("empty folder settles", || async {
        let all: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
        let s = all.iter().find(|s| s.id == info.id).unwrap();
        s.scan.as_ref().is_some_and(|r| r.finished && r.found == 0)
            && s.health
                .as_ref()
                .is_some_and(|h| h.code == "nothing_indexable")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secrets_are_stored_never_echoed_and_cleaned_up() {
    const PASSWORD: &str = "hunter2-very-secret";
    const CLIENT_SECRET: &str = "GOCSPX-client-secret-value";
    let mut h = Harness::start(NO_RUNTIME).await;
    let seen = collect(&h.client);

    // IMAP without a password parks in needs_auth (no network is touched).
    let closed_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let imap: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "imap", "name": "Mail", "settings": {
                "host": "127.0.0.1", "port": closed_port, "tls": "implicit",
                "username": "me@example.com", "folders": ["INBOX"]}}),
        )
        .await;
    eventually("imap needs auth", || async {
        let list: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
        list.iter()
            .any(|s| s.id == imap.id && s.state == SourceState::NeedsAuth)
    })
    .await;

    // Validation of secret.set.
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": imap.id, "secret": ""}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS);
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": "nope-1", "secret": "x"}),
        )
        .await;
    assert_eq!(c, code::NOT_FOUND);

    let r: Value = h
        .call(
            method::SOURCES_SECRET_SET,
            json!({"id": imap.id, "secret": PASSWORD}),
        )
        .await;
    assert_eq!(r, Value::Null);
    assert_eq!(
        h.secrets.get("password:imap-1").unwrap().as_deref(),
        Some(PASSWORD)
    );

    // OAuth kinds: the client secret goes to its own account, never into settings.
    let gmail: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "gmail", "name": "Gmail", "settings": {"client_id": "123-abc.apps.googleusercontent.com"}}),
        )
        .await;
    let (c, msg) = h
        .call_err(
            method::SOURCES_ADD,
            json!({"kind": "gcal", "name": "Cal", "settings": {"client_id": "x", "client_secret": CLIENT_SECRET}}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS, "{msg}");
    assert!(!msg.contains(CLIENT_SECRET));
    let _: Value = h
        .call(
            method::SOURCES_SECRET_SET,
            json!({"id": gmail.id, "secret": CLIENT_SECRET}),
        )
        .await;
    assert_eq!(
        h.secrets
            .get("oauth_client_secret:gmail-1")
            .unwrap()
            .as_deref(),
        Some(CLIENT_SECRET)
    );
    // Auth begins with a URL for the client to open (the daemon opens nothing).
    let auth: nugget_core::ipc::AuthBegin = h
        .call(method::SOURCES_AUTH_BEGIN, json!({"id": gmail.id}))
        .await;
    assert!(auth.url.starts_with("https://"), "{}", auth.url);
    assert!(auth.url.contains("123-abc.apps.googleusercontent.com"));
    assert!(!auth.url.contains(CLIENT_SECRET));

    // Files take no secret.
    let docs = h.tmp.path().join("plain");
    std::fs::create_dir_all(&docs).unwrap();
    let files: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "F", "settings": {"roots": [docs]}}),
        )
        .await;
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": files.id, "secret": "x"}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS);

    // No secret anywhere it could leak: responses, notifications, sources.toml, audit.
    let list: Value = h.call(method::SOURCES_LIST, json!({})).await;
    let status: Value = h.call(method::INGEST_STATUS, json!({})).await;
    let toml = std::fs::read_to_string(h.paths.config.join(nugget_ingest::SOURCES_FILE)).unwrap();
    let notes = serde_json::to_string(&*seen.lock().unwrap()).unwrap();
    let audit = serde_json::to_string(&h.audit()).unwrap();
    for (what, text) in [
        ("sources.list", list.to_string()),
        ("ingest.status", status.to_string()),
        ("sources.toml", toml.clone()),
        ("notifications", notes),
        ("audit", audit.clone()),
    ] {
        assert!(!text.contains(PASSWORD), "{what} leaks the password");
        assert!(
            !text.contains(CLIENT_SECRET),
            "{what} leaks the client secret"
        );
    }
    assert!(
        toml.contains("oauth_client_secret:gmail-1"),
        "account name recorded"
    );
    assert!(audit.contains("\"account\":\"password:imap-1\""), "{audit}");
    assert!(audit.contains("source.auth.begin"), "{audit}");

    // Removing the sources deletes their keychain entries.
    for id in [&imap.id, &gmail.id] {
        let _: Value = h
            .call(method::SOURCES_REMOVE, json!({"id": id, "purge": true}))
            .await;
    }
    let left: Vec<String> = h.secrets.accounts();
    assert!(left.is_empty(), "{left:?}");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn builtin_clients_are_listed_and_disconnect_forgets_tokens() {
    let h = Harness::start(NO_RUNTIME).await;
    let builtin: Value = h.call(method::SOURCES_OAUTH_BUILTIN, json!({})).await;
    assert_eq!(
        builtin,
        json!({"providers": nugget_oauth::bundled::available()})
    );

    let gmail: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "gmail", "name": "Gmail", "settings": {"client_id": "123-abc.apps.googleusercontent.com"}}),
        )
        .await;
    // An expired token without a refresh token: nothing to revoke, so no request leaves.
    h.secrets
        .set("oauth:gmail-1", r#"{"access_token":"at","expires_at":1}"#)
        .unwrap();
    h.secrets.set("oauth_client_secret:gmail-1", "cs").unwrap();
    let r: Value = h
        .call(method::SOURCES_AUTH_REVOKE, json!({"id": gmail.id}))
        .await;
    assert_eq!(r, Value::Null);
    assert_eq!(h.secrets.get("oauth:gmail-1").unwrap(), None);
    // The user's own client stays configured for the next Connect.
    assert!(
        h.secrets
            .get("oauth_client_secret:gmail-1")
            .unwrap()
            .is_some()
    );
    let audit = serde_json::to_string(&h.audit()).unwrap();
    assert!(audit.contains("source.auth.revoke"));
    let (c, _) = h
        .call_err(method::SOURCES_AUTH_REVOKE, json!({"id": "nosuch-1"}))
        .await;
    assert_ne!(c, 0);
}
