//! macOS: an active `CGEventTap` at the session level, on its own thread and run loop.
//!
//! Unlike the NSEvent monitors in `crate::macos`, an event tap is delivered by whichever run loop
//! its source is added to, so it needs neither the main thread nor AppKit. It is an *active*
//! (filtering) tap inserted at the head, so returning NULL from the callback swallows the event
//! before any app sees it. Creating it needs the Accessibility permission.
//!
//! The tap listens to key down/up and mouse down only. `flagsChanged` is not needed: the
//! modifiers are read from each key event's own flags. The callback classifies the event
//! ([`mac_tap_key`], [`mac_mods`]), asks the [`SuggestMachine`] and hands any [`SuggestKey`] to
//! the dispatcher; it never blocks. Events posted by this process (our own paste) are passed
//! through untouched, recognised by their source pid (`kCGEventSourceUnixProcessID`).

use std::cell::{OnceCell, RefCell};
use std::ffi::c_void;
use std::ptr::{NonNull, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use nugget_core::{Error, Result};
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use super::{SuggestKey, SuggestMachine, TapBackend};
use crate::chord::Platform;
use crate::engine::Fire;
use crate::keymap::{mac_mods, mac_tap_key};

/// Everything the callback needs. It lives on the tap thread's stack for the whole run loop and
/// is only touched from that thread.
struct Context {
    active: Arc<AtomicBool>,
    machine: RefCell<SuggestMachine>,
    emit: Fire<SuggestKey>,
    pid: i64,
    tap: OnceCell<CFRetained<CFMachPort>>,
}

#[allow(unsafe_code)]
unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let pass = event.as_ptr();
    // SAFETY: `user_info` is the `Context` that `run` keeps alive (and in place) until the tap is
    // invalidated, and the callback only runs on that same thread's run loop.
    let ctx = unsafe { &*(user_info as *const Context) };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(tap) = ctx.tap.get() {
            CGEvent::tap_enable(tap, true);
        }
        return pass;
    }
    // SAFETY: the event is valid for the duration of the callback.
    let ev = Some(unsafe { event.as_ref() });
    if CGEvent::integer_value_field(ev, CGEventField::EventSourceUnixProcessID) == ctx.pid {
        return pass;
    }
    let Ok(mut machine) = ctx.machine.try_borrow_mut() else {
        return pass;
    };
    let active = ctx.active.load(Ordering::Acquire);
    let verdict = if kind == CGEventType::KeyDown || kind == CGEventType::KeyUp {
        let code = CGEvent::integer_value_field(ev, CGEventField::KeyboardEventKeycode);
        // Only the class survives this line; the key code is dropped.
        let key = mac_tap_key(u16::try_from(code).unwrap_or(u16::MAX));
        let repeat = CGEvent::integer_value_field(ev, CGEventField::KeyboardEventAutorepeat) != 0;
        let mods = mac_mods(CGEvent::flags(ev).0);
        machine.key(key, kind == CGEventType::KeyDown, repeat, mods, active)
    } else if kind == CGEventType::LeftMouseDown
        || kind == CGEventType::RightMouseDown
        || kind == CGEventType::OtherMouseDown
    {
        machine.mouse(active)
    } else {
        return pass;
    };
    if let Some(key) = verdict.emit {
        ctx.emit.send(key);
    }
    if verdict.consume { null_mut() } else { pass }
}

fn mask() -> CGEventMask {
    [
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::LeftMouseDown,
        CGEventType::RightMouseDown,
        CGEventType::OtherMouseDown,
    ]
    .into_iter()
    .fold(0, |m, t| m | (1 << t.0))
}

/// The tap thread's run loop, for stopping it from the handle.
struct RunLoop(CFRetained<CFRunLoop>);

// SAFETY: only `CFRunLoopStop` and `CFRunLoopWakeUp` are called through this handle, and Apple
// documents both as safe to call on another thread's run loop.
#[allow(unsafe_code)]
unsafe impl Send for RunLoop {}
// SAFETY: as above.
#[allow(unsafe_code)]
unsafe impl Sync for RunLoop {}

struct MacTap {
    run_loop: RunLoop,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TapBackend for MacTap {}

impl Drop for MacTap {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.run_loop.0.stop();
        self.run_loop.0.wake_up();
        if let Some(thread) = self.thread.take() {
            // The tap thread never runs user code (that is the dispatcher's), so this cannot
            // deadlock, even when the dispatcher's callback drops the tap.
            let _ = thread.join();
        }
    }
}

const REFUSED: &str = "macOS refused the event tap for ghost-text suggestion keys. Grant Heed \
     Accessibility (and Input Monitoring) in System Settings > Privacy & Security, then restart \
     Heed.";

#[allow(unsafe_code)]
fn run(
    active: Arc<AtomicBool>,
    emit: Fire<SuggestKey>,
    stopping: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<RunLoop>>,
) {
    let ctx = Box::new(Context {
        active,
        machine: RefCell::new(SuggestMachine::new(Platform::MacOs)),
        emit,
        pid: i64::from(std::process::id()),
        tap: OnceCell::new(),
    });
    let user_info = (&*ctx as *const Context).cast_mut().cast::<c_void>();
    // SAFETY: `callback` matches CGEventTapCallBack and only dereferences `user_info` as the
    // `Context`, which outlives the tap (it is invalidated below before `ctx` drops).
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask(),
            Some(callback),
            user_info,
        )
    };
    let Some(tap) = tap else {
        let _ = ready.send(Err(Error::Other(REFUSED.into())));
        return;
    };
    let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0);
    let (Some(source), Some(run_loop)) = (source, CFRunLoop::current()) else {
        tap.invalidate();
        let _ = ready.send(Err(Error::Other(
            "cannot attach the ghost-text event tap to a run loop".into(),
        )));
        return;
    };
    // SAFETY: reading an immutable CoreFoundation constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    run_loop.add_source(Some(&source), mode);
    let _ = ctx.tap.set(tap.clone());
    CGEvent::tap_enable(&tap, true);
    if ready.send(Ok(RunLoop(run_loop))).is_ok() {
        // Short slices, so a stop that lands before the loop starts is still seen.
        while !stopping.load(Ordering::Acquire) {
            CFRunLoop::run_in_mode(mode, 0.5, false);
        }
    }
    CGEvent::tap_enable(&tap, false);
    // Invalidating removes the source from the run loop; no callback can run after this.
    tap.invalidate();
    drop(source);
    drop(ctx);
}

pub(super) fn start(
    active: Arc<AtomicBool>,
    emit: Fire<SuggestKey>,
) -> Result<Box<dyn TapBackend>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    let s = stopping.clone();
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-suggest".into())
        .spawn(move || run(active, emit, s, ready_tx))
        .map_err(|e| Error::Other(format!("cannot start the ghost-text tap thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok(run_loop)) => Ok(Box::new(MacTap {
            run_loop,
            stopping,
            thread: Some(thread),
        })),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(Error::Other(
                "the ghost-text tap thread exited during start-up".into(),
            ))
        }
    }
}
