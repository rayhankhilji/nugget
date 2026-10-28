//! Renders digit codes into a PNG with a built-in 5×7 bitmap font (no font files), and reads
//! them back by template matching (used to verify the renderer and by mock servers in tests).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Cursor;

use nugget_core::{Error, Result};

const GLYPH_W: u32 = 5;
const GLYPH_H: u32 = 7;
/// Pixels per font dot.
const SCALE: u32 = 12;
/// Gap between glyphs, in font dots.
const GAP: u32 = 2;
/// Border around the text, in font dots.
const MARGIN: u32 = 4;

/// Rows of each digit, most significant of the low 5 bits = leftmost dot.
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ], // 0
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ], // 1
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ], // 2
    [
        0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
    ], // 3
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ], // 4
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ], // 5
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ], // 6
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ], // 7
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ], // 8
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ], // 9
];

/// A random 4-digit code (1000–9999, so no leading zero to drop).
pub fn random_code() -> String {
    let mut h = RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    h.write_u128(nanos);
    format!("{}", 1000 + h.finish() % 9000)
}

fn dims(n: u32) -> (u32, u32) {
    let w = (2 * MARGIN + n * GLYPH_W + n.saturating_sub(1) * GAP) * SCALE;
    let h = (2 * MARGIN + GLYPH_H) * SCALE;
    (w, h)
}

fn glyph_origin(i: u32) -> (u32, u32) {
    ((MARGIN + i * (GLYPH_W + GAP)) * SCALE, MARGIN * SCALE)
}

/// Render `code` (ASCII digits only) as black-on-white 8-bit grayscale PNG bytes.
pub fn render_code_png(code: &str) -> Result<Vec<u8>> {
    if code.is_empty() || code.len() > 32 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid("code must be 1-32 ASCII digits".into()));
    }
    let n = code.len() as u32;
    let (w, h) = dims(n);
    let mut px = vec![255u8; (w * h) as usize];
    for (i, b) in code.bytes().enumerate() {
        let glyph = &DIGITS[(b - b'0') as usize];
        let (ox, oy) = glyph_origin(i as u32);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    let y = oy + row as u32 * SCALE + dy;
                    let start = (y * w + ox + col * SCALE) as usize;
                    px[start..start + SCALE as usize].fill(0);
                }
            }
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| Error::Other(format!("png header: {e}")))?;
        writer
            .write_image_data(&px)
            .map_err(|e| Error::Other(format!("png data: {e}")))?;
        writer
            .finish()
            .map_err(|e| Error::Other(format!("png finish: {e}")))?;
    }
    Ok(out)
}

/// Read back a code rendered by [`render_code_png`] with `digits` digits. `None` if the image is
/// not in that format.
pub fn read_code_png(png_bytes: &[u8], digits: usize) -> Option<String> {
    let decoder = png::Decoder::new(Cursor::new(png_bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Grayscale || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let n = digits as u32;
    if (info.width, info.height) != dims(n) {
        return None;
    }
    let w = info.width;
    let dark = |x: u32, y: u32| buf[(y * w + x) as usize] < 128;
    let mut code = String::with_capacity(digits);
    for i in 0..n {
        let (ox, oy) = glyph_origin(i);
        let mut rows = [0u8; 7];
        for (row, bits) in rows.iter_mut().enumerate() {
            for col in 0..GLYPH_W {
                // Sample the centre of each font dot.
                let x = ox + col * SCALE + SCALE / 2;
                let y = oy + row as u32 * SCALE + SCALE / 2;
                if dark(x, y) {
                    *bits |= 1 << (GLYPH_W - 1 - col);
                }
            }
        }
        let d = DIGITS.iter().position(|g| *g == rows)?;
        code.push(char::from(b'0' + d as u8));
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_distinct() {
        for (a, ga) in DIGITS.iter().enumerate() {
            for (b, gb) in DIGITS.iter().enumerate().skip(a + 1) {
                assert_ne!(ga, gb, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn round_trip_all_digits() {
        for code in ["0123", "4567", "8901", "9999", "1000"] {
            let png = render_code_png(code).unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!(read_code_png(&png, 4).as_deref(), Some(code));
        }
        let png = render_code_png("0123456789").unwrap();
        assert_eq!(read_code_png(&png, 10).as_deref(), Some("0123456789"));
        assert_eq!(read_code_png(&png, 4), None);
    }

    #[test]
    fn random_codes_are_four_digits() {
        for _ in 0..50 {
            let c = random_code();
            assert_eq!(c.len(), 4);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
            assert_ne!(c.as_bytes()[0], b'0');
        }
    }

    #[test]
    fn rejects_non_digits() {
        assert!(render_code_png("12a4").is_err());
        assert!(render_code_png("").is_err());
        assert_eq!(read_code_png(b"not a png", 4), None);
    }
}
