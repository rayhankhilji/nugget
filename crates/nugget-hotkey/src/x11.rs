//! X11: XInput2 raw key and button events on the root window, through `x11rb` (pure Rust).
//!
//! Raw events report every key press system-wide without grabbing anything, so input still
//! reaches the focused app. Each keycode is classified once, from the server's keymap, into a
//! 256-entry table that knows only "this modifier", "Escape" or "other"; raw event key codes are
//! looked up in that table and immediately dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use nugget_core::{Error, Result};
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, KeyEventFlags, XIEventMask};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, Window,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::engine::{Engine, Fire};
use crate::keymap::x11_keysym;
use crate::tap::{Detector, KeyClass};

fn x_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Other(format!("X11 {what}: {e}"))
}

/// keycode → class, for the 256 possible keycodes.
fn key_table(conn: &RustConnection) -> Result<[KeyClass; 256]> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| x_err("keyboard mapping", e))?
        .reply()
        .map_err(|e| x_err("keyboard mapping", e))?;
    let per = usize::from(reply.keysyms_per_keycode.max(1));
    let mut table = [KeyClass::Other; 256];
    for (i, syms) in reply.keysyms.chunks(per).enumerate() {
        let keycode = usize::from(min) + i;
        if let (Some(slot), Some(&sym)) = (table.get_mut(keycode), syms.first()) {
            *slot = x11_keysym(sym);
        }
    }
    Ok(table)
}

struct X11Listener {
    conn: Arc<RustConnection>,
    wake: Window,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for X11Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocked event loop with a message to our own window.
        let msg = ClientMessageEvent::new(32, self.wake, AtomEnum::NONE, [0u32; 5]);
        let _ = self
            .conn
            .send_event(false, self.wake, EventMask::NO_EVENT, msg);
        let _ = self.conn.flush();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn start_detector(detector: Detector, fire: Fire) -> Result<Box<dyn Send>> {
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
        .xinput_xi_query_version(2, 0)
        .map_err(|e| x_err("XInput version", e))?
        .reply()
        .map_err(|e| x_err("XInput version", e))?;
    if version.major_version < 2 {
        return Err(Error::Unsupported(
            "the X server does not support XInput 2".into(),
        ));
    }

    let mut table = key_table(&conn)?;
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

    let conn = Arc::new(conn);
    let stop = Arc::new(AtomicBool::new(false));
    let (c, s) = (conn.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-x11".into())
        .spawn(move || {
            let mut engine = Engine::new(detector, fire);
            loop {
                let event = match c.wait_for_event() {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "nugget-hotkey: X11 connection lost");
                        break;
                    }
                };
                if s.load(Ordering::Acquire) {
                    break;
                }
                match event {
                    Event::XinputRawKeyPress(e) => {
                        if !e.flags.contains(KeyEventFlags::KEY_REPEAT) {
                            engine.key(lookup(&table, e.detail), true);
                        }
                    }
                    Event::XinputRawKeyRelease(e) => engine.key(lookup(&table, e.detail), false),
                    // Buttons 4–7 are scroll wheels, not clicks.
                    Event::XinputRawButtonPress(e) if !(4..=7).contains(&e.detail) => {
                        engine.mouse()
                    }
                    Event::MappingNotify(_) => {
                        if let Ok(t) = key_table(&c) {
                            table = t;
                        }
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| Error::Other(format!("cannot start the X11 hotkey thread: {e}")))?;
    Ok(Box::new(X11Listener {
        conn,
        wake,
        stop,
        thread: Some(thread),
    }))
}

fn lookup(table: &[KeyClass; 256], keycode: u32) -> KeyClass {
    usize::try_from(keycode)
        .ok()
        .and_then(|k| table.get(k).copied())
        .unwrap_or(KeyClass::Other)
}
