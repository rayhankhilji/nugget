//! The kill switch: one process-wide flag every executor checks before each step.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type Hook = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Default)]
struct Inner {
    halted: AtomicBool,
    next_hook: AtomicU64,
    hooks: Mutex<Vec<(u64, Hook)>>,
}

/// A cheap, cloneable handle to a halt flag. [`KillSwitch::global`] is the process-wide one;
/// [`KillSwitch::new`] makes an independent one (tests, sandboxes).
///
/// Checking is a single atomic load. Engaging runs the registered hooks (the confirmation broker
/// uses one to deny everything pending; the engine uses one to log the halt).
#[derive(Clone, Default)]
pub struct KillSwitch {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for KillSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KillSwitch")
            .field("halted", &self.halted())
            .finish()
    }
}

/// Unregisters a hook when dropped.
pub struct HookGuard {
    inner: std::sync::Weak<Inner>,
    id: u64,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade()
            && let Ok(mut hooks) = inner.hooks.lock()
        {
            hooks.retain(|(id, _)| *id != self.id);
        }
    }
}

impl KillSwitch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide kill switch.
    pub fn global() -> &'static KillSwitch {
        static GLOBAL: OnceLock<KillSwitch> = OnceLock::new();
        GLOBAL.get_or_init(KillSwitch::new)
    }

    #[inline]
    pub fn halted(&self) -> bool {
        self.inner.halted.load(Ordering::SeqCst)
    }

    /// Engage. Returns `true` if this call changed the state.
    pub fn halt(&self) -> bool {
        let changed = !self.inner.halted.swap(true, Ordering::SeqCst);
        if changed {
            self.notify(true);
        }
        changed
    }

    /// Disengage. Returns `true` if this call changed the state.
    pub fn resume(&self) -> bool {
        let changed = self.inner.halted.swap(false, Ordering::SeqCst);
        if changed {
            self.notify(false);
        }
        changed
    }

    /// Run `hook(halted)` on every state change, on the thread that changed it. Keep the guard
    /// alive as long as the hook should stay registered.
    #[must_use = "the hook is unregistered when the guard is dropped"]
    pub fn on_change(&self, hook: impl Fn(bool) + Send + Sync + 'static) -> HookGuard {
        let id = self.inner.next_hook.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut hooks) = self.inner.hooks.lock() {
            hooks.push((id, Arc::new(hook)));
        }
        HookGuard {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }

    /// A callback for the hotkey crate's kill trigger:
    /// `nugget_hotkey::start(kill_trigger, kill_switch.trigger_hook())`. It only ever halts;
    /// resuming is a deliberate action in the app.
    pub fn trigger_hook(&self) -> Box<dyn Fn() + Send + Sync> {
        let this = self.clone();
        Box::new(move || {
            this.halt();
        })
    }

    fn notify(&self, halted: bool) {
        // Clone the hooks out so a hook may register or drop hooks without deadlocking.
        let hooks: Vec<Hook> = match self.inner.hooks.lock() {
            Ok(h) => h.iter().map(|(_, h)| Arc::clone(h)).collect(),
            Err(_) => return,
        };
        for hook in hooks {
            hook(halted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn halt_and_resume() {
        let k = KillSwitch::new();
        assert!(!k.halted());
        assert!(k.halt());
        assert!(!k.halt());
        assert!(k.halted());
        assert!(k.resume());
        assert!(!k.halted());
    }

    #[test]
    fn hooks_fire_and_unregister() {
        let k = KillSwitch::new();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = Arc::clone(&n);
        let guard = k.on_change(move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        k.halt();
        k.resume();
        assert_eq!(n.load(Ordering::SeqCst), 2);
        drop(guard);
        k.halt();
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn trigger_hook_halts() {
        let k = KillSwitch::new();
        let hook = k.trigger_hook();
        hook();
        assert!(k.halted());
        hook();
        assert!(k.halted());
    }
}
