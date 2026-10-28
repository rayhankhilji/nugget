use std::io::Cursor;

use nugget_browser::action::BrowserAction;
use nugget_browser::framing::{
    FrameError, MAX_FROM_EXTENSION, MAX_TO_EXTENSION, encode, read_frame, read_frame_async,
    write_frame, write_frame_async,
};
use nugget_browser::protocol::{
    BrowserEvent, ErrorCode, ExtensionMessage, HostMessage, HostRequest, SnapshotOptions,
};
use serde_json::{Value, json};

#[test]
fn round_trip_sync() {
    let msg = HostMessage {
        id: 7,
        request: HostRequest::Act {
            tab_id: None,
            snapshot_id: "ab.1".into(),
            action: BrowserAction::Type {
                element: "n3".into(),
                text: "naïve — 日本語 🍣".into(),
                clear: Some(false),
            },
        },
    };
    let mut buf = Vec::new();
    write_frame(&mut buf, &msg, MAX_TO_EXTENSION).unwrap();
    write_frame(
        &mut buf,
        &HostMessage {
            id: 8,
            request: HostRequest::Halt,
        },
        MAX_TO_EXTENSION,
    )
    .unwrap();
    let len = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
    let first: Value = serde_json::from_slice(&buf[4..4 + len]).unwrap();
    assert_eq!(
        first,
        json!({"id": 7, "type": "act", "snapshot_id": "ab.1",
               "action": {"kind": "type", "element": "n3", "text": "naïve — 日本語 🍣", "clear": false}})
    );
    let mut r = Cursor::new(buf);
    let a: HostMessage = read_frame(&mut r, MAX_TO_EXTENSION).unwrap();
    let b: HostMessage = read_frame(&mut r, MAX_TO_EXTENSION).unwrap();
    assert_eq!(a, msg);
    assert_eq!(b.request, HostRequest::Halt);
    assert!(matches!(
        read_frame::<_, Value>(&mut r, MAX_TO_EXTENSION),
        Err(FrameError::Closed)
    ));
}

#[tokio::test]
async fn round_trip_async() {
    let (mut a, mut b) = tokio::io::duplex(1 << 16);
    let msg = HostMessage {
        id: 1,
        request: HostRequest::Snapshot {
            tab_id: Some(4),
            options: Some(SnapshotOptions {
                include_text: Some(false),
                include_hidden: None,
            }),
        },
    };
    write_frame_async(&mut a, &msg, MAX_TO_EXTENSION)
        .await
        .unwrap();
    let got: HostMessage = read_frame_async(&mut b, MAX_TO_EXTENSION).await.unwrap();
    assert_eq!(got, msg);
}

#[test]
fn oversized_messages_are_rejected_both_ways() {
    let big = "x".repeat(MAX_TO_EXTENSION);
    assert!(matches!(
        encode(&big, MAX_TO_EXTENSION),
        Err(FrameError::TooLarge { .. })
    ));

    // A header announcing more than the cap is refused before allocating or reading the body.
    let mut header = ((MAX_FROM_EXTENSION + 1) as u32).to_ne_bytes().to_vec();
    header.extend_from_slice(b"{}");
    let err = read_frame::<_, Value>(&mut Cursor::new(header), MAX_FROM_EXTENSION).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { len, .. } if len == MAX_FROM_EXTENSION + 1));
}

#[test]
fn truncated_and_malformed_frames() {
    let mut buf = encode(&json!({"a": 1}), 100).unwrap();
    buf.pop();
    assert!(matches!(
        read_frame::<_, Value>(&mut Cursor::new(buf), 100),
        Err(FrameError::Truncated)
    ));
    let mut bad = 3u32.to_ne_bytes().to_vec();
    bad.extend_from_slice(b"{x}");
    assert!(matches!(
        read_frame::<_, Value>(&mut Cursor::new(bad), 100),
        Err(FrameError::Json(_))
    ));
}

#[test]
fn extension_messages_parse() {
    let reply: ExtensionMessage =
        serde_json::from_value(json!({"type": "reply", "id": 3, "result": {"ok": 1}})).unwrap();
    assert_eq!(
        reply,
        ExtensionMessage::Reply {
            id: 3,
            result: Some(json!({"ok": 1})),
            error: None
        }
    );
    let err: ExtensionMessage = serde_json::from_value(
        json!({"type": "reply", "id": 4, "error": {"code": "stale_snapshot", "message": "m"}}),
    )
    .unwrap();
    assert_eq!(
        err,
        ExtensionMessage::error_reply(4, ErrorCode::StaleSnapshot, "m")
    );
    let future: ExtensionMessage = serde_json::from_value(
        json!({"type": "reply", "id": 5, "error": {"code": "brand_new", "message": "m"}}),
    )
    .unwrap();
    assert!(
        matches!(future, ExtensionMessage::Reply { error: Some(e), .. } if e.code == ErrorCode::Unknown)
    );
    let ev: ExtensionMessage =
        serde_json::from_value(json!({"type": "event", "event": "user_stop", "reason": "overlay"}))
            .unwrap();
    assert_eq!(
        ev,
        ExtensionMessage::Event(BrowserEvent::UserStop {
            reason: "overlay".into()
        })
    );
    assert_eq!(
        serde_json::to_value(&ev).unwrap(),
        json!({"type": "event", "event": "user_stop", "reason": "overlay"})
    );
}
