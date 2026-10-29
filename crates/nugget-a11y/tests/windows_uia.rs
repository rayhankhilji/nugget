//! Real UI Automation integration tests on Windows against the WinForms fixture app
//! (`tests/fixtures/native/windows/`, built with `build.ps1`; its path in
//! `NUGGET_A11Y_FIXTURE`).
//!
//! Ignored by default (it needs an interactive desktop session and the fixture); the real-os.yml
//! `a11y` job runs it with `--ignored`.

#![cfg(windows)]

mod native;

use nugget_a11y::Permission;
use nugget_a11y::clipboard::ClipboardSnapshot;

/// `CF_UNICODETEXT` data: UTF-16LE with the terminating NUL.
fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[test]
#[ignore = "needs an interactive Windows desktop and the WinForms fixture (NUGGET_A11Y_FIXTURE); run by real-os.yml"]
fn uia_native_fixture() {
    assert_eq!(nugget_a11y::system().permission(), Permission::NotRequired);
    native::run(native::Os {
        platform: nugget_a11y::UiaPlatform::new,
        seed_clipboard: ClipboardSnapshot {
            items: vec![vec![
                // CF_UNICODETEXT
                native::text_format("#13", utf16z("previous clipboard ✓")),
                native::text_format("Nugget Test Format", vec![0, 1, 2, 3, 255]),
            ]],
        },
        end_of_line: "end",
    });
}
