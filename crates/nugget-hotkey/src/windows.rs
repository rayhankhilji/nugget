//! Windows: low-level keyboard and mouse hooks for detectors, `RegisterHotKey` (through
//! `global-hotkey`) for chords. Each listener owns a thread with its own message loop, since both
//! deliver to the installing thread's queue.
//!
//! The hooks never block or swallow input: each event is classified (only modifiers and, for the
//! kill switch, Escape keep an identity), fed to the detector, and passed on.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::thread::JoinHandle;

use global_hotkey::GlobalHotKeyManager;
use nugget_core::{Error, Result};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_XBUTTONDOWN,
};

use crate::chord::Chord;
use crate::engine::{Engine, Fire};
use crate::ghk;
use crate::keymap::windows_vk;
use crate::tap::Detector;

pub(crate) enum Mode {
    Detect(Detector),
    Chord(Chord),
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

#[allow(unsafe_code)]
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam as u32;
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        if down || matches!(message, WM_KEYUP | WM_SYSKEYUP) {
            // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT valid during this call.
            let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            // AltGr's fake Left Ctrl shows up as scan code 0x21D, either literally or as 0x1D with
            // the extended flag (LLKHF_EXTENDED, bit 0) set; a real Left Ctrl is never extended.
            let scan = info.scanCode | ((info.flags & 1) << 9);
            if let Some(class) = windows_vk(info.vkCode, scan) {
                ENGINE.with_borrow_mut(|engine| {
                    if let Some(engine) = engine {
                        engine.key(class, down);
                    }
                });
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
        ENGINE.with_borrow_mut(|engine| {
            if let Some(engine) = engine {
                engine.mouse();
            }
        });
    }
    // SAFETY: as above.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

struct Hooks(Vec<HHOOK>);

impl Hooks {
    #[allow(unsafe_code)]
    fn install() -> Result<Self> {
        let mut hooks = Self(Vec::new());
        // SAFETY: a null name returns this process's module handle; it is never freed.
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        for (id, proc_) in [
            (
                WH_KEYBOARD_LL,
                keyboard_proc as unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
            ),
            (WH_MOUSE_LL, mouse_proc),
        ] {
            // SAFETY: the procedures match HOOKPROC and live for the whole program; the hook is
            // global (thread id 0), as low-level hooks must be, and removed in `Drop`.
            let hook = unsafe { SetWindowsHookExW(id, Some(proc_), module, 0) };
            if hook.is_null() {
                return Err(Error::Other(format!(
                    "SetWindowsHookExW failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            hooks.0.push(hook);
        }
        Ok(hooks)
    }
}

impl Drop for Hooks {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        for hook in self.0.drain(..) {
            // SAFETY: each handle came from SetWindowsHookExW on this thread and is unhooked once.
            unsafe { UnhookWindowsHookEx(hook) };
        }
    }
}

#[allow(unsafe_code)]
fn ensure_queue_and_thread_id() -> u32 {
    // SAFETY: MSG is plain data, and an all-zero MSG is a valid out-parameter.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: peeking (without removing) creates this thread's message queue, so a WM_QUIT posted
    // right after start-up is not lost.
    unsafe { PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE) };
    // SAFETY: no preconditions.
    unsafe { GetCurrentThreadId() }
}

#[allow(unsafe_code)]
fn pump() {
    // SAFETY: as above.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: msg is a valid out-parameter; a null window means "any message of this thread".
        let r = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break; // WM_QUIT, or an error
        }
        // SAFETY: msg was just filled in by GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn run(mode: Mode, fire: Fire, ready: mpsc::Sender<Result<u32>>) {
    let thread_id = ensure_queue_and_thread_id();
    match mode {
        Mode::Detect(detector) => {
            ENGINE.set(Some(Engine::new(detector, fire)));
            let hooks = match Hooks::install() {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready.send(Err(e));
                    return;
                }
            };
            let _ = ready.send(Ok(thread_id));
            pump();
            drop(hooks);
            ENGINE.set(None);
        }
        Mode::Chord(chord) => {
            let setup = (|| {
                let hotkey = ghk::hotkey(&chord)?;
                let route = ghk::add_route(&hotkey, &chord, fire)?;
                // The manager's hidden window belongs to this thread, so WM_HOTKEY arrives here.
                let manager = GlobalHotKeyManager::new().map_err(|e| ghk::os_error(&chord, e))?;
                manager
                    .register(hotkey)
                    .map_err(|e| ghk::os_error(&chord, e))?;
                Ok((hotkey, route, manager))
            })();
            match setup {
                Ok((hotkey, route, manager)) => {
                    let _ = ready.send(Ok(thread_id));
                    pump();
                    let _ = manager.unregister(hotkey);
                    drop(manager);
                    drop(route);
                }
                Err(e) => {
                    let _ = ready.send(Err(e));
                }
            }
        }
    }
}

struct WinListener {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Drop for WinListener {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: posting to a thread id is always safe; the queue exists (see
        // `ensure_queue_and_thread_id`).
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn start(mode: Mode, fire: Fire) -> Result<Box<dyn Send>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-win".into())
        .spawn(move || run(mode, fire, ready_tx))
        .map_err(|e| Error::Other(format!("cannot start the hotkey thread: {e}")))?;
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
                "the hotkey thread exited during start-up".into(),
            ))
        }
    }
}
