//! `egress.record`: the desktop app's own requests (update checks) land in the daemon's ledger,
//! and nothing but the client features is accepted.

use std::time::Duration;

use nugget_core::egress::EgressEvent;
use nugget_core::ipc::method;
use nugget_core::paths::Paths;
use nugget_ipc::{Client, IpcError, code};
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(30);

async fn call(client: &Client, m: &str, params: Value) -> Result<Value, IpcError> {
    tokio::time::timeout(WAIT, client.call(m, &params))
        .await
        .unwrap_or_else(|_| panic!("{m} timed out"))
}

fn invalid(r: Result<Value, IpcError>) -> bool {
    matches!(r, Err(IpcError::Rpc(e)) if e.code == code::INVALID_PARAMS)
}

#[tokio::test]
async fn client_requests_are_recorded_and_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), "[runtime]\nenabled = false\n").unwrap();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes([3; 32]));
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();

    // A feed check now, and a download queued while the daemon was away (its own time kept).
    let r = call(
        &client,
        method::EGRESS_RECORD,
        json!({"host": "github.com", "feature": "update_check", "bytes_received": 2048}),
    )
    .await
    .unwrap();
    assert_eq!(r, Value::Null);
    call(
        &client,
        method::EGRESS_RECORD,
        json!({
            "at": "2026-01-02T03:04:05Z",
            "host": "objects.example.com",
            "feature": "update_download",
            "bytes_sent": 0,
            "bytes_received": 9_000_000,
        }),
    )
    .await
    .unwrap();

    // Refused: a daemon feature, a URL instead of a host, missing fields.
    assert!(invalid(
        call(
            &client,
            method::EGRESS_RECORD,
            json!({"host": "api.example.com", "feature": "chat"}),
        )
        .await
    ));
    assert!(invalid(
        call(
            &client,
            method::EGRESS_RECORD,
            json!({"host": "https://github.com/x", "feature": "update_check"}),
        )
        .await
    ));
    assert!(invalid(
        call(
            &client,
            method::EGRESS_RECORD,
            json!({"feature": "update_check"})
        )
        .await
    ));

    let events: Vec<EgressEvent> = serde_json::from_value(
        call(&client, method::EGRESS_RECENT, json!({"limit": 50}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    let check = events.iter().find(|e| e.feature == "update_check").unwrap();
    assert_eq!(
        (check.host.as_str(), check.bytes_received),
        ("github.com", 2048)
    );
    assert!(check.items.is_empty());
    let download = events
        .iter()
        .find(|e| e.feature == "update_download")
        .unwrap();
    assert_eq!(download.host, "objects.example.com");
    assert_eq!(download.bytes_received, 9_000_000);
    assert_eq!(download.at.unix_timestamp(), 1_767_323_045);

    let _ = tx.send(());
    tokio::time::timeout(WAIT, server)
        .await
        .expect("daemon stops")
        .unwrap()
        .unwrap();
}
