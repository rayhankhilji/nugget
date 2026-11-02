//! X11: XInput2 raw events decide, `XGrabKey` consumes.
//!
//! Raw key and button events (XInput 2.2 announced, so they arrive even while a key is grabbed)
//! are classified through a 256-entry keycode table into [`TapKey`]s and modifiers, and fed to
//! the [`SuggestMachine`]. X11 cannot swallow an event from a raw-event listener, so while the
//! tap is active it also holds passive grabs on the root window for Tab and Escape (no
//! modifiers) and Right (Control only), each with every combination of Lock and NumLock. A
//! grabbed press goes to this connection instead of the focused app (it is drained and
//! dropped), and X keeps the keyboard grabbed until that key is released, so its auto-repeats
//! and release are swallowed too, even after the passive grab is removed. While that grab is
//! active the server may withhold the raw release (Xvfb does), so the grab's core `KeyRelease`
//! also ends the machine's consumed-key state.
//!
//! Limitations (best effort):
//! - The grab decides what is swallowed and the raw events decide what is reported; they agree
//!   except in a race where the tap is (de)activated between the press and its processing.
//! - Activating a grab sends the focused window `FocusOut`/`FocusIn` with mode `NotifyGrab` /
//!   `NotifyUngrab`, which most toolkits ignore.
//! - A key cannot be grabbed while another client holds the same grab; that key then reaches
//!   the app (a warning is logged once, without naming the key).
//! - X11 does not say which client injected an event, so events this process injects through
//!   XTest are seen like any other input: they report `Typed`, and while active a grabbed key
//!   would be swallowed. Deactivate the tap, and ignore `Typed`, while injecting keys.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use nugget_core::{Error, Result};
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, KeyEventFlags, XIEventMask};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
    Keycode, ModMask, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::{Mods, SuggestKey, SuggestMachine, TapBackend, TapKey};
use crate::Modifier;
use crate::chord::Platform;
use crate::engine::Fire;
use crate::keymap::{mods_from_held, x11_keysym, x11_tap_key};
use crate::tap::KeyClass;

fn x_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Other(format!("X11 {what}: {e}"))
}

const TAB: u32 = 0xff09;
const ESCAPE: u32 = 0xff1b;
const RIGHT: u32 = 0xff53;
const NUM_LOCK: u32 = 0xff7f;

/// Per keycode: the suggestion class and, for a modifier key, which modifier.
#[derive(Clone, Copy)]
struct Entry {
    key: TapKey,
    modifier: Option<Modifier>,
}

struct Keymap {
    table: [Entry; 256],
    /// Passive grabs to hold while active: (keycode, modifiers).
    grabs: Vec<(Keycode, u16)>,
}

fn keymap(conn: &RustConnection) -> Result<Keymap> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| x_err("keyboard mapping", e))?
        .reply()
        .map_err(|e| x_err("keyboard mapping", e))?;
    let per = usize::from(reply.keysyms_per_keycode.max(1));
    let mut table = [Entry {
        key: TapKey::Other,
        modifier: None,
    }; 256];
    let mut targets = Vec::new();
    let mut num_lock_codes = Vec::new();
    for (i, syms) in reply.keysyms.chunks(per).enumerate() {
        let Ok(keycode) = Keycode::try_from(usize::from(min) + i) else {
            continue;
        };
        let Some(&sym) = syms.first() else { continue };
        let modifier = match x11_keysym(sym) {
            KeyClass::Modifier(m) => Some(m),
            _ => None,
        };
        table[usize::from(keycode)] = Entry {
            key: x11_tap_key(sym),
            modifier,
        };
        match sym {
            TAB | ESCAPE => targets.push((keycode, 0)),
            RIGHT => targets.push((keycode, u16::from(ModMask::CONTROL))),
            NUM_LOCK => num_lock_codes.push(keycode),
            _ => {}
        }
    }
    // Which of the eight modifier rows NumLock sits in (usually Mod2).
    let mut num_lock = 0u16;
    if let Ok(reply) = conn
        .get_modifier_mapping()
        .map_err(|e| x_err("modifier mapping", e))?
        .reply()
    {
        let per = (reply.keycodes.len() / 8).max(1);
        for (row, codes) in reply.keycodes.chunks(per).enumerate().take(8) {
            if codes.iter().any(|c| *c != 0 && num_lock_codes.contains(c)) {
                num_lock |= 1 << row;
            }
        }
    }
    let lock = u16::from(ModMask::LOCK);
    let mut extras = vec![0, lock];
    if num_lock != 0 && num_lock != lock {
        extras.extend([num_lock, num_lock | lock]);
    }
    let grabs = targets
        .into_iter()
        .flat_map(|(code, base)| extras.iter().map(move |extra| (code, base | extra)))
        .collect();
    Ok(Keymap { table, grabs })
}

struct Grabs {
    keys: Vec<(Keycode, u16)>,
    held: bool,
    warned: bool,
}

struct Shared {
    conn: RustConnection,
    root: Window,
    grabs: Mutex<Grabs>,
}

impl Shared {
    fn grab(&self, g: &mut Grabs) {
        let mut cookies = Vec::with_capacity(g.keys.len());
        for &(code, mods) in &g.keys {
            match self.conn.grab_key(
                false,
                self.root,
                ModMask::from(mods),
                code,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            ) {
                Ok(c) => cookies.push(c),
                Err(e) => tracing::warn!(error = %e, "nugget-hotkey: X11 grab request failed"),
            }
        }
        // Checking each request waits for the server, so the keys are grabbed on return.
        let refused = cookies
            .into_iter()
            .map(|c| c.check())
            .filter(Result::is_err)
            .count();
        if refused > 0 && !g.warned {
            g.warned = true;
            tracing::warn!(
                refused,
                "nugget-hotkey: another X11 client holds a ghost-text key grab; that key will \
                 also reach the focused app"
            );
        }
        g.held = true;
    }

    fn ungrab(&self, g: &mut Grabs) {
        for &(code, mods) in &g.keys {
            let _ = self.conn.ungrab_key(code, self.root, ModMask::from(mods));
        }
        let _ = self.conn.flush();
        g.held = false;
    }

    fn set_active(&self, active: bool) {
        let mut g = self.grabs.lock().unwrap_or_else(|e| e.into_inner());
        match (active, g.held) {
            (true, false) => self.grab(&mut g),
            (false, true) => self.ungrab(&mut g),
            _ => {}
        }
    }

    /// The keymap changed: move the grabs to the new keycodes.
    fn regrab(&self, keys: Vec<(Keycode, u16)>) {
        let mut g = self.grabs.lock().unwrap_or_else(|e| e.into_inner());
        let held = g.held;
        if held {
            self.ungrab(&mut g);
        }
        g.keys = keys;
        if held {
            self.grab(&mut g);
        }
    }
}

struct X11Tap {
    shared: Arc<Shared>,
    wake: Window,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TapBackend for X11Tap {
    fn set_active(&self, active: bool) {
        self.shared.set_active(active);
    }
}

impl Drop for X11Tap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.shared.set_active(false);
        let conn = &self.shared.conn;
        let msg = ClientMessageEvent::new(32, self.wake, AtomEnum::NONE, [0u32; 5]);
        let _ = conn.send_event(false, self.wake, EventMask::NO_EVENT, msg);
        let _ = conn.flush();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn entry(table: &[Entry; 256], keycode: u32) -> Entry {
    usize::try_from(keycode)
        .ok()
        .and_then(|k| table.get(k).copied())
        .unwrap_or(Entry {
            key: TapKey::Other,
            modifier: None,
        })
}

fn listen(
    shared: &Shared,
    mut table: [Entry; 256],
    active: &AtomicBool,
    emit: &Fire<SuggestKey>,
    stop: &AtomicBool,
) {
    let mut machine = SuggestMachine::new(Platform::Linux);
    // Held modifiers, one bit per `Modifier`.
    let mut held: u8 = 0;
    let mods = |held: u8| -> Mods {
        mods_from_held(
            Modifier::ALL
                .into_iter()
                .filter(|m| held & (1 << (*m as u8)) != 0),
        )
    };
    loop {
        let event = match shared.conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "nugget-hotkey: X11 connection lost");
                break;
            }
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        let verdict = match event {
            Event::XinputRawKeyPress(e) | Event::XinputRawKeyRelease(e) => {
                let down = e.event_type == xinput::RAW_KEY_PRESS_EVENT;
                // Only the class survives this line; the keycode is dropped.
                let Entry { key, modifier } = entry(&table, e.detail);
                if let Some(m) = modifier {
                    let bit = 1 << (m as u8);
                    held = if down { held | bit } else { held & !bit };
                }
                let repeat = e.flags.contains(KeyEventFlags::KEY_REPEAT);
                machine.key(
                    key,
                    down,
                    repeat,
                    mods(held),
                    active.load(Ordering::Acquire),
                )
            }
            // Buttons 4–7 are scroll wheels, not clicks.
            Event::XinputRawButtonPress(e) if !(4..=7).contains(&e.detail) => {
                machine.mouse(active.load(Ordering::Acquire))
            }
            Event::MappingNotify(_) => {
                match keymap(&shared.conn) {
                    Ok(k) => {
                        table = k.table;
                        shared.regrab(k.grabs);
                    }
                    Err(e) => tracing::warn!(error = %e, "nugget-hotkey: X11 keymap reload"),
                }
                continue;
            }
            // The release of a key our grab swallowed. X servers do not reliably send the raw
            // release of a key while this client holds the keyboard grab it caused, so the
            // grab's own core release ends the consumed key. Releases never report anything,
            // and a second release (if the raw one does arrive) is a no-op.
            Event::KeyRelease(e) => {
                let key = entry(&table, u32::from(e.detail)).key;
                machine.key(
                    key,
                    false,
                    false,
                    Mods::NONE,
                    active.load(Ordering::Acquire),
                )
            }
            // Core key presses from our grabs (the raw press decides), errors and the rest.
            _ => continue,
        };
        if let Some(key) = verdict.emit {
            emit.send(key);
        }
    }
}

pub(super) fn start(
    active: Arc<AtomicBool>,
    emit: Fire<SuggestKey>,
) -> Result<Box<dyn TapBackend>> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| x_err("connect", e))?;
    let root = conn
        .setup()
        .roots
        .get(screen)
        .ok_or_else(|| x_err("screen", screen))?
        .root;
    if conn
        .extension_information(xinput::X11_EXTENSION_NAME)
        .map_err(|e| x_err("XInput query", e))?
        .is_none()
    {
        return Err(Error::Unsupported(
            "the X server has no XInput extension".into(),
        ));
    }
    let version = conn
        .xinput_xi_query_version(2, 2)
        .map_err(|e| x_err("XInput version", e))?
        .reply()
        .map_err(|e| x_err("XInput version", e))?;
    if version.major_version < 2 {
        return Err(Error::Unsupported(
            "the X server does not support XInput 2".into(),
        ));
    }
    let Keymap { table, grabs } = keymap(&conn)?;
    let mask =
        XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE | XIEventMask::RAW_BUTTON_PRESS;
    conn.xinput_xi_select_events(
        root,
        &[xinput::EventMask {
            deviceid: xinput::Device::ALL_MASTER.into(),
            mask: vec![mask],
        }],
    )
    .map_err(|e| x_err("select events", e))?
    .check()
    .map_err(|e| x_err("select events", e))?;

    let wake = conn.generate_id().map_err(|e| x_err("id", e))?;
    conn.create_window(
        0,
        wake,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )
    .map_err(|e| x_err("create window", e))?
    .check()
    .map_err(|e| x_err("create window", e))?;

    let shared = Arc::new(Shared {
        conn,
        root,
        grabs: Mutex::new(Grabs {
            keys: grabs,
            held: false,
            warned: false,
        }),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (sh, st) = (shared.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-suggest".into())
        .spawn(move || listen(&sh, table, &active, &emit, &st))
        .map_err(|e| Error::Other(format!("cannot start the X11 ghost-text thread: {e}")))?;
    Ok(Box::new(X11Tap {
        shared,
        wake,
        stop,
        thread: Some(thread),
    }))
}
