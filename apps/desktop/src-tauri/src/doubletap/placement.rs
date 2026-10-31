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

/// Where the overlay goes and how tall it is, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// It sits above the caret (keep it there while it grows, so it doesn't jump).
    pub above: bool,
    /// The chosen monitor's scale factor.
    pub scale: f64,
}

/// What [`fit`] is asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Want {
    /// Width and wanted height, logical pixels.
    pub size: (f64, f64),
    /// The least height worth showing, logical pixels.
    pub min_height: f64,
    /// The field is right-to-left.
    pub rtl: bool,
    pub prefer_above: Option<bool>,
}

/// Place an overlay that wants `size` (logical) but may be shorter, down to `min_height`: below
/// the caret when it fits, else above it when that fits, else on the side with more room, its
/// height cut to that room so it never covers the caret line (the content scrolls instead).
/// `prefer_above` keeps the side chosen earlier in the session. Right-to-left fields align the
/// overlay's right edge with the caret. Without a caret it is centred on the monitor of the
/// `fallback` point (e.g. the mouse), a third of the way down.
pub fn fit(
    caret: Option<Area>,
    units: Units,
    screens: &[Screen],
    want: Want,
    fallback: Option<(f64, f64)>,
) -> Option<Fit> {
    let Want {
        size,
        min_height,
        rtl,
        prefer_above,
    } = want;
    let caret = caret
        .filter(|c| c.x.is_finite() && c.y.is_finite() && c.width >= 0.0 && c.height >= 0.0)
        .map(|c| caret_to_physical(c, units, screens));
    let screen = match (caret, fallback) {
        (Some(c), _) => screen_at(screens, c.x + c.width / 2.0, c.y + c.height / 2.0),
        (None, Some((x, y))) => screen_at(screens, x, y),
        (None, None) => screens.first(),
    };
    let px = |v: f64| v.round().max(0.0) as u32;
    let Some(screen) = screen else {
        return caret.map(|c| Fit {
            x: c.x.round() as i32,
            y: c.bottom().round() as i32,
            width: px(size.0),
            height: px(size.1),
            above: false,
            scale: 1.0,
        });
    };
    let k = screen.scale.max(0.1);
    let work = screen.work;
    let w = (size.0 * k).min(work.width);
    let min_h = (min_height * k).min(size.1 * k);
    let wanted = (size.1 * k).min(work.height);
    let gap = GAP * k;
    let (x, y, h, above) = match caret {
        Some(c) => {
            let below_room = work.bottom() - (c.bottom() + gap);
            let above_room = (c.y - gap) - work.y;
            let above = prefer_above.unwrap_or(
                wanted > below_room && (wanted <= above_room || above_room > below_room),
            );
            let room = if above { above_room } else { below_room };
            let h = wanted.min(room.max(min_h));
            let y = if above {
                c.y - gap - h
            } else {
                c.bottom() + gap
            };
            let x = if rtl { c.right() - w } else { c.x };
            (x, y, h, above)
        }
        None => (
            work.x + (work.width - w) / 2.0,
            work.y + work.height / 3.0 - wanted / 2.0,
            wanted,
            false,
        ),
    };
    let x = clamp_axis(x, w, work.x, work.right());
    let y = clamp_axis(y, h, work.y, work.bottom());
    Some(Fit {
        x: x.round() as i32,
        y: y.round() as i32,
        width: px(w),
        height: px(h),
        above,
        scale: k,
    })
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

    /// The fixed-size placement the original tests describe.
    fn place(
        caret: Option<Area>,
        units: Units,
        screens: &[Screen],
        size: (f64, f64),
        fallback: Option<(f64, f64)>,
    ) -> Option<(i32, i32)> {
        fit(
            caret,
            units,
            screens,
            w(size, size.1, false, None),
            fallback,
        )
        .map(|f| (f.x, f.y))
    }

    fn w(size: (f64, f64), min_height: f64, rtl: bool, prefer_above: Option<bool>) -> Want {
        Want {
            size,
            min_height,
            rtl,
            prefer_above,
        }
    }

    #[test]
    fn fit_grows_below_then_cuts_to_the_room_and_scrolls() {
        let screens = [screen(0.0, 0.0, 1920.0, 1080.0, 1.0)];
        let caret = Area::new(300.0, 200.0, 2.0, 18.0);
        let f = fit(
            Some(caret),
            Units::Physical,
            &screens,
            w((440.0, 150.0), 120.0, false, None),
            None,
        )
        .unwrap();
        assert_eq!(
            (f.x, f.y, f.width, f.height, f.above),
            (300, 224, 440, 150, false)
        );
        // Near the bottom: above when it fits there.
        let low = Area::new(300.0, 900.0, 2.0, 18.0);
        let f = fit(
            Some(low),
            Units::Physical,
            &screens,
            w((440.0, 300.0), 120.0, false, None),
            None,
        )
        .unwrap();
        assert!(f.above);
        assert_eq!(f.y, 900 - 6 - 300);
        assert!(f.y + f.height as i32 <= 900, "never over the caret line");
        // Kept below by an earlier choice: cut to the room below the caret.
        let f = fit(
            Some(low),
            Units::Physical,
            &screens,
            w((440.0, 300.0), 120.0, false, Some(false)),
            None,
        )
        .unwrap();
        assert!(!f.above);
        assert_eq!(f.y, 924);
        assert_eq!(f.height, 1080 - 924);
    }

    #[test]
    fn fit_never_covers_the_caret_when_neither_side_fits() {
        // A 400 px tall work area with the caret in the middle: 160-ish px each side.
        let screens = [Screen {
            bounds: Area::new(0.0, 0.0, 1000.0, 450.0),
            work: Area::new(0.0, 50.0, 1000.0, 400.0),
            scale: 1.0,
        }];
        let caret = Area::new(100.0, 230.0, 2.0, 20.0);
        let f = fit(
            Some(caret),
            Units::Physical,
            &screens,
            w((440.0, 420.0), 120.0, false, None),
            None,
        )
        .unwrap();
        let (top, bottom) = (f.y, f.y + f.height as i32);
        assert!(bottom <= 230 || top >= 250, "{f:?} overlaps the caret");
        assert!(f.height >= 120);
    }

    #[test]
    fn fit_scales_on_high_dpi_and_aligns_rtl_to_the_caret_end() {
        let screens = [
            screen(0.0, 0.0, 1920.0, 1080.0, 1.0),
            screen(1920.0, 0.0, 3840.0, 2160.0, 2.0),
        ];
        let caret = Area::new(3000.0, 400.0, 2.0, 36.0);
        let f = fit(
            Some(caret),
            Units::Physical,
            &screens,
            w((440.0, 200.0), 120.0, true, None),
            None,
        )
        .unwrap();
        // 2x: 880 x 400 physical, right edge at the caret's right edge.
        assert_eq!((f.width, f.height), (880, 400));
        assert_eq!(f.x, 3002 - 880);
        assert_eq!(f.y, 436 + 12);
        // Clamped inside the monitor when the caret is near its left edge.
        let edge = Area::new(1930.0, 400.0, 2.0, 36.0);
        let f = fit(
            Some(edge),
            Units::Physical,
            &screens,
            w((440.0, 200.0), 120.0, true, None),
            None,
        )
        .unwrap();
        assert_eq!(f.x, 1920);
    }

    #[test]
    fn fit_without_a_caret_centres_and_caps_to_the_work_area() {
        let screens = [screen(0.0, 0.0, 800.0, 600.0, 1.0)];
        let f = fit(
            None,
            Units::Physical,
            &screens,
            w((440.0, 900.0), 120.0, false, None),
            Some((10.0, 10.0)),
        )
        .unwrap();
        assert_eq!(f.height, 550);
        assert_eq!(f.y, 50);
        assert_eq!(f.x, 180);
        assert_eq!(
            fit(
                None,
                Units::Physical,
                &[],
                w((440.0, 200.0), 120.0, false, None),
                None
            ),
            None
        );
    }
}
