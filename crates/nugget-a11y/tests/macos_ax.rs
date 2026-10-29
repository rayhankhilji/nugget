//! Real AX integration tests on macOS against the AppKit fixture app
//! (`tests/fixtures/native/macos/`, built with `build.sh`; its path in `NUGGET_A11Y_FIXTURE`).
//!
//! Ignored by default (it needs a desktop session, the fixture and the Accessibility
//! permission); the real-os.yml `a11y` job runs it with `--ignored`. Every AX call needs this
//! process (or the process responsible for it) to be trusted for Accessibility in TCC. When it
//! is not, the test fails, unless `NUGGET_A11Y_ALLOW_UNTRUSTED=1`, in which case it reports a
//! loud skip naming exactly what is missing (and a workflow warning) instead of passing quietly.

#![cfg(target_os = "macos")]

mod native;

use nugget_a11y::Permission;
use nugget_a11y::clipboard::ClipboardSnapshot;

/// `CGPreflightPostEventAccess`: whether this process may post synthetic events (the paste
/// keystroke and key input), which TCC grants separately from AX (`kTCCServicePostEvent`).
fn may_post_events() -> bool {
    #[allow(unsafe_code)]
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightPostEventAccess() -> bool;
    }
    // SAFETY: no arguments, no preconditions; a documented CoreGraphics query.
    #[allow(unsafe_code)]
    unsafe {
        CGPreflightPostEventAccess()
    }
}

/// The chain of parent processes, for the TCC "responsible process" diagnosis.
fn ancestry() -> String {
    let mut out = Vec::new();
    let mut pid = std::process::id().to_string();
    for _ in 0..12 {
        let o = std::process::Command::new("ps")
            .args(["-o", "ppid=,comm=", "-p", &pid])
            .output();
        let Ok(o) = o else { break };
        let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let Some((ppid, comm)) = line.split_once(' ') else {
            break;
        };
        out.push(format!("{pid} {}", comm.trim()));
        if ppid.trim() == "0" || ppid.trim() == "1" {
            out.push(format!("{} launchd", ppid.trim()));
            break;
        }
        pid = ppid.trim().to_string();
    }
    out.join(" <- ")
}

#[test]
#[ignore = "needs a macOS desktop session, the AppKit fixture (NUGGET_A11Y_FIXTURE) and Accessibility trust; run by real-os.yml"]
fn ax_native_fixture() {
    let perm = nugget_a11y::system().permission();
    let exe = std::env::current_exe().unwrap();
    eprintln!(
        "AXIsProcessTrusted: {perm:?}; test binary {}",
        exe.display()
    );
    eprintln!("process ancestry: {}", ancestry());
    eprintln!("CGPreflightPostEventAccess: {}", may_post_events());
    if perm != Permission::Granted {
        let why = format!(
            "AXIsProcessTrusted() is false for {} (process chain: {}): TCC grants \
             kTCCServiceAccessibility neither to this binary nor to its responsible process, so \
             every AX call fails with kAXErrorAPIDisabled",
            exe.display(),
            ancestry()
        );
        if std::env::var("NUGGET_A11Y_ALLOW_UNTRUSTED").is_ok_and(|v| v == "1") {
            println!("::warning title=nugget-a11y AX test skipped::{why}");
            eprintln!("SKIPPED (not passed): {why}");
            return;
        }
        panic!("{why}");
    }

    native::run(native::Os {
        platform: nugget_a11y::AxPlatform::new,
        seed_clipboard: ClipboardSnapshot {
            items: vec![vec![
                native::text_format(
                    "public.utf8-plain-text",
                    "previous clipboard ✓".as_bytes().to_vec(),
                ),
                native::text_format("dev.nugget.test", vec![0, 1, 2, 3, 255]),
            ]],
        },
        end_of_line: "cmd+right",
    });
}
