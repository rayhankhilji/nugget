//! macOS: NSEvent global and local monitors.
//!
//! `flagsChanged` events carry the device-dependent modifier bits (`NX_DEVICE*KEYMASK`), which
//! tell left from right. Global monitors for `flagsChanged` and mouse-down events need no
//! permission. Global key-down events (used only to cancel a tap, and for the Escape kill
//! switch) are delivered only once the app has Accessibility permission; without it a shortcut
//! typed into another app cannot cancel a tap, but the hold limit still rules out most shortcut
//! use. The local monitor covers events sent to Nugget's own windows, which global monitors
//! never see.
//!
//! Monitors run on the main thread and are delivered by the main run loop, so the host app must
//! be running its `NSApplication` event loop. A spawned `CFRunLoop` thread does not work:
//! AppKit only dispatches monitors on the main thread.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dispatch2::{DispatchQueue, MainThreadBound};
use nugget_core::{Error, Result};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};

use crate::engine::{Engine, Fire};
use crate::keymap::{mac_flag_changes, mac_keycode};
use crate::tap::{Detector, KeyClass};

/// Run `f` on the main thread: now if this is the main thread, otherwise asynchronously on the
/// main dispatch queue.
pub(crate) fn on_main<F: FnOnce(MainThreadMarker) + Send + 'static>(f: F) {
    if let Some(mtm) = MainThreadMarker::new() {
        f(mtm);
    } else {
        DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                f(mtm);
            }
        });
    }
}

/// [`on_main`] for fallible setup: the error is returned when already on the main thread, and
/// logged when the work had to be queued.
pub(crate) fn run_on_main<F>(what: &'static str, f: F) -> Result<()>
where
    F: FnOnce(MainThreadMarker) -> Result<()> + Send + 'static,
{
    if let Some(mtm) = MainThreadMarker::new() {
        return f(mtm);
    }
    on_main(move |mtm| {
        if let Err(e) = f(mtm) {
            tracing::warn!(error = %e, "nugget-hotkey: {what} failed on the main thread");
        }
    });
    Ok(())
}

struct State {
    engine: Engine,
    flags: u64,
}

fn handle(state: &Mutex<State>, event: &NSEvent) {
    let Ok(mut s) = state.lock() else { return };
    let kind = event.r#type();
    if kind == NSEventType::FlagsChanged {
        let flags = event.modifierFlags().0 as u64;
        let previous = std::mem::replace(&mut s.flags, flags);
        for (m, down) in mac_flag_changes(previous, flags) {
            s.engine.key(KeyClass::Modifier(m), down);
        }
    } else if kind == NSEventType::KeyDown || kind == NSEventType::KeyUp {
        // Only the class survives: a modifier, Escape, or "some key".
        let class = match mac_keycode(event.keyCode()) {
            KeyClass::Modifier(_) => return, // reported through flagsChanged
            other => other,
        };
        s.engine.key(class, kind == NSEventType::KeyDown);
    } else if kind == NSEventType::LeftMouseDown
        || kind == NSEventType::RightMouseDown
        || kind == NSEventType::OtherMouseDown
    {
        s.engine.mouse();
    }
}

type Monitors = Vec<Retained<AnyObject>>;

#[allow(unsafe_code)]
fn install(state: Arc<Mutex<State>>) -> Result<Monitors> {
    let mask = NSEventMask::FlagsChanged
        | NSEventMask::KeyDown
        | NSEventMask::KeyUp
        | NSEventMask::LeftMouseDown
        | NSEventMask::RightMouseDown
        | NSEventMask::OtherMouseDown;

    let global_state = state.clone();
    let global = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit calls the handler with a valid NSEvent that outlives the call.
        handle(&global_state, unsafe { event.as_ref() });
    });
    let global = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global)
        .ok_or_else(|| Error::Other("AppKit refused the global event monitor".into()))?;

    let local = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: as above; the event is returned unchanged so the app still receives it.
        handle(&state, unsafe { event.as_ref() });
        event.as_ptr()
    });
    // SAFETY: the block returns the valid event it was given, which is what AppKit requires.
    let local = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local) };
    let Some(local) = local else {
        remove(vec![global]);
        return Err(Error::Other(
            "AppKit refused the local event monitor".into(),
        ));
    };
    Ok(vec![global, local])
}

#[allow(unsafe_code)]
fn remove(monitors: Monitors) {
    for monitor in monitors {
        // SAFETY: each object was returned by `addGlobalMonitor…`/`addLocalMonitor…` and is
        // removed once, on the main thread.
        unsafe { NSEvent::removeMonitor(&monitor) };
    }
}

type Slot = Arc<Mutex<Option<MainThreadBound<Monitors>>>>;

struct MacListener {
    slot: Slot,
    cancelled: Arc<AtomicBool>,
}

impl Drop for MacListener {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let slot = self.slot.clone();
        on_main(move |mtm| {
            let taken = slot.lock().ok().and_then(|mut s| s.take());
            if let Some(bound) = taken {
                remove(bound.into_inner(mtm));
            }
        });
    }
}

pub(crate) fn start_detector(detector: Detector, fire: Fire) -> Result<Box<dyn Send>> {
    let state = Arc::new(Mutex::new(State {
        engine: Engine::new(detector, fire),
        flags: 0,
    }));
    let slot: Slot = Arc::new(Mutex::new(None));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (s, c) = (slot.clone(), cancelled.clone());
    run_on_main("event monitor installation", move |mtm| {
        if c.load(Ordering::Acquire) {
            return Ok(());
        }
        let monitors = install(state)?;
        if let Ok(mut guard) = s.lock() {
            *guard = Some(MainThreadBound::new(monitors, mtm));
        }
        Ok(())
    })?;
    Ok(Box::new(MacListener { slot, cancelled }))
}
