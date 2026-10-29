//! A test probe for the native overlay runs (`apps/desktop/e2e-native`), in debug builds only.
//!
//! With `NUGGET_E2E_PROBE=<file>` set, the Double Tap flow appends one JSON line per step to
//! that file: the listener starting, the trigger firing, each overlay view it emits, and the
//! overlay window being shown or hidden (with its position, size and visibility as the window
//! system reports them). The driver reads it for its timings and to know which phase the overlay
//! is in; what is on screen is checked through the OS window list and a screenshot.
//!
//! Release builds compile this to nothing: no environment variable is read and nothing is
//! written. Never contains more than the overlay already shows.

#[cfg(debug_assertions)]
pub(crate) fn record(event: &str, detail: impl FnOnce() -> serde_json::Value) {
    use std::io::Write as _;
    use std::sync::OnceLock;

    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var_os("NUGGET_E2E_PROBE").map(Into::into))
    else {
        return;
    };
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or_default();
    let line = serde_json::json!({ "t": t, "event": event, "detail": detail() });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(crate) fn record(_event: &str, _detail: impl FnOnce() -> serde_json::Value) {}

/// Record the overlay window's state as the window system reports it.
pub(crate) fn window(event: &str, win: &tauri::WebviewWindow) {
    if cfg!(debug_assertions) {
        let pos = win.outer_position().ok().map(|p| (p.x, p.y));
        let size = win.outer_size().ok().map(|s| (s.width, s.height));
        record(event, || {
            serde_json::json!({
                "visible": win.is_visible().ok(),
                "focused": win.is_focused().ok(),
                "position": pos,
                "size": size,
                "scale": win.scale_factor().ok(),
            })
        });
    }
}
