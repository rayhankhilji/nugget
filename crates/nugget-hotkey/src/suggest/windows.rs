//! Windows: the suggestion tap's own `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks, on a thread with
//! its own message loop. Returning 1 from the keyboard hook swallows a consumed key.
//!
//! The hook classifies each event ([`windows_tap_key`]); for Tab, Escape and Right presses it
//! reads the modifier state with `GetAsyncKeyState` (which, inside a low-level hook, reflects
//! every key except the one being reported). Injected events (`LLKHF_INJECTED`,
//! `LLMHF_INJECTED`) are all ignored, which covers this process's own `SendInput` paste; input
//! injected by other software (on-screen keyboards, remote-control tools) is ignored too, so it
//! neither accepts nor reports typing.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use nugget_core::{Error, Result};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSLLHOOKSTRUCT,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_RBUTTONDOWN, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_XBUTTONDOWN,
};

use super::{Mods, SuggestKey, SuggestMachine, TapBackend, TapKey, Verdict};
use crate::chord::Platform;
use crate::engine::Fire;
use crate::keymap::windows_tap_key;
use crate::windows::{Hooks, WinListener, ensure_queue_and_thread_id, pump};

struct HookState {
    active: Arc<AtomicBool>,
    machine: SuggestMachine,
    emit: Fire<SuggestKey>,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

impl TapBackend for WinListener {}

#[allow(unsafe_code)]
fn held(vk: i32) -> bool {
    // SAFETY: no preconditions; the high bit is "down now".
    unsafe { GetAsyncKeyState(vk) < 0 }
}

fn current_mods() -> Mods {
    Mods {
        shift: held(0x10),                 // VK_SHIFT
        control: held(0x11),               // VK_CONTROL
        option: held(0x12),                // VK_MENU
        command: held(0x5B) || held(0x5C), // VK_LWIN, VK_RWIN
    }
}

/// Run the machine and hand any report to the dispatcher; `true` means swallow the event.
fn decide(f: impl FnOnce(&mut SuggestMachine, bool) -> Verdict) -> bool {
    STATE.with_borrow_mut(|state| {
        let Some(state) = state else { return false };
        let verdict = f(&mut state.machine, state.active.load(Ordering::Acquire));
        if let Some(key) = verdict.emit {
            state.emit.send(key);
        }
        verdict.consume
    })
}

#[allow(unsafe_code)]
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam as u32;
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        if down || matches!(message, WM_KEYUP | WM_SYSKEYUP) {
            // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT valid during this call.
            let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            if info.flags & LLKHF_INJECTED == 0 {
                // Only the class survives this line; the virtual-key code is dropped.
                let key = windows_tap_key(info.vkCode);
                let mods = if down && matches!(key, TapKey::Tab | TapKey::Escape | TapKey::Right) {
                    current_mods()
                } else {
                    Mods::NONE
                };
                // The low-level hook has no repeat flag; the machine treats a key-down of a
                // consumed key that is still held as a repeat.
                if decide(|m, active| m.key(key, down, false, mods, active)) {
                    return 1;
                }
            }
        }
    }
    // SAFETY: passing the hook's own arguments on, as every hook must.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

#[allow(unsafe_code)]
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && matches!(
            wparam as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        // SAFETY: for HC_ACTION, lparam points to an MSLLHOOKSTRUCT valid during this call.
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            // Never consumed.
            decide(|m, active| m.mouse(active));
        }
    }
    // SAFETY: as above.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn run(active: Arc<AtomicBool>, emit: Fire<SuggestKey>, ready: mpsc::Sender<Result<u32>>) {
    let thread_id = ensure_queue_and_thread_id();
    STATE.set(Some(HookState {
        active,
        machine: SuggestMachine::new(Platform::Windows),
        emit,
    }));
    let hooks = match Hooks::install_with(keyboard_proc, mouse_proc) {
        Ok(h) => h,
        Err(e) => {
            STATE.set(None);
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(thread_id));
    pump();
    drop(hooks);
    STATE.set(None);
}

pub(super) fn start(
    active: Arc<AtomicBool>,
    emit: Fire<SuggestKey>,
) -> Result<Box<dyn TapBackend>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-suggest".into())
        .spawn(move || run(active, emit, ready_tx))
        .map_err(|e| Error::Other(format!("cannot start the ghost-text hook thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok(thread_id)) => Ok(Box::new(WinListener {
            thread_id,
            thread: Some(thread),
        })),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(Error::Other(
                "the ghost-text hook thread exited during start-up".into(),
            ))
        }
    }
}
