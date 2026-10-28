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

use std::sync::mpsc;
use std::thread::sleep;
use std::time::Duration;

use nugget_hotkey::{KillSwitch, Modifier, Trigger, start, start_kill_switch};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Keycode, Window,
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

fn channel_callback() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    (
        Box::new(move || {
            let _ = tx.lock().unwrap().send(());
        }),
        rx,
    )
}

fn fired(rx: &mpsc::Receiver<()>) -> usize {
    let mut n = 0;
    while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
        n += 1;
    }
    n
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
    let listener = start(
        Trigger::DoubleTap {
            modifier: Modifier::RightControl,
            window_ms: 350,
        },
        cb,
    )
    .expect("start the double-tap listener");
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
}
