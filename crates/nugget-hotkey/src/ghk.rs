//! Chords through the `global-hotkey` crate (Carbon on macOS, `RegisterHotKey` on Windows,
//! `XGrabKey` on X11).
//!
//! `global-hotkey` reports every chord through one process-wide event handler, so this module
//! installs that handler once and routes each event by hot-key id to its listener. The app must
//! not install its own `global-hotkey` handler (for example through Tauri's global-shortcut
//! plugin): only the first handler set in a process receives events.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex, Once};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use nugget_core::{Error, Result};

use crate::chord::Chord;
use crate::engine::Fire;

static ROUTES: LazyLock<Mutex<HashMap<u32, Fire>>> = LazyLock::new(Default::default);

fn install_handler() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            if event.state != HotKeyState::Pressed {
                return;
            }
            if let Ok(routes) = ROUTES.lock()
                && let Some(fire) = routes.get(&event.id)
            {
                fire.fire();
            }
        }));
    });
}

pub(crate) fn hotkey(chord: &Chord) -> Result<HotKey> {
    HotKey::from_str(&chord.global_hotkey_string())
        .map_err(|e| Error::Invalid(format!("chord {chord}: {e}")))
}

/// A registered route; dropping it stops delivery to this listener.
pub(crate) struct Route {
    id: u32,
}

impl Drop for Route {
    fn drop(&mut self) {
        if let Ok(mut routes) = ROUTES.lock() {
            routes.remove(&self.id);
        }
    }
}

pub(crate) fn add_route(hotkey: &HotKey, chord: &Chord, fire: Fire) -> Result<Route> {
    install_handler();
    let mut routes = ROUTES
        .lock()
        .map_err(|_| Error::Other("hotkey routes poisoned".into()))?;
    if routes.contains_key(&hotkey.id()) {
        return Err(Error::Invalid(format!(
            "chord {chord} is already used by another Nugget listener"
        )));
    }
    routes.insert(hotkey.id(), fire);
    Ok(Route { id: hotkey.id() })
}

pub(crate) fn os_error(chord: &Chord, e: global_hotkey::Error) -> Error {
    Error::Other(format!(
        "cannot register chord {chord}: {e} (another app may already use it)"
    ))
}

/// X11: one process-wide manager (it runs its own X connection and thread).
#[cfg(target_os = "linux")]
pub(crate) fn start_chord(chord: Chord, fire: Fire) -> Result<Box<dyn Send>> {
    use global_hotkey::GlobalHotKeyManager;
    use std::sync::{Arc, OnceLock};

    static MANAGER: OnceLock<std::result::Result<Arc<GlobalHotKeyManager>, String>> =
        OnceLock::new();

    struct Registration {
        manager: Arc<GlobalHotKeyManager>,
        hotkey: HotKey,
        _route: Route,
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            let _ = self.manager.unregister(self.hotkey);
        }
    }

    let hotkey = hotkey(&chord)?;
    let manager = MANAGER
        .get_or_init(|| {
            GlobalHotKeyManager::new()
                .map(Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| Error::Other(format!("cannot start the X11 hot-key manager: {e}")))?;
    let route = add_route(&hotkey, &chord, fire)?;
    manager.register(hotkey).map_err(|e| os_error(&chord, e))?;
    Ok(Box::new(Registration {
        manager,
        hotkey,
        _route: route,
    }))
}

/// macOS: one process-wide manager, created and used on the main thread (Carbon hot keys are
/// delivered through the main run loop).
#[cfg(target_os = "macos")]
pub(crate) fn start_chord(chord: Chord, fire: Fire) -> Result<Box<dyn Send>> {
    use global_hotkey::GlobalHotKeyManager;
    use std::sync::OnceLock;

    // `GlobalHotKeyManager` is `Send + Sync` on macOS; we still only touch it on the main thread.
    static MANAGER: OnceLock<std::result::Result<GlobalHotKeyManager, String>> = OnceLock::new();

    fn manager() -> Result<&'static GlobalHotKeyManager> {
        MANAGER
            .get_or_init(|| GlobalHotKeyManager::new().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error::Other(format!("cannot start the macOS hot-key manager: {e}")))
    }

    struct Registration {
        hotkey: HotKey,
        _route: Route,
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            let hotkey = self.hotkey;
            crate::macos::on_main(move |_| {
                if let Ok(m) = manager() {
                    let _ = m.unregister(hotkey);
                }
            });
        }
    }

    let hotkey = hotkey(&chord)?;
    let route = add_route(&hotkey, &chord, fire)?;
    crate::macos::run_on_main("chord registration", move |_| {
        manager()?.register(hotkey).map_err(|e| os_error(&chord, e))
    })?;
    Ok(Box::new(Registration {
        hotkey,
        _route: route,
    }))
}
