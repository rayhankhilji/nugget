//! Focus change notifications from the OS, so the read-ahead loop can sleep instead of polling.
//!
//! - macOS: `NSWorkspaceDidActivateApplicationNotification` (another app came to the front).
//!   Window and tab switches inside an app are not reported; the loop's slow fallback poll
//!   notices those.
//! - Windows: WinEvents `EVENT_SYSTEM_FOREGROUND` (any window came to the front) and
//!   `EVENT_OBJECT_NAMECHANGE` of the foreground window itself (its title changed: a tab
//!   switch), out of context, on a thread of their own.
//! - Linux: none (AT-SPI event subscription is not used); [`FocusEvents::start`] returns `None`
//!   and the loop polls adaptively.
//!
//! The callbacks only wake a [`Notify`] (which keeps at most one pending wake-up, so a burst of
//! changes is one wake-up, and never blocks the OS callback). Started once, on the first loop
//! start, and kept for the app's lifetime: while the loop is stopped nobody listens and the
//! wake-ups are dropped.

use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::Notify;

/// Wakes on each focus change (coalesced).
#[derive(Debug, Clone)]
pub struct FocusEvents {
    notify: Arc<Notify>,
}

impl FocusEvents {
    /// Start listening, or `None` where there is no event source (Linux) or it failed.
    pub fn start(app: &AppHandle) -> Option<Self> {
        let notify = Arc::new(Notify::new());
        let started = platform::start(app, notify.clone());
        tracing::debug!(started, "read-ahead focus events");
        started.then_some(Self { notify })
    }

    /// Resolves at the next focus change, or at once when one happened since the last call.
    pub async fn changed(&self) {
        self.notify.notified().await;
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidActivateApplicationNotification};
    use objc2_foundation::NSNotification;
    use tauri::AppHandle;
    use tokio::sync::Notify;

    pub(super) fn start(app: &AppHandle, notify: Arc<Notify>) -> bool {
        // Registered from the main thread; with no queue the block runs on the thread that
        // posts, which for workspace notifications is the main thread (Tauri runs its loop).
        app.run_on_main_thread(move || observe(notify))
            .map_err(|e| tracing::debug!(error = %e, "cannot observe app activation"))
            .is_ok()
    }

    #[allow(unsafe_code)]
    fn observe(notify: Arc<Notify>) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| notify.notify_one());
        // SAFETY: an immutable AppKit constant, valid for the program's lifetime.
        let name = unsafe { NSWorkspaceDidActivateApplicationNotification };
        // SAFETY: no object filter and no queue (the block runs on the posting thread); the
        // block only wakes a `Notify`, which is Send and Sync, and the center copies it.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        // Observed for the app's lifetime: the token is never removed.
        std::mem::forget(token);
    }
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::ptr::null_mut;
    use std::sync::{Arc, mpsc};

    use tauri::AppHandle;
    use tokio::sync::Notify;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CHILDID_SELF, DispatchMessageW, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_FOREGROUND,
        GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, MSG, OBJID_WINDOW,
        PostThreadMessageW, TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
        WM_APP,
    };

    /// Posted to the hook thread when the foreground window changed: follow its title.
    const REHOOK: u32 = WM_APP + 1;

    thread_local! {
        static NOTIFY: RefCell<Option<Arc<Notify>>> = const { RefCell::new(None) };
    }

    pub(super) fn start(_app: &AppHandle, notify: Arc<Notify>) -> bool {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("nugget-focus-events".into())
            .spawn(move || run(notify, tx));
        spawned.is_ok() && rx.recv().unwrap_or(false)
    }

    #[allow(unsafe_code)]
    unsafe extern "system" fn on_event(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        object: i32,
        child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if event == EVENT_OBJECT_NAMECHANGE {
            // Only the foreground window's own title, not its controls'.
            // SAFETY: no preconditions.
            let front = unsafe { GetForegroundWindow() };
            if object != OBJID_WINDOW || child != CHILDID_SELF as i32 || hwnd != front {
                return;
            }
        } else if event == EVENT_SYSTEM_FOREGROUND {
            // SAFETY: posting a message without pointers to this thread's own queue.
            unsafe { PostThreadMessageW(GetCurrentThreadId(), REHOOK, 0, 0) };
        }
        NOTIFY.with_borrow(|n| {
            if let Some(n) = n {
                n.notify_one();
            }
        });
    }

    /// The hook thread: hooks foreground changes, follows the foreground window's title, and
    /// pumps messages (out-of-context WinEvents are delivered through this thread's queue).
    #[allow(unsafe_code)]
    fn run(notify: Arc<Notify>, ready: mpsc::Sender<bool>) {
        NOTIFY.set(Some(notify));
        // SAFETY: `on_event` matches WINEVENTPROC and lives for the whole program; out of
        // context, so no module handle; every process and thread. Never unhooked (the thread
        // runs for the app's lifetime).
        let foreground = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                null_mut(),
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        let _ = ready.send(!foreground.is_null());
        if foreground.is_null() {
            return;
        }
        let mut names: HWINEVENTHOOK = null_mut();
        rehook(&mut names);
        // SAFETY: MSG is plain data, and an all-zero MSG is a valid out-parameter.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: msg is a valid out-parameter; a null window means "any message of this
            // thread".
            let r = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
            if r == 0 || r == -1 {
                break;
            }
            if msg.message == REHOOK && msg.hwnd.is_null() {
                rehook(&mut names);
                continue;
            }
            // SAFETY: msg was filled in by GetMessageW.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Hook title changes of the foreground window's thread only (hooking every process's name
    /// changes would wake this thread for every label update on the system).
    #[allow(unsafe_code)]
    fn rehook(names: &mut HWINEVENTHOOK) {
        if !names.is_null() {
            // SAFETY: the handle came from SetWinEventHook on this thread and is unhooked once.
            unsafe { UnhookWinEvent(*names) };
            *names = null_mut();
        }
        // SAFETY: no preconditions.
        let front = unsafe { GetForegroundWindow() };
        if front.is_null() {
            return;
        }
        let mut pid = 0u32;
        // SAFETY: `front` is a window handle (a stale one only makes this return 0) and `pid` a
        // valid out-parameter.
        let tid = unsafe { GetWindowThreadProcessId(front, &mut pid) };
        if tid == 0 {
            return;
        }
        // SAFETY: as for the foreground hook, limited to that process and thread; Nugget's own
        // windows are skipped.
        *names = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_NAMECHANGE,
                EVENT_OBJECT_NAMECHANGE,
                null_mut(),
                Some(on_event),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::sync::Arc;

    use tauri::AppHandle;
    use tokio::sync::Notify;

    /// No event source: the loop polls.
    pub(super) fn start(_app: &AppHandle, _notify: Arc<Notify>) -> bool {
        false
    }
}
