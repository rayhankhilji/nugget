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
    let mut line = serde_json::json!({ "t": t, "event": event, "detail": detail() }).to_string();
    line.push('\n');
    // One write of the whole line under a lock: `writeln!` on a `File` issues several writes, and
    // the read-ahead thread records alongside the Double Tap flow, so lines interleaved and the
    // driver read broken JSON (Real OS, macOS overlay run 41115883351).
    static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
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
