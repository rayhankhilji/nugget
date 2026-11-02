//! Caret geometry for ghost text: where the insertion point is on screen, how big the field's
//! text is, and its colour, read from the focused element without walking the thread.
//!
//! One trait, [`CaretTracker`], over every backend:
//!
//! | OS | Caret | Style |
//! |---|---|---|
//! | macOS | `AXSelectedTextRange` + `AXBoundsForRange` (text marker ranges for web editors) | `AXAttributedStringForRange`: `AXFont` size, `AXForegroundColor` |
//! | Windows | UIA `TextPattern.GetSelection` + `GetBoundingRectangles` (`EM_GETSEL` for plain edits) | `UIA_FontSizeAttributeId`, `UIA_ForegroundColorAttributeId` |
//! | Linux | AT-SPI `Text.GetCharacterExtents` at the caret | `Text.GetAttributeRun` `size` and `fg-color` |
//!
//! When the toolkit exposes no caret, the element's frame stands in ([`CaretSource::Frame`]):
//! the ghost text then starts at the frame's first line, which is right for an empty field and
//! approximate otherwise (the desktop shows the panel instead when it can't do better).
//!
//! Units follow [`crate::Focused::caret`]: logical points on macOS, physical pixels on Windows
//! and Linux, origin top-left of the primary display. Secure fields and denied apps report no
//! geometry and no text.

use nugget_core::Result;
use nugget_core::ipc::{AppInfo, FieldInfo};
use serde::{Deserialize, Serialize};

use crate::Rect;

/// Where a caret rectangle came from, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaretSource {
    /// The bounds of the (empty) range at the insertion point.
    Range,
    /// The bounds of a non-empty selection; the caret is put at its end.
    Selection,
    /// No text geometry: the element's frame.
    Frame,
}

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// From components in `0.0..=1.0` (AppKit, CoreGraphics). Out-of-range values are clamped.
    pub fn from_unit(r: f64, g: f64, b: f64, a: f64) -> Option<Self> {
        let c = |v: f64| {
            v.is_finite()
                .then(|| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        Some(Self {
            r: c(r)?,
            g: c(g)?,
            b: c(b)?,
            a: c(a)?,
        })
    }

    /// A UIA `ForegroundColor` attribute: a COLORREF, `0x00BBGGRR`. Negative values are the
    /// "mixed" / "not supported" sentinels.
    pub fn from_colorref(v: i32) -> Option<Self> {
        let v = u32::try_from(v).ok()?;
        if v > 0x00FF_FFFF {
            return None;
        }
        Some(Self {
            r: (v & 0xFF) as u8,
            g: ((v >> 8) & 0xFF) as u8,
            b: ((v >> 16) & 0xFF) as u8,
            a: 255,
        })
    }

    /// An AT-SPI `fg-color` text attribute: `"r,g,b"` with 8-bit (GTK, Gecko) or 16-bit (some
    /// older ATK toolkits) components.
    pub fn from_atspi(s: &str) -> Option<Self> {
        let parts: Vec<u32> = s
            .split(',')
            .map(|p| p.trim().parse::<u32>().ok())
            .collect::<Option<_>>()?;
        let [r, g, b] = parts[..] else { return None };
        let wide = r > 255 || g > 255 || b > 255;
        let scale = |v: u32| -> Option<u8> {
            if wide {
                u8::try_from((v.min(65_535) * 255 + 32_767) / 65_535).ok()
            } else {
                u8::try_from(v).ok()
            }
        };
        Some(Self {
            r: scale(r)?,
            g: scale(g)?,
            b: scale(b)?,
            a: 255,
        })
    }

    /// CoreGraphics colour components (`CGColorGetComponents`): grey and alpha (2) for a
    /// monochrome colour space, red, green, blue and alpha (4) for an RGB one. The caller checks
    /// the colour space model; any other count is `None`.
    pub fn from_cg_components(c: &[f64]) -> Option<Self> {
        match *c {
            [grey, a] => Self::from_unit(grey, grey, grey, a),
            [r, g, b, a] => Self::from_unit(r, g, b, a),
            _ => None,
        }
    }

    /// `#rrggbb` (or `#rrggbbaa` when not opaque), for the overlay's CSS.
    pub fn to_css(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// The field's text style at the caret, where the toolkit reports it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TextStyle {
    /// Font size in points (typographic points: on macOS the same as logical pixels).
    pub font_size: Option<f64>,
    pub color: Option<Rgba>,
}

impl TextStyle {
    /// Keep only plausible values (a toolkit reporting 0 or 4000 pt is not to be trusted).
    pub fn sane(self) -> Self {
        Self {
            font_size: self
                .font_size
                .filter(|s| s.is_finite() && (MIN_FONT..=MAX_FONT).contains(s)),
            color: self.color.filter(|c| c.a > 0),
        }
    }
}

impl TextStyle {
    /// From AT-SPI text attributes (`Text.GetAttributeRun` with defaults): `size` and
    /// `fg-color` (`"r,g,b"`, or a CSS `"rgb(r, g, b)"` as some web toolkits pass through).
    /// Missing or unparsable values are `None`.
    pub fn from_atspi_attrs(attrs: &std::collections::HashMap<String, String>) -> Self {
        let color = attrs.get("fg-color").and_then(|s| {
            let s = s.trim();
            let inner = s
                .strip_prefix("rgb(")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(s);
            Rgba::from_atspi(inner)
        });
        Self {
            font_size: attrs.get("size").and_then(|s| parse_atspi_size(s)),
            color,
        }
    }
}

/// An AT-SPI `size` text attribute (`"11"`, `"10.5"`, `"12pt"`) in points.
pub fn parse_atspi_size(s: &str) -> Option<f64> {
    let t = s.trim().trim_end_matches("pt").trim();
    t.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

/// Font sizes outside this range are ignored.
pub const MIN_FONT: f64 = 6.0;
pub const MAX_FONT: f64 = 96.0;
/// A line is about this many times the font size (the common default line height).
pub const LINE_HEIGHT_RATIO: f64 = 1.25;
/// Inset of the first line inside a field's frame, used by the frame fallback (points).
pub const FRAME_INSET: f64 = 6.0;
/// The first line's height assumed by the frame fallback when the frame is taller (points).
pub const FRAME_LINE: f64 = 18.0;

/// The caret, its source, the field's frame and its text style.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CaretGeometry {
    /// A thin rectangle at the insertion point, as tall as the line.
    pub caret: Rect,
    pub source: CaretSource,
    /// The field's own frame, when known.
    pub field: Option<Rect>,
    pub style: TextStyle,
}

impl CaretGeometry {
    /// From what the platform reported: the range or selection bounds (`text`, with whether it
    /// is a non-empty selection), else the field's frame. `None` when neither is usable.
    pub fn resolve(
        text: Option<Rect>,
        selection: bool,
        frame: Option<Rect>,
        style: TextStyle,
    ) -> Option<Self> {
        let frame = frame.filter(usable);
        let style = style.sane();
        if let Some(r) = text.filter(usable) {
            let (caret, source) = if selection && r.width > 1.0 {
                // The suggestion goes after the selection: its trailing edge.
                (
                    Rect {
                        x: r.x + r.width - 1.0,
                        width: 1.0,
                        ..r
                    },
                    CaretSource::Selection,
                )
            } else {
                (
                    Rect {
                        width: r.width.clamp(1.0, 2.0),
                        ..r
                    },
                    CaretSource::Range,
                )
            };
            // A caret far outside its own field is stale (scrolled away, a different element).
            if let Some(f) = frame
                && !near(&caret, &f)
            {
                return Some(Self::from_frame(f, style));
            }
            return Some(Self {
                caret,
                source,
                field: frame,
                style,
            });
        }
        frame.map(|f| Self::from_frame(f, style))
    }

    fn from_frame(f: Rect, style: TextStyle) -> Self {
        let line = style
            .font_size
            .map_or(FRAME_LINE, |s| s * LINE_HEIGHT_RATIO);
        let inset_y = ((f.height - line) / 2.0).clamp(0.0, FRAME_INSET);
        Self {
            caret: Rect {
                x: f.x + FRAME_INSET.min(f.width / 2.0),
                y: f.y + inset_y,
                width: 1.0,
                height: line.min(f.height).max(1.0),
            },
            source: CaretSource::Frame,
            field: Some(f),
            style,
        }
    }

    /// The font size to render at: the reported one, else estimated from the caret's height.
    pub fn font_size(&self) -> f64 {
        self.style
            .font_size
            .unwrap_or_else(|| estimate_font_size(self.caret.height))
    }

    /// Whether the geometry places the text precisely (not a frame guess).
    pub fn precise(&self) -> bool {
        self.source != CaretSource::Frame
    }
}

/// A font size from a line (caret) height.
pub fn estimate_font_size(line_height: f64) -> f64 {
    if !line_height.is_finite() || line_height <= 0.0 {
        return 13.0;
    }
    (line_height / LINE_HEIGHT_RATIO).clamp(MIN_FONT, MAX_FONT)
}

fn usable(r: &Rect) -> bool {
    [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite())
        && r.height > 0.0
        && r.height < 2_000.0
        && r.width >= 0.0
        // AX reports (0,0,0,0) or a point at the origin for elements it can't place.
        && !(r.x == 0.0 && r.y == 0.0 && r.width <= 1.0)
}

/// Whether the caret lies within (a line's slack of) the field's frame.
fn near(c: &Rect, f: &Rect) -> bool {
    let slack = c.height.max(8.0);
    c.x >= f.x - slack
        && c.x <= f.x + f.width + slack
        && c.y + c.height >= f.y - slack
        && c.y <= f.y + f.height + slack
}

/// The focused field without its surroundings: text, selection and caret geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct CaretRead {
    pub app: AppInfo,
    /// Role, label and placeholder; value and selection are empty for secure fields and denied
    /// apps.
    pub field: FieldInfo,
    /// `None` for secure fields and denied apps, or when neither text bounds nor a frame exist.
    pub geometry: Option<CaretGeometry>,
    /// The app is on the accessibility deny list.
    pub denied: bool,
}

/// A cheap read of the focused field for ghost text: no thread walk, so it can run on every
/// keystroke pause and while the field scrolls.
pub trait CaretTracker: Send + Sync {
    /// The focused text field, or `None` when focus is not in one.
    fn caret(&self) -> Result<Option<CaretRead>>;
}

impl CaretTracker for crate::Unsupported {
    fn caret(&self) -> Result<Option<CaretRead>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn range_bounds_win_over_the_frame() {
        let g = CaretGeometry::resolve(
            Some(r(120.0, 400.0, 0.0, 17.0)),
            false,
            Some(r(100.0, 390.0, 600.0, 40.0)),
            TextStyle::default(),
        )
        .unwrap();
        assert_eq!(g.source, CaretSource::Range);
        assert_eq!(g.caret, r(120.0, 400.0, 1.0, 17.0));
        assert!(g.precise());
        assert!((g.font_size() - 13.6).abs() < 0.01, "{}", g.font_size());
    }

    #[test]
    fn a_selection_puts_the_caret_at_its_end() {
        let g = CaretGeometry::resolve(
            Some(r(100.0, 10.0, 50.0, 16.0)),
            true,
            None,
            TextStyle::default(),
        )
        .unwrap();
        assert_eq!(g.source, CaretSource::Selection);
        assert_eq!(g.caret, r(149.0, 10.0, 1.0, 16.0));
    }

    #[test]
    fn the_frame_stands_in_without_text_bounds() {
        let style = TextStyle {
            font_size: Some(14.0),
            color: None,
        };
        let g =
            CaretGeometry::resolve(None, false, Some(r(10.0, 20.0, 300.0, 30.0)), style).unwrap();
        assert_eq!(g.source, CaretSource::Frame);
        assert!(!g.precise());
        assert_eq!(g.caret.x, 16.0);
        assert!((g.caret.height - 17.5).abs() < 1e-9);
        assert!(g.caret.y >= 20.0 && g.caret.y + g.caret.height <= 50.0);
        assert_eq!(g.font_size(), 14.0);
        // A tiny frame still gives a caret inside it.
        let g = CaretGeometry::resolve(
            None,
            false,
            Some(r(0.5, 0.0, 4.0, 6.0)),
            TextStyle::default(),
        )
        .unwrap();
        assert!(g.caret.height <= 6.0 && g.caret.x <= 2.5);
    }

    #[test]
    fn unusable_rects_are_ignored() {
        let zero = r(0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            CaretGeometry::resolve(Some(zero), false, Some(zero), TextStyle::default()),
            None
        );
        let nan = r(f64::NAN, 1.0, 1.0, 1.0);
        assert_eq!(
            CaretGeometry::resolve(Some(nan), false, None, TextStyle::default()),
            None
        );
        // A stale caret far outside its field falls back to the field.
        let g = CaretGeometry::resolve(
            Some(r(3000.0, 3000.0, 1.0, 16.0)),
            false,
            Some(r(10.0, 10.0, 200.0, 30.0)),
            TextStyle::default(),
        )
        .unwrap();
        assert_eq!(g.source, CaretSource::Frame);
    }

    #[test]
    fn styles_are_sanity_checked() {
        let s = TextStyle {
            font_size: Some(0.0),
            color: Some(Rgba {
                r: 1,
                g: 2,
                b: 3,
                a: 0,
            }),
        }
        .sane();
        assert_eq!(s, TextStyle::default());
        assert_eq!(estimate_font_size(0.0), 13.0);
        assert_eq!(estimate_font_size(400.0), MAX_FONT);
        assert_eq!(estimate_font_size(5.0), MIN_FONT);
    }

    #[test]
    fn colour_parsers() {
        assert_eq!(
            Rgba::from_colorref(0x0000_80FF),
            Some(Rgba {
                r: 0xFF,
                g: 0x80,
                b: 0,
                a: 255
            })
        );
        // UIA's "mixed attribute" / "not supported" sentinels are not colours.
        assert_eq!(Rgba::from_colorref(-1), None);
        assert_eq!(Rgba::from_colorref(0x0100_0000), None);
        assert_eq!(Rgba::from_atspi("51,51,51").unwrap().to_css(), "#333333");
        assert_eq!(
            Rgba::from_atspi("65535, 0, 32768").unwrap().to_css(),
            "#ff0080"
        );
        assert_eq!(Rgba::from_atspi("1,2"), None);
        assert_eq!(Rgba::from_atspi("a,b,c"), None);
        assert_eq!(
            Rgba::from_unit(0.0, 0.5, 1.0, 0.5).unwrap().to_css(),
            "#0080ff80"
        );
        assert_eq!(Rgba::from_unit(f64::NAN, 0.0, 0.0, 1.0), None);
        assert_eq!(parse_atspi_size("10.5"), Some(10.5));
        assert_eq!(parse_atspi_size(" 12pt"), Some(12.0));
        assert_eq!(parse_atspi_size("big"), None);
        assert_eq!(parse_atspi_size("0"), None);
    }

    /// Every recorded and modelled tree: the focused node's bounds as the frame give a caret
    /// inside it, and a caret rectangle inside the frame is taken as the range. Every fixture's
    /// focused node has bounds (asserted, so a new fixture without them is noticed and documented
    /// here).
    #[test]
    fn every_fixture_focus_resolves_inside_its_frame() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trees");
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        names.sort();
        assert!(names.len() >= 10, "fixtures: {names:?}");
        for path in names {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let snap = crate::tree::Snapshot::from_json(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let focused = snap
                .focused()
                .unwrap_or_else(|| panic!("{name}: no focused node"));
            let frame = focused
                .bounds
                .unwrap_or_else(|| panic!("{name}: the focused node has no bounds"));
            let inside = |c: &Rect| {
                c.x >= frame.x
                    && c.x + c.width <= frame.x + frame.width
                    && c.y >= frame.y
                    && c.y + c.height <= frame.y + frame.height
            };

            let g = CaretGeometry::resolve(None, false, Some(frame), TextStyle::default())
                .unwrap_or_else(|| panic!("{name}: unusable frame {frame:?}"));
            assert_eq!(g.source, CaretSource::Frame, "{name}");
            assert_eq!(g.field, Some(frame), "{name}");
            assert!(inside(&g.caret), "{name}: {:?} outside {frame:?}", g.caret);

            // A caret one line into the field, a few points in from its left edge.
            let line = 16.0_f64.min(frame.height);
            let caret = r(
                frame.x + 8.0,
                frame.y + (frame.height - line) / 2.0,
                0.0,
                line,
            );
            let g = CaretGeometry::resolve(Some(caret), false, Some(frame), TextStyle::default())
                .unwrap();
            assert_eq!(g.source, CaretSource::Range, "{name}");
            assert!(inside(&g.caret), "{name}: {:?} outside {frame:?}", g.caret);
            assert_eq!(g.caret.x, caret.x, "{name}");
        }
    }

    #[test]
    fn cg_components() {
        let grey = Rgba::from_cg_components(&[0.2, 1.0]).unwrap();
        assert_eq!(grey.to_css(), "#333333");
        let rgba = Rgba::from_cg_components(&[1.0, 0.0, 0.5, 0.5]).unwrap();
        assert_eq!(rgba.to_css(), "#ff008080");
        // Extended-range components are clamped.
        assert_eq!(
            Rgba::from_cg_components(&[1.2, -0.1, 0.0, 1.0])
                .unwrap()
                .to_css(),
            "#ff0000"
        );
        // CMYK + alpha, a bare component, and nothing are not read.
        assert_eq!(Rgba::from_cg_components(&[0.0, 0.0, 0.0, 1.0, 1.0]), None);
        assert_eq!(Rgba::from_cg_components(&[0.5]), None);
        assert_eq!(Rgba::from_cg_components(&[]), None);
        assert_eq!(Rgba::from_cg_components(&[f64::NAN, 1.0]), None);
    }

    #[test]
    fn atspi_attribute_maps() {
        let map = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let gtk = map(&[
            ("family-name", "Cantarell"),
            ("size", "11"),
            ("fg-color", "46,52,54"),
            ("weight", "400"),
        ]);
        assert_eq!(
            TextStyle::from_atspi_attrs(&gtk),
            TextStyle {
                font_size: Some(11.0),
                color: Some(Rgba {
                    r: 46,
                    g: 52,
                    b: 54,
                    a: 255
                }),
            }
        );
        // A unit on the size, CSS syntax on the colour.
        let web = map(&[("size", "10.5pt"), ("fg-color", "rgb(0, 0, 255)")]);
        let s = TextStyle::from_atspi_attrs(&web);
        assert_eq!(s.font_size, Some(10.5));
        assert_eq!(s.color.map(Rgba::to_css).as_deref(), Some("#0000ff"));
        let rgba = map(&[("fg-color", "rgba(0, 0, 0, 0.5)")]);
        assert_eq!(TextStyle::from_atspi_attrs(&rgba).color, None);
        assert_eq!(TextStyle::from_atspi_attrs(&map(&[])), TextStyle::default());
        assert_eq!(
            TextStyle::from_atspi_attrs(&map(&[("size", "huge"), ("fg-color", "1,2")])),
            TextStyle::default()
        );
    }
}
