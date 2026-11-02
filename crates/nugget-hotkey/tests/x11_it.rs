//! X11 integration test: real listeners against a real X server, with keys injected through
//! XTest.
//!
//! Skipped unless `NUGGET_HOTKEY_IT=1`. Setup (no window manager or desktop needed):
//!
//! ```sh
//! Xvfb :99 -screen 0 1024x768x24 &
//! DISPLAY=:99 NUGGET_HOTKEY_SESSION=x11 NUGGET_HOTKEY_IT=1 \
//!     cargo test -p nugget-hotkey --test x11_it -- --nocapture
//! ```
//!
//! `NUGGET_HOTKEY_SESSION=x11` is only needed when running inside a Wayland desktop. Every
//! scenario runs in one test function because injected keys are global: parallel tests would
//! see each other's keystrokes.

#![cfg(target_os = "linux")]

mod support;

use std::thread::sleep;
use std::time::Duration;

use std::sync::mpsc;

use nugget_hotkey::{
    KillSwitch, Modifier, SuggestKey, Trigger, start, start_kill_switch, start_suggest_tap,
};
use support::{channel_callback, fired};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    Keycode, Window, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

const SHIFT_L: u32 = 0xffe1;
const CONTROL_L: u32 = 0xffe3;
const CONTROL_R: u32 = 0xffe4;
const ALT_L: u32 = 0xffe9;
const ESCAPE: u32 = 0xff1b;
const SPACE: u32 = 0x20;
const LETTER_A: u32 = 0x61;
const TAB: u32 = 0xff09;
const RIGHT: u32 = 0xff53;

/// A focused window standing in for the app the user types into: it counts the key presses
/// and releases that reach it.
struct App {
    conn: RustConnection,
}

impl App {
    fn new() -> Self {
        let (conn, screen) = x11rb::connect(None).expect("connect to $DISPLAY");
        let s = &conn.setup().roots[screen];
        let (root, visual) = (s.root, s.root_visual);
        let win = conn.generate_id().unwrap();
        conn.create_window(
            0,
            win,
            root,
            0,
            0,
            200,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();
        sleep(Duration::from_millis(100));
        conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        Self { conn }
    }

    /// (presses, releases) received since the last call.
    fn received(&self) -> (usize, usize) {
        sleep(Duration::from_millis(50));
        let (mut down, mut up) = (0, 0);
        while let Some(e) = self.conn.poll_for_event().unwrap() {
            match e {
                Event::KeyPress(_) => down += 1,
                Event::KeyRelease(_) => up += 1,
                _ => {}
            }
        }
        (down, up)
    }
}

fn suggest_keys(rx: &mpsc::Receiver<SuggestKey>) -> Vec<SuggestKey> {
    sleep(Duration::from_millis(80));
    rx.try_iter().collect()
}

struct Injector {
    conn: RustConnection,
    root: Window,
    keymap: Vec<(Keycode, u32)>,
}

impl Injector {
    fn new() -> Self {
        let (conn, screen) = x11rb::connect(None).expect("connect to $DISPLAY");
        let root = conn.setup().roots[screen].root;
        let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .unwrap()
            .reply()
            .unwrap();
        let per = usize::from(reply.keysyms_per_keycode);
        let keymap = reply
            .keysyms
            .chunks(per)
            .enumerate()
            .filter_map(|(i, s)| s.first().map(|&sym| (min + i as u8, sym)))
            .collect();
        Self { conn, root, keymap }
    }

    fn keycode(&self, keysym: u32) -> Keycode {
        self.keymap
            .iter()
            .find(|(_, s)| *s == keysym)
            .map(|(k, _)| *k)
            .unwrap_or_else(|| panic!("no keycode for keysym {keysym:#x} in the server keymap"))
    }

    fn send(&self, keysym: u32, press: bool) {
        let kind = if press {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        self.conn
            .xtest_fake_input(
                kind,
                self.keycode(keysym),
                x11rb::CURRENT_TIME,
                self.root,
                0,
                0,
                0,
            )
            .unwrap();
        self.conn.flush().unwrap();
        sleep(Duration::from_millis(30));
    }

    fn tap(&self, keysym: u32) {
        self.send(keysym, true);
        self.send(keysym, false);
        sleep(Duration::from_millis(40));
    }

    fn chord(&self, keysyms: &[u32]) {
        for &k in keysyms {
            self.send(k, true);
        }
        for &k in keysyms.iter().rev() {
            self.send(k, false);
        }
        sleep(Duration::from_millis(40));
    }
}

#[test]
fn x11_listeners_against_a_real_server() {
    if std::env::var("NUGGET_HOTKEY_IT").as_deref() != Ok("1") {
        eprintln!("skipped: set NUGGET_HOTKEY_IT=1 and DISPLAY (see the file header)");
        return;
    }
    let caps = nugget_hotkey::capabilities();
    assert_eq!(
        caps.session,
        nugget_hotkey::Session::X11,
        "set DISPLAY or NUGGET_HOTKEY_SESSION=x11"
    );
    let x = Injector::new();
    // Settle any modifiers left down by an earlier run.
    for k in [SHIFT_L, CONTROL_L, CONTROL_R, ALT_L] {
        x.send(k, false);
    }

    // Double tap of right Ctrl.
    let (cb, rx) = channel_callback();
    let trigger = Trigger::DoubleTap {
        modifier: Modifier::RightControl,
        window_ms: 350,
    };
    let listener = start(trigger, cb).expect("start the double-tap listener");
    sleep(Duration::from_millis(200));

    x.tap(CONTROL_R);
    x.tap(CONTROL_R);
    assert_eq!(fired(&rx), 1, "double tap of right Ctrl");

    // Used in a shortcut (Ctrl+A twice): no fire.
    x.chord(&[CONTROL_R, LETTER_A]);
    x.chord(&[CONTROL_R, LETTER_A]);
    assert_eq!(fired(&rx), 0, "right Ctrl used in a shortcut");

    // The left key is a different key.
    x.tap(CONTROL_L);
    x.tap(CONTROL_L);
    assert_eq!(
        fired(&rx),
        0,
        "left Ctrl must not trigger a right-Ctrl listener"
    );

    // Slow pair: no fire.
    x.tap(CONTROL_R);
    sleep(Duration::from_millis(700));
    x.tap(CONTROL_R);
    assert_eq!(fired(&rx), 0, "slow pair");

    // Held: no fire.
    x.send(CONTROL_R, true);
    sleep(Duration::from_millis(600));
    x.send(CONTROL_R, false);
    x.tap(CONTROL_R);
    assert_eq!(fired(&rx), 0, "hold then tap");

    // Triple tap fires once.
    x.tap(CONTROL_R);
    x.tap(CONTROL_R);
    x.tap(CONTROL_R);
    assert_eq!(fired(&rx), 1, "triple tap");

    // Kill switch: Escape three times, independently of the running double-tap listener.
    let (kcb, krx) = channel_callback();
    let kill = start_kill_switch(KillSwitch::default(), kcb).expect("start the kill switch");
    sleep(Duration::from_millis(200));
    x.tap(ESCAPE);
    x.tap(ESCAPE);
    x.tap(ESCAPE);
    assert_eq!(fired(&krx), 1, "Escape x3 kill switch");
    assert_eq!(fired(&rx), 0, "Escape must not trigger the double tap");

    // Chord through XGrabKey.
    let (ccb, crx) = channel_callback();
    let chord =
        start(Trigger::Chord("Ctrl+Alt+Space".into()), ccb).expect("start the chord listener");
    sleep(Duration::from_millis(200));
    x.chord(&[CONTROL_L, ALT_L, SPACE]);
    assert_eq!(fired(&crx), 1, "Ctrl+Alt+Space");
    x.chord(&[CONTROL_L, SPACE]);
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
    drop(chord);
    sleep(Duration::from_millis(100));
    x.tap(CONTROL_R);
    x.tap(CONTROL_R);
    x.tap(ESCAPE);
    x.tap(ESCAPE);
    x.tap(ESCAPE);
    x.chord(&[CONTROL_L, ALT_L, SPACE]);
    assert_eq!(
        fired(&rx) + fired(&krx) + fired(&crx),
        0,
        "fired after drop"
    );

    // A chord can be registered again once its listener is gone.
    let again = start(Trigger::Chord("Ctrl+Alt+Space".into()), Box::new(|| {}));
    assert!(again.is_ok(), "re-register after drop: {:?}", again.err());
    drop(again);

    suggest_tap_scenarios(&x);
}

fn suggest_tap_scenarios(x: &Injector) {
    use SuggestKey::*;
    let app = App::new();
    let (tx, rx) = mpsc::channel();
    let tap = start_suggest_tap(Box::new(move |k| {
        let _ = tx.send(k);
    }))
    .expect("start the suggestion tap");
    sleep(Duration::from_millis(200));
    app.received();

    // Inactive: Tab reaches the app and reports only a key press.
    x.tap(TAB);
    assert_eq!(app.received(), (1, 1), "inactive Tab must reach the app");
    assert_eq!(suggest_keys(&rx), [Typed]);

    // Active: Tab is consumed, press and release, and accepts.
    tap.set_active(true);
    x.tap(TAB);
    assert_eq!(app.received(), (0, 0), "active Tab must be consumed");
    assert_eq!(suggest_keys(&rx), [Accept]);

    // Escape dismisses, Ctrl+Right accepts a word; both consumed.
    x.tap(ESCAPE);
    assert_eq!(suggest_keys(&rx), [Dismiss]);
    x.chord(&[CONTROL_L, RIGHT]);
    assert_eq!(suggest_keys(&rx), [AcceptWord]);
    // Only Ctrl itself reached the app.
    assert_eq!(
        app.received(),
        (1, 1),
        "Escape and Ctrl+Right must be consumed"
    );

    // Shift+Tab and other keys pass through as typing.
    // A second accept works: the first Tab's release ended its consumption.
    x.tap(TAB);
    assert_eq!(suggest_keys(&rx), [Accept]);
    assert_eq!(app.received(), (0, 0));
    x.chord(&[SHIFT_L, TAB]);
    assert_eq!(suggest_keys(&rx), [Typed]);
    assert_eq!(app.received(), (2, 2), "Shift+Tab must reach the app");
    x.tap(LETTER_A);
    assert_eq!(suggest_keys(&rx), [Typed]);
    assert_eq!(app.received(), (1, 1));

    // Tab pressed while active, released after deactivation: the release is consumed too.
    x.send(TAB, true);
    tap.set_active(false);
    x.send(TAB, false);
    assert_eq!(suggest_keys(&rx), [Accept]);
    assert_eq!(
        app.received(),
        (0, 0),
        "orphan Tab release must not reach the app"
    );

    // Inactive again: everything reaches the app.
    x.tap(TAB);
    x.tap(ESCAPE);
    assert_eq!(app.received(), (2, 2));
    assert_eq!(suggest_keys(&rx), [Typed, Typed]);

    // Dropping the tap releases the grabs.
    tap.set_active(true);
    drop(tap);
    x.tap(TAB);
    assert_eq!(app.received(), (1, 1), "Tab must reach the app after drop");
    assert!(suggest_keys(&rx).is_empty(), "reported after drop");
}
