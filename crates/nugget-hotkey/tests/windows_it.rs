//! Windows integration tests on a real desktop: the Double Tap listeners fed with `SendInput`,
//! and the trigger-to-insert path into Notepad, read back through UI Automation.
//!
//! Ignored by default (they press real keys and open windows). Run them on an interactive
//! desktop, one at a time, as `.github/workflows/real-os.yml` does:
//!
//! ```sh
//! cargo test -p nugget-hotkey --test windows_it -- --ignored --test-threads=1 --nocapture
//! ```

#![cfg(target_os = "windows")]
#![allow(unsafe_code)]

mod support;

use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nugget_hotkey::{KillSwitch, Modifier, Trigger, start, start_kill_switch};
use support::{Mode, Pipeline};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
};

// Virtual keys and their set-1 scan codes.
const RCONTROL: (u16, u16, bool) = (0xA3, 0x1D, true);
const LCONTROL: (u16, u16, bool) = (0xA2, 0x1D, false);
const LMENU: (u16, u16, bool) = (0xA4, 0x38, false);
const SPACE: (u16, u16, bool) = (0x20, 0x39, false);
const ESCAPE: (u16, u16, bool) = (0x1B, 0x01, false);
const KEY_F13: (u16, u16, bool) = (0x7C, 0x64, false);

fn send(key: (u16, u16, bool), down: bool) {
    let (vk, scan, extended) = key;
    let mut flags = 0;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one valid keyboard INPUT, with its size.
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(
        sent,
        1,
        "SendInput was blocked ({}): no interactive desktop, or a higher-integrity window has \
         the foreground",
        std::io::Error::last_os_error()
    );
    sleep(Duration::from_millis(25));
}

fn tap(key: (u16, u16, bool)) {
    send(key, true);
    send(key, false);
    sleep(Duration::from_millis(40));
}

fn chord(keys: &[(u16, u16, bool)]) {
    for &k in keys {
        send(k, true);
    }
    for &k in keys.iter().rev() {
        send(k, false);
    }
    sleep(Duration::from_millis(40));
}

fn channel_callback() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<Instant>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    (
        Box::new(move || {
            let _ = tx.lock().unwrap().send(Instant::now());
        }),
        rx,
    )
}

/// How many fires arrive until 300 ms pass without one.
fn fired(rx: &mpsc::Receiver<Instant>) -> usize {
    let mut n = 0;
    while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
        n += 1;
    }
    n
}

fn right_ctrl_double_tap() -> Trigger {
    Trigger::DoubleTap {
        modifier: Modifier::RightControl,
        window_ms: 350,
    }
}

#[test]
#[ignore = "presses real keys on an interactive desktop; run with --ignored (real-os.yml)"]
fn windows_hotkeys() {
    assert_eq!(nugget_hotkey::session(), nugget_hotkey::Session::Windows);
    for k in [RCONTROL, LCONTROL, LMENU] {
        send(k, false);
    }

    // The recommended trigger: a right-Ctrl double tap, through the WH_KEYBOARD_LL hook.
    assert_eq!(
        nugget_hotkey::recommended_trigger(),
        right_ctrl_double_tap()
    );
    let (cb, rx) = channel_callback();
    let listener = start(right_ctrl_double_tap(), cb).expect("start the double-tap listener");
    sleep(Duration::from_millis(200));

    send(RCONTROL, true);
    send(RCONTROL, false);
    send(RCONTROL, true);
    // Measured from just before the second release is injected (`send` then waits 25 ms).
    let released = Instant::now();
    send(RCONTROL, false);
    let at = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the right-Ctrl double tap did not fire within 2 s");
    println!(
        "DOUBLETAP-HOTKEY os=windows trigger=double_tap(RightControl) key_up_to_fire_ms={:.2}",
        support::ms(at.saturating_duration_since(released))
    );
    assert_eq!(fired(&rx), 0, "one double tap fires once");

    // Used in a shortcut (Ctrl+F13 twice): no fire. F13 does nothing in any foreground app.
    chord(&[RCONTROL, KEY_F13]);
    chord(&[RCONTROL, KEY_F13]);
    assert_eq!(fired(&rx), 0, "right Ctrl used in a shortcut");

    // The left key is a different key.
    tap(LCONTROL);
    tap(LCONTROL);
    assert_eq!(
        fired(&rx),
        0,
        "left Ctrl must not fire a right-Ctrl listener"
    );

    // Too slow: no fire.
    tap(RCONTROL);
    sleep(Duration::from_millis(700));
    tap(RCONTROL);
    assert_eq!(fired(&rx), 0, "slow pair");

    // Triple tap fires once.
    tap(RCONTROL);
    tap(RCONTROL);
    tap(RCONTROL);
    assert_eq!(fired(&rx), 1, "triple tap");

    // Kill switch: Escape x3, alongside the double-tap listener.
    let (kcb, krx) = channel_callback();
    let kill = start_kill_switch(KillSwitch::default(), kcb).expect("start the kill switch");
    sleep(Duration::from_millis(200));
    tap(ESCAPE);
    tap(ESCAPE);
    tap(ESCAPE);
    assert_eq!(fired(&krx), 1, "Escape x3 kill switch");
    assert_eq!(fired(&rx), 0, "Escape must not fire the double tap");

    // Chord through RegisterHotKey.
    let (ccb, crx) = channel_callback();
    let chord_listener =
        start(Trigger::Chord("Ctrl+Alt+Space".into()), ccb).expect("register Ctrl+Alt+Space");
    sleep(Duration::from_millis(200));
    let pressed = Instant::now();
    chord(&[LCONTROL, LMENU, SPACE]);
    let at = crx
        .recv_timeout(Duration::from_secs(2))
        .expect("Ctrl+Alt+Space did not fire within 2 s");
    println!(
        "DOUBLETAP-HOTKEY os=windows trigger=chord(Ctrl+Alt+Space) press_to_fire_ms={:.2}",
        support::ms(at.saturating_duration_since(pressed))
    );
    assert_eq!(fired(&crx), 0, "one chord fires once");
    chord(&[LCONTROL, SPACE]);
    assert_eq!(fired(&crx), 0, "Ctrl+Space is not the chord");
    assert!(
        start(
            Trigger::Chord("Control+Option+Space".into()),
            Box::new(|| {})
        )
        .is_err(),
        "the same chord twice must be refused"
    );

    // Dropping stops each listener.
    drop(listener);
    drop(kill);
    drop(chord_listener);
    sleep(Duration::from_millis(150));
    tap(RCONTROL);
    tap(RCONTROL);
    tap(ESCAPE);
    tap(ESCAPE);
    tap(ESCAPE);
    chord(&[LCONTROL, LMENU, SPACE]);
    assert_eq!(
        fired(&rx) + fired(&krx) + fired(&crx),
        0,
        "fired after drop"
    );
    let again = start(Trigger::Chord("Ctrl+Alt+Space".into()), Box::new(|| {}));
    assert!(again.is_ok(), "re-register after drop: {:?}", again.err());
}

/// Kills the Notepad it started (unsaved text and all) when the test ends, pass or fail.
struct Notepad {
    file: std::path::PathBuf,
    child: std::process::Child,
}

impl Notepad {
    fn open() -> Self {
        let file =
            std::env::temp_dir().join(format!("nugget-doubletap-{}.txt", std::process::id()));
        std::fs::write(&file, b"").expect("create the scratch file");
        let child = std::process::Command::new("notepad.exe")
            .arg(&file)
            .spawn()
            .expect("start notepad.exe");
        Self { file, child }
    }
}

impl Drop for Notepad {
    fn drop(&mut self) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "notepad.exe"])
            .output();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.file);
    }
}

fn is_notepad(app: &nugget_core::ipc::AppInfo) -> bool {
    let id = app.id.as_deref().unwrap_or_default().to_ascii_lowercase();
    id == "notepad.exe" || app.name.eq_ignore_ascii_case("notepad")
}

#[test]
#[ignore = "opens Notepad and presses real keys on an interactive desktop; run with --ignored"]
fn windows_doubletap_into_notepad() {
    let a11y = nugget_a11y::system();
    let _notepad = Notepad::open();
    let focused = match support::wait_for_field(a11y.as_ref(), Duration::from_secs(10), is_notepad)
    {
        Ok(f) => f,
        Err(first) => {
            // Windows may keep the launcher in front (foreground lock): activate it the way
            // computer use does, and look again.
            println!("Notepad is not in front yet ({first}); activating it");
            nugget_a11y::system_computer()
                .open_app("notepad")
                .expect("activate Notepad");
            support::wait_for_field(a11y.as_ref(), Duration::from_secs(10), is_notepad)
                .unwrap_or_else(|e| panic!("{e}"))
        }
    };
    println!(
        "Notepad field: app={:?} id={:?} role={} value={:?} selection={:?}",
        focused.app.name,
        focused.app.id,
        focused.field.role,
        focused.field.value,
        focused.field.selection
    );
    assert!(!focused.field.secure);
    // The backend the shell's `doubletap::flow` builds on Windows.
    let backend = nugget_a11y::Backend::new(
        nugget_a11y::UiaPlatform::new(),
        nugget_a11y::A11yConfig::default(),
    );
    support::read_breakdown(&backend, "windows");
    for k in [RCONTROL, LCONTROL, LMENU] {
        send(k, false);
    }

    for (mode, chunks) in [
        (Mode::Shell, support::SHELL_CHUNKS),
        (Mode::Streamed, support::STREAMED_CHUNKS),
    ] {
        let pipeline = Pipeline::new(chunks);
        let (cb, rx) = channel_callback();
        let listener = start(right_ctrl_double_tap(), cb).expect("start the double-tap listener");
        sleep(Duration::from_millis(200));
        // The draft starts on its own thread at the fire; the listener stays up meanwhile, as in
        // the shell.
        let report = support::trigger_and_draft(&backend, &pipeline, mode, rx, |_| {
            tap(RCONTROL);
            tap(RCONTROL);
        })
        .unwrap_or_else(|e| panic!("{} mode: {e}", mode.as_str()));
        drop(listener);
        report.publish("windows");
        assert!(is_notepad(&report.app), "{:?}", report.app);
    }
}
