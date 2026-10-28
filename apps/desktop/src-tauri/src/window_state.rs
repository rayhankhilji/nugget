//! Remembers the main window's position, size and maximized state across launches, in
//! `window-state.json` next to `settings.json`.
//!
//! Geometry is in physical pixels: outer position, inner size. It is tracked in memory as the
//! window moves and resizes (never while minimized; while maximized only the flag changes, so
//! un-maximizing returns to the last normal size) and written when the window closes and when
//! Nugget exits. On start it is clamped onto a connected monitor, so a window last seen on an
//! unplugged display comes back fully visible. Wayland doesn't let apps read or set their
//! position, so there only size and maximized persist.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow, Window};

/// Smallest restored size; the window's own `minWidth`/`minHeight` still apply on top.
const MIN_SIZE: (u32, u32) = (320, 240);
/// Larger than any real display: such values come from a corrupt file.
const MAX_EXTENT: u32 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// A monitor's usable area (work area: without the menu bar, dock or taskbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Saved state, or `None` for anything missing, malformed or implausible.
pub fn parse(bytes: &[u8]) -> Option<Geometry> {
    let g: Geometry = serde_json::from_slice(bytes).ok()?;
    let sane = |v: u32| (1..=MAX_EXTENT).contains(&v);
    let near = |v: i32| v.unsigned_abs() <= MAX_EXTENT;
    (sane(g.width) && sane(g.height) && near(g.x) && near(g.y)).then_some(g)
}

fn overlap(a: &Area, g: &Geometry) -> u64 {
    let span = |a0: i32, al: u32, b0: i32, bl: u32| {
        let (a0, b0) = (i64::from(a0), i64::from(b0));
        let lo = a0.max(b0);
        let hi = (a0 + i64::from(al)).min(b0 + i64::from(bl));
        u64::try_from(hi - lo).unwrap_or(0)
    };
    span(a.x, a.width, g.x, g.width) * span(a.y, a.height, g.y, g.height)
}

/// Fit `g` onto the monitor it overlaps most (the first one, centred, if it overlaps none):
/// shrink it to the monitor's area and slide it fully inside. `None` without monitors.
pub fn clamp(g: Geometry, monitors: &[Area]) -> Option<Geometry> {
    let best = monitors
        .iter()
        .map(|m| (overlap(m, &g), m))
        .max_by_key(|(o, _)| *o)?;
    let (visible, m) = (best.0 > 0, best.1);
    let width = g.width.max(MIN_SIZE.0).min(m.width);
    let height = g.height.max(MIN_SIZE.1).min(m.height);
    let fit = |pos: i32, len: u32, m0: i32, mlen: u32| -> i32 {
        let lo = i64::from(m0);
        let hi = lo + i64::from(mlen) - i64::from(len);
        let p = if visible {
            i64::from(pos).clamp(lo, hi)
        } else {
            lo + (hi - lo) / 2
        };
        i32::try_from(p).unwrap_or(m0)
    };
    Some(Geometry {
        x: fit(g.x, width, m.x, m.width),
        y: fit(g.y, height, m.y, m.height),
        width,
        height,
        maximized: g.maximized,
    })
}

pub struct WindowState {
    path: Option<PathBuf>,
    current: Mutex<Option<Geometry>>,
}

impl WindowState {
    pub fn new(config_dir: Option<&Path>) -> Self {
        let path = config_dir.map(|d| d.join("window-state.json"));
        let current = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| parse(&b));
        Self {
            path,
            current: Mutex::new(current),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Geometry>> {
        self.current.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Apply the saved geometry (clamped to today's monitors) before the window is shown.
    pub fn restore<R: Runtime>(&self, window: &WebviewWindow<R>) {
        let Some(saved) = *self.lock() else { return };
        let monitors: Vec<Area> = window
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                let a = m.work_area();
                Area {
                    x: a.position.x,
                    y: a.position.y,
                    width: a.size.width,
                    height: a.size.height,
                }
            })
            .filter(|a| a.width > 0 && a.height > 0)
            .collect();
        let Some(g) = clamp(saved, &monitors) else {
            return;
        };
        let _ = window.set_size(PhysicalSize::new(g.width, g.height));
        let _ = window.set_position(PhysicalPosition::new(g.x, g.y));
        if g.maximized {
            let _ = window.maximize();
        }
    }

    /// Record where the window is now. Called on every move and resize; cheap, no I/O.
    pub fn capture<R: Runtime>(&self, window: &Window<R>) {
        if window.is_minimized().unwrap_or(true) || !window.is_visible().unwrap_or(false) {
            return;
        }
        let maximized = window.is_maximized().unwrap_or(false);
        let mut current = self.lock();
        if maximized {
            if let Some(g) = current.as_mut() {
                g.maximized = true;
            }
            return;
        }
        let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        *current = Some(Geometry {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
            maximized: false,
        });
    }

    /// Write the last captured geometry (via a temporary file and rename).
    pub fn save(&self) {
        let (Some(path), Some(g)) = (&self.path, *self.lock()) else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&g).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %path.display(), error = %e, "couldn't save the window state");
        }
    }
}

/// The managed state, created from the app's config directory.
pub fn init<R: Runtime>(app: &tauri::App<R>) -> WindowState {
    let dir = app.path().app_config_dir().ok();
    WindowState::new(dir.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: Area = Area {
        x: 0,
        y: 25,
        width: 1440,
        height: 875,
    };
    const RIGHT: Area = Area {
        x: 1440,
        y: 0,
        width: 2560,
        height: 1440,
    };

    fn geo(x: i32, y: i32, width: u32, height: u32) -> Geometry {
        Geometry {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn parse_tolerates_garbage() {
        let g = parse(br#"{"x":10,"y":-20,"width":800,"height":600,"maximized":true}"#).unwrap();
        assert_eq!(
            (g.x, g.y, g.width, g.height, g.maximized),
            (10, -20, 800, 600, true)
        );
        // Older files without the flag.
        assert!(
            !parse(br#"{"x":1,"y":2,"width":3,"height":4}"#)
                .unwrap()
                .maximized
        );
        for bad in [
            &b""[..],
            b"{not json",
            b"null",
            b"[]",
            br#"{"x":1,"y":2}"#,
            br#"{"x":0,"y":0,"width":0,"height":600}"#,
            br#"{"x":0,"y":0,"width":800,"height":-1}"#,
            br#"{"x":0,"y":0,"width":800,"height":9999999}"#,
            br#"{"x":-2000000000,"y":0,"width":800,"height":600}"#,
            br#"{"x":"a","y":0,"width":800,"height":600}"#,
        ] {
            assert_eq!(parse(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn visible_windows_stay_put() {
        let g = geo(100, 100, 1080, 720);
        assert_eq!(clamp(g, &[LAPTOP, RIGHT]), Some(g));
        let on_right = geo(2000, 300, 1200, 800);
        assert_eq!(clamp(on_right, &[LAPTOP, RIGHT]), Some(on_right));
    }

    #[test]
    fn partly_offscreen_windows_slide_inside() {
        // Hanging off the bottom-right of the laptop and behind the menu bar.
        let g = clamp(geo(1000, 0, 1080, 720), &[LAPTOP]).unwrap();
        assert_eq!((g.x, g.y), (1440 - 1080, 25));
        // Mostly on the right monitor: moves inside that one, not the laptop.
        let g = clamp(geo(1400, -50, 1000, 700), &[LAPTOP, RIGHT]).unwrap();
        assert_eq!((g.x, g.y), (1440, 0));
    }

    #[test]
    fn windows_on_missing_monitors_are_centred_on_the_first() {
        // Last seen on the right monitor, which is gone.
        let g = clamp(geo(2500, 400, 1080, 720), &[LAPTOP]).unwrap();
        assert_eq!((g.width, g.height), (1080, 720));
        assert_eq!((g.x, g.y), ((1440 - 1080) / 2, 25 + (875 - 720) / 2));
    }

    #[test]
    fn sizes_are_bounded_by_the_monitor_and_the_minimum() {
        let mut big = geo(0, 0, 4000, 3000);
        big.maximized = true;
        let g = clamp(big, &[LAPTOP]).unwrap();
        assert_eq!((g.x, g.y, g.width, g.height), (0, 25, 1440, 875));
        assert!(g.maximized);
        let g = clamp(geo(10, 30, 20, 10), &[LAPTOP]).unwrap();
        assert_eq!((g.width, g.height), MIN_SIZE);
        assert_eq!(clamp(geo(0, 0, 800, 600), &[]), None);
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        let g = clamp(geo(i32::MAX, i32::MIN, u32::MAX, u32::MAX), &[LAPTOP]).unwrap();
        assert_eq!((g.width, g.height), (1440, 875));
        assert_eq!((g.x, g.y), (0, 25));
    }

    #[test]
    fn saves_and_reloads() {
        let dir = std::env::temp_dir().join(format!("nugget-window-state-{}", std::process::id()));
        let state = WindowState::new(Some(&dir));
        assert_eq!(*state.lock(), None);
        state.save(); // Nothing captured: nothing written.
        assert!(!dir.join("window-state.json").exists());
        let g = geo(5, 6, 700, 500);
        *state.lock() = Some(g);
        state.save();
        assert_eq!(*WindowState::new(Some(&dir)).lock(), Some(g));
        std::fs::write(dir.join("window-state.json"), b"\x00garbage").unwrap();
        assert_eq!(*WindowState::new(Some(&dir)).lock(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
