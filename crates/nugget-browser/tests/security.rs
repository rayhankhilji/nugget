//! Red-team regressions (W8): each test is a proof of concept that failed before its fix.

mod common;

use std::time::Duration;

use common::form;
use nugget_browser::action::{BrowserAction, ValidationError};
use nugget_browser::framing::write_frame_async;
use nugget_browser::host::{spawn_extension_reader, wait_extension_closed};
use nugget_browser::install::{
    self, Browser, CHROMIUM_DEV_EXTENSION_ID, FIREFOX_EXTENSION_ID, InstallConfig, InstallError, Os,
};
use nugget_browser::snapshot::{SelectOption, Sensitive};
use serde_json::json;
use tokio::io::AsyncWriteExt;

/// BR-2: a payment `<select>` (card expiry month) is refused like any other payment field.
#[test]
fn select_in_a_payment_select_is_refused() {
    let mut s = form();
    let plan = s.elements.iter_mut().find(|e| e.name == "Plan").unwrap();
    plan.sensitive = Some(Sensitive::Payment);
    plan.value = None;
    plan.options = Some(vec![SelectOption {
        value: "07".into(),
        label: "07".into(),
        selected: false,
    }]);
    let id = plan.id.clone();
    assert!(matches!(
        BrowserAction::Select {
            element: id,
            value: "07".into()
        }
        .validate(Some(&s)),
        Err(ValidationError::SensitiveField {
            kind: Sensitive::Payment,
            ..
        })
    ));
}

/// BR-7: one malformed frame while the daemon is unreachable must not make the host exit (and
/// so drop the extension's connection); only EOF or a desynchronized stream does.
#[tokio::test]
async fn waiting_for_the_daemon_survives_a_malformed_frame() {
    let (host_side, mut ext) = tokio::io::duplex(1 << 16);
    let mut rx = spawn_extension_reader(host_side);
    ext.write_all(&3u32.to_ne_bytes()).await.unwrap();
    ext.write_all(b"{x}").await.unwrap();
    write_frame_async(
        &mut ext,
        &json!({"type": "event", "event": "user_stop", "reason": "x"}),
        1 << 16,
    )
    .await
    .unwrap();
    let early =
        tokio::time::timeout(Duration::from_millis(200), wait_extension_closed(&mut rx)).await;
    assert!(early.is_err(), "host gave up on a malformed frame");
    drop(ext);
    tokio::time::timeout(Duration::from_secs(5), wait_extension_closed(&mut rx))
        .await
        .expect("ends on EOF");
}

fn linux() -> InstallConfig {
    InstallConfig {
        os: Os::Linux,
        home: "/home/ada".into(),
        local_app_data: None,
        host_path: "/home/ada/.local/bin/nugget-browser-host".into(),
        chromium_extension_ids: vec![CHROMIUM_DEV_EXTENSION_ID.into()],
        firefox_extension_ids: vec![FIREFOX_EXTENSION_ID.into()],
    }
}

/// BR-8: `--extension-id` / `--firefox-id` values end up in `allowed_origins` /
/// `allowed_extensions`; wildcards and junk are refused instead of written.
#[test]
fn install_refuses_wildcard_and_malformed_extension_ids() {
    for bad in [
        "*",
        "",
        "chrome-extension://*/",
        "abcdefghijklmnopabcdefghijklmnoz",
    ] {
        let mut cfg = linux();
        cfg.chromium_extension_ids = vec![bad.into()];
        assert!(
            matches!(
                install::plan(&[Browser::Chrome], &cfg),
                Err(InstallError::BadExtensionId(_))
            ),
            "{bad:?} accepted"
        );
    }
    for bad in ["*", "*@nugget.dev", "a@b@c", "x/y@z", ""] {
        let mut cfg = linux();
        cfg.firefox_extension_ids = vec![bad.into()];
        assert!(
            matches!(
                install::plan(&[Browser::Firefox], &cfg),
                Err(InstallError::BadExtensionId(_))
            ),
            "{bad:?} accepted"
        );
    }
    let mut cfg = linux();
    cfg.chromium_extension_ids = vec![];
    assert!(install::plan(&[Browser::Edge], &cfg).is_err());
    // The defaults, a GUID-style Firefox id and only-Firefox installs with no Chromium ids pass.
    assert!(install::plan(&Browser::ALL, &linux()).is_ok());
    let mut cfg = linux();
    cfg.chromium_extension_ids = vec![];
    cfg.firefox_extension_ids = vec!["{12345678-1234-1234-1234-123456789abc}".into()];
    assert!(install::plan(&[Browser::Firefox], &cfg).is_ok());
}
