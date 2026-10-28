//! Daemon hub <-> host relay <-> fake extension, in process and over the real IPC socket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nugget_browser::bridge::{
    AttachInfo, BridgeError, BridgeHub, BrowserBridge, HubHandler, KillSwitch,
};
use nugget_browser::framing::{
    MAX_FROM_EXTENSION, MAX_TO_EXTENSION, read_frame_async, write_frame_async,
};
use nugget_browser::host::{
    DaemonLink, HubLink, IpcDaemonLink, RelayEnd, run_relay, spawn_extension_reader,
};
use nugget_browser::protocol::{
    BrowserEvent, ErrorCode, ExtensionMessage, HostMessage, HostRequest,
};
use serde_json::json;
use tokio::io::{AsyncWriteExt, DuplexStream};

#[derive(Default)]
struct RecordingKill(Mutex<Vec<String>>);

impl KillSwitch for RecordingKill {
    fn engage(&self, reason: &str) {
        self.0.lock().unwrap().push(reason.to_owned());
    }
}

/// A fake extension: answers `hello` and echoes the request type; sends a user_stop event when
/// asked to `halt`.
fn spawn_fake_extension(mut io: DuplexStream) {
    tokio::spawn(async move {
        loop {
            let Ok(msg) = read_frame_async::<_, HostMessage>(&mut io, MAX_TO_EXTENSION).await
            else {
                return;
            };
            let reply = match &msg.request {
                HostRequest::Hello { .. } => json!({"type": "reply", "id": msg.id,
                    "result": {"protocol": 1, "extension_version": "0.0.1", "browser": "fake"}}),
                HostRequest::Snapshot { .. } => json!({"type": "reply", "id": msg.id,
                    "error": {"code": "permission_required", "message": "grant it"}}),
                HostRequest::Halt => {
                    let ev = json!({"type": "event", "event": "user_stop", "reason": "overlay"});
                    write_frame_async(&mut io, &ev, MAX_FROM_EXTENSION)
                        .await
                        .unwrap();
                    json!({"type": "reply", "id": msg.id, "result": {}})
                }
                _ => {
                    json!({"type": "reply", "id": msg.id, "result": {"echo": serde_json::to_value(&msg.request).unwrap()["type"]}})
                }
            };
            if write_frame_async(&mut io, &reply, MAX_FROM_EXTENSION)
                .await
                .is_err()
            {
                return;
            }
        }
    });
}

async fn exercise(hub: &BridgeHub, kill: &RecordingKill) {
    let hello = hub
        .request(HostRequest::Hello { protocol: 1 })
        .await
        .unwrap();
    assert_eq!(hello["browser"], "fake");
    let err = hub
        .request(HostRequest::Snapshot {
            tab_id: None,
            options: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        err,
        BridgeError::Extension {
            code: ErrorCode::PermissionRequired,
            message: "grant it".into()
        }
    );

    // Concurrent requests are correlated by id.
    let (a, b) = tokio::join!(
        hub.request(HostRequest::Overlay {
            tab_id: None,
            active: true,
            label: None
        }),
        hub.request(HostRequest::Hello { protocol: 1 }),
    );
    assert_eq!(a.unwrap()["echo"], "overlay");
    assert_eq!(b.unwrap()["protocol"], 1);

    // A user Stop event reaches the kill switch and event subscribers.
    let mut events = hub.subscribe();
    hub.request(HostRequest::Halt).await.unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ev,
        BrowserEvent::UserStop {
            reason: "overlay".into()
        }
    );
    assert_eq!(kill.0.lock().unwrap().as_slice(), ["overlay"]);
}

#[tokio::test]
async fn in_process_relay() {
    let kill = Arc::new(RecordingKill::default());
    let hub = Arc::new(BridgeHub::new(Some(kill.clone())).with_timeout(Duration::from_secs(5)));
    assert_eq!(
        hub.request(HostRequest::Halt).await.unwrap_err(),
        BridgeError::NotConnected
    );

    let (host_side, ext_side) = tokio::io::duplex(1 << 20);
    spawn_fake_extension(ext_side);
    let (read, mut write) = tokio::io::split(host_side);
    let link = HubLink::attach(
        hub.clone(),
        AttachInfo {
            origin: "chrome-extension://x/".into(),
            host_version: "t".into(),
        },
    );
    assert_eq!(hub.attached_info().unwrap().origin, "chrome-extension://x/");
    let mut from_ext = spawn_extension_reader(read);
    let relay = tokio::spawn(async move {
        let end = run_relay(&mut from_ext, &mut write, &link).await;
        drop(link);
        end
    });
    exercise(&hub, &kill).await;

    // A request too large for Chrome's 1 MiB limit is answered with an error, not sent.
    let big = HostRequest::Overlay {
        tab_id: None,
        active: true,
        label: Some("x".repeat(MAX_TO_EXTENSION)),
    };
    assert!(matches!(
        hub.request(big).await,
        Err(BridgeError::Extension {
            code: ErrorCode::TooLarge,
            ..
        })
    ));
    relay.abort();
}

#[tokio::test]
async fn relay_ends_on_oversized_extension_frame() {
    let hub = Arc::new(BridgeHub::new(None));
    let (host_side, mut ext_side) = tokio::io::duplex(1 << 16);
    let (read, mut write) = tokio::io::split(host_side);
    let link = HubLink::attach(hub.clone(), AttachInfo::default());
    let mut from_ext = spawn_extension_reader(read);
    ext_side
        .write_all(&((MAX_FROM_EXTENSION + 1) as u32).to_ne_bytes())
        .await
        .unwrap();
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        run_relay(&mut from_ext, &mut write, &link),
    )
    .await
    .unwrap();
    assert_eq!(end, RelayEnd::ExtensionClosed);
}

#[tokio::test]
async fn relay_skips_malformed_json_and_ends_on_eof() {
    struct Sink(Mutex<Vec<ExtensionMessage>>);
    #[async_trait::async_trait]
    impl DaemonLink for Sink {
        async fn next_request(&self) -> Option<HostMessage> {
            std::future::pending().await
        }
        async fn deliver(
            &self,
            msg: ExtensionMessage,
        ) -> Result<(), nugget_browser::host::LinkError> {
            self.0.lock().unwrap().push(msg);
            Ok(())
        }
    }
    let (host_side, mut ext_side) = tokio::io::duplex(1 << 16);
    let (read, mut write) = tokio::io::split(host_side);
    let mut from_ext = spawn_extension_reader(read);
    ext_side.write_all(&3u32.to_ne_bytes()).await.unwrap();
    ext_side.write_all(b"{x}").await.unwrap();
    write_frame_async(&mut ext_side, &json!({"type": "event", "event": "permission_changed", "origins": ["https://a.example/*"], "granted": true}), 1 << 16).await.unwrap();
    drop(ext_side);
    let sink = Sink(Mutex::new(Vec::new()));
    assert_eq!(
        run_relay(&mut from_ext, &mut write, &sink).await,
        RelayEnd::ExtensionClosed
    );
    let got = sink.0.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert!(matches!(
        &got[0],
        ExtensionMessage::Event(BrowserEvent::PermissionChanged { granted: true, .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn over_the_daemon_ipc_socket() {
    use nugget_core::paths::Paths;
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(dir.path().to_path_buf());
    let kill = Arc::new(RecordingKill::default());
    let hub = Arc::new(BridgeHub::new(Some(kill.clone())).with_timeout(Duration::from_secs(5)));
    let listener = nugget_ipc::bind(&paths).await.unwrap();
    tokio::spawn(nugget_ipc::serve(
        listener,
        Arc::new(HubHandler::new(hub.clone())),
    ));

    let (host_side, ext_side) = tokio::io::duplex(1 << 20);
    spawn_fake_extension(ext_side);
    let (read, mut write) = tokio::io::split(host_side);
    let link = IpcDaemonLink::connect(
        &paths,
        AttachInfo {
            origin: "o".into(),
            host_version: "t".into(),
        },
    )
    .await
    .unwrap();
    let mut from_ext = spawn_extension_reader(read);
    tokio::spawn(async move { run_relay(&mut from_ext, &mut write, &link).await });

    // Wait for the attach call to land.
    for _ in 0..100 {
        if hub.is_connected() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(hub.is_connected());
    exercise(&hub, &kill).await;
}
