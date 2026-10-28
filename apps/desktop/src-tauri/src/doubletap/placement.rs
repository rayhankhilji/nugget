//! Where the overlay goes: just below the caret, flipped above it near the bottom of the screen,
//! clamped to the monitor's work area. Pure math over physical pixels, unit-tested.
//!
//! Caret rectangles come from the accessibility APIs. On macOS they are in points (logical, top
//! left of the primary display, which is also how Tauri derives each monitor's physical origin:
//! logical origin × scale). Windows UI Automation (in a per-monitor DPI-aware process, which a
//! Tauri app is) and AT-SPI on X11 report physical pixels.

/// A rectangle in physical pixels (or logical points where stated).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
    fn right(&self) -> f64 {
        self.x + self.width
    }
    fn bottom(&self) -> f64 {
        self.y + self.height
    }
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    fn distance2(&self, x: f64, y: f64) -> f64 {
        let dx = (self.x - x).max(0.0).max(x - self.right());
        let dy = (self.y - y).max(0.0).max(y - self.bottom());
        dx * dx + dy * dy
    }
}

/// A monitor: full bounds and work area (without menu bar, dock, taskbar), physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub bounds: Area,
    pub work: Area,
    pub scale: f64,
}

/// The units the caret rectangle is reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    Logical,
    Physical,
}

/// The caret units of this OS's accessibility backend.
pub const CARET_UNITS: Units = if cfg!(target_os = "macos") {
    Units::Logical
} else {
    Units::Physical
};

/// Gap between the caret and the overlay, logical pixels.
const GAP: f64 = 6.0;

fn caret_to_physical(caret: Area, units: Units, screens: &[Screen]) -> Area {
    match units {
        Units::Physical => caret,
        Units::Logical => {
            let cx = caret.x + caret.width / 2.0;
            let cy = caret.y + caret.height / 2.0;
            let scale = screens
                .iter()
                .find(|s| {
                    let k = s.scale.max(0.1);
                    Area::new(
                        s.bounds.x / k,
                        s.bounds.y / k,
                        s.bounds.width / k,
                        s.bounds.height / k,
                    )
                    .contains(cx, cy)
                })
                .or_else(|| screens.first())
                .map_or(1.0, |s| s.scale);
            Area::new(
                caret.x * scale,
                caret.y * scale,
                caret.width * scale,
                caret.height * scale,
            )
        }
    }
}

fn screen_at(screens: &[Screen], x: f64, y: f64) -> Option<&Screen> {
    screens
        .iter()
        .find(|s| s.bounds.contains(x, y))
        .or_else(|| {
            screens.iter().min_by(|a, b| {
                a.bounds
                    .distance2(x, y)
                    .total_cmp(&b.bounds.distance2(x, y))
            })
        })
}

fn clamp_axis(pos: f64, len: f64, lo: f64, hi: f64) -> f64 {
    if len >= hi - lo {
        lo
    } else {
        pos.clamp(lo, hi - len)
    }
}

/// The overlay's top-left corner in physical pixels.
///
/// - `caret`: the caret or selection rectangle, in `units`.
/// - `screens`: every monitor.
/// - `size`: the overlay's size in logical pixels (scaled by the chosen monitor).
/// - `fallback`: a physical point to centre on when there is no caret (e.g. the mouse).
pub fn place(
    caret: Option<Area>,
    units: Units,
    screens: &[Screen],
    size: (f64, f64),
    fallback: Option<(f64, f64)>,
) -> Option<(i32, i32)> {
    let caret = caret
        .filter(|c| c.x.is_finite() && c.y.is_finite() && c.width >= 0.0 && c.height >= 0.0)
        .map(|c| caret_to_physical(c, units, screens));
    let screen = match (caret, fallback) {
        (Some(c), _) => screen_at(screens, c.x + c.width / 2.0, c.y + c.height / 2.0),
        (None, Some((x, y))) => screen_at(screens, x, y),
        (None, None) => screens.first(),
    };
    let Some(screen) = screen else {
        return caret.map(|c| (c.x.round() as i32, c.bottom().round() as i32));
    };
    let k = screen.scale.max(0.1);
    let (w, h) = (size.0 * k, size.1 * k);
    let gap = GAP * k;
    let work = screen.work;
    let (x, y) = match caret {
        Some(c) => {
            let below = c.bottom() + gap;
            let y = if below + h > work.bottom() && c.y - gap - h >= work.y {
                c.y - gap - h
            } else {
                below
            };
            (c.x, y)
        }
        None => (
            work.x + (work.width - w) / 2.0,
            work.y + work.height / 3.0 - h / 2.0,
        ),
    };
    let x = clamp_axis(x, w, work.x, work.right());
    let y = clamp_axis(y, h, work.y, work.bottom());
    Some((x.round() as i32, y.round() as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f64, y: f64, w: f64, h: f64, scale: f64) -> Screen {
        Screen {
            bounds: Area::new(x, y, w, h),
            // A 50 px menu bar / taskbar strip at the top.
            work: Area::new(x, y + 50.0, w, h - 50.0),
            scale,
        }
    }

    const SIZE: (f64, f64) = (400.0, 150.0);

    #[test]
    fn below_the_caret_physical() {
        let screens = [screen(0.0, 0.0, 1920.0, 1080.0, 1.0)];
        let caret = Area::new(300.0, 200.0, 2.0, 18.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((300, 224))
        );
    }

    #[test]
    fn logical_caret_on_a_retina_display_is_scaled() {
        let screens = [screen(0.0, 0.0, 2880.0, 1800.0, 2.0)];
        let caret = Area::new(300.0, 200.0, 2.0, 18.0);
        // (300, 218 + 6) points -> (600, 448) pixels.
        assert_eq!(
            place(Some(caret), Units::Logical, &screens, SIZE, None),
            Some((600, 448))
        );
    }

    #[test]
    fn flips_above_near_the_bottom_and_clamps_the_right_edge() {
        let screens = [screen(0.0, 0.0, 1920.0, 1080.0, 1.0)];
        let caret = Area::new(1900.0, 1000.0, 2.0, 20.0);
        // Above: 1000 - 6 - 150 = 844; right edge: 1920 - 400 = 1520.
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((1520, 844))
        );
        // Scale 1.5: overlay 600 x 225, gap 9.
        let screens = [screen(0.0, 0.0, 2880.0, 1620.0, 1.5)];
        let caret = Area::new(2870.0, 1500.0, 3.0, 30.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((2280, 1266))
        );
    }

    #[test]
    fn clamps_to_the_work_area_and_picks_the_right_monitor() {
        // Second monitor to the left, at negative coordinates, scale 2.
        let screens = [
            screen(0.0, 0.0, 1920.0, 1080.0, 1.0),
            screen(-3840.0, 0.0, 3840.0, 2160.0, 2.0),
        ];
        // A caret at the very top of the left monitor goes below the menu bar strip.
        let caret = Area::new(-3000.0, 0.0, 2.0, 20.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((-3000, 50))
        );
        // Logical caret on the scale-2 monitor: (-1500, 10) points -> (-3000, 20) px.
        let caret = Area::new(-1500.0, 10.0, 1.0, 10.0);
        assert_eq!(
            place(Some(caret), Units::Logical, &screens, SIZE, None),
            Some((-3000, 52))
        );
        // A caret off every screen lands on the nearest one, clamped inside.
        let caret = Area::new(5000.0, 500.0, 2.0, 20.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((1520, 526))
        );
    }

    #[test]
    fn without_a_caret_centres_on_the_fallback_monitor() {
        let screens = [
            screen(0.0, 0.0, 1920.0, 1080.0, 1.0),
            screen(1920.0, 0.0, 1920.0, 1080.0, 1.0),
        ];
        // Work area 1920 x 1030 from y 50: x = 1920 + 760, y = 50 + 343.3 - 75.
        assert_eq!(
            place(None, Units::Physical, &screens, SIZE, Some((2500.0, 400.0))),
            Some((2680, 318))
        );
        assert_eq!(
            place(None, Units::Physical, &screens, SIZE, None),
            Some((760, 318))
        );
        assert_eq!(place(None, Units::Physical, &[], SIZE, None), None);
        // No monitor information: just below the caret.
        let caret = Area::new(10.0, 10.0, 1.0, 10.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &[], SIZE, None),
            Some((10, 20))
        );
    }

    #[test]
    fn an_overlay_larger_than_the_screen_sticks_to_the_top_left() {
        let screens = [screen(0.0, 0.0, 300.0, 200.0, 1.0)];
        let caret = Area::new(100.0, 100.0, 1.0, 10.0);
        assert_eq!(
            place(Some(caret), Units::Physical, &screens, SIZE, None),
            Some((0, 50))
        );
    }
}
