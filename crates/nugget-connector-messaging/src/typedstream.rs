//! Just enough of Apple's `typedstream` (NSArchiver) format to pull the plain text out of a
//! Messages `attributedBody` blob (an archived `NSAttributedString`).
//!
//! The string payload follows the `NSString` class name: a few type bytes, then `+` (0x2B)
//! introducing a C string whose length is one byte, or 0x81 + u16 LE, or 0x82 + u32 LE.

const MARKERS: &[&[u8]] = &[b"NSString", b"NSMutableString"];
/// How far after the class name the `+` may appear.
const MAX_GAP: usize = 16;

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The text of an archived attributed string, if the blob holds one.
pub fn attributed_body_text(blob: &[u8]) -> Option<String> {
    let start = MARKERS
        .iter()
        .filter_map(|m| find(blob, m).map(|i| i + m.len()))
        .min()?;
    let window = &blob[start..blob.len().min(start + MAX_GAP)];
    let plus = window.iter().position(|b| *b == b'+')?;
    let mut i = start + plus + 1;
    let tag = *blob.get(i)?;
    i += 1;
    let len = match tag {
        0x81 => {
            let b = blob.get(i..i + 2)?;
            i += 2;
            usize::from(u16::from_le_bytes([b[0], b[1]]))
        }
        0x82 => {
            let b = blob.get(i..i + 4)?;
            i += 4;
            usize::try_from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok()?
        }
        n if n < 0x80 => usize::from(n),
        _ => return None,
    };
    let bytes = blob.get(i..i.checked_add(len)?)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Build a minimal blob in the layout Messages writes (tests and fixtures).
pub fn encode_for_tests(text: &str) -> Vec<u8> {
    let mut b = b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01@\x84\x84\x84\x12NSAttributedString\x00\x84\x84\x08NSObject\x00\x85\x92\x84\x84\x84\x08NSString\x01\x94\x84\x01+".to_vec();
    let bytes = text.as_bytes();
    match bytes.len() {
        n if n < 0x80 => b.push(n as u8),
        n if n <= usize::from(u16::MAX) => {
            b.push(0x81);
            b.extend_from_slice(&(n as u16).to_le_bytes());
        }
        n => {
            b.push(0x82);
            b.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }
    b.extend_from_slice(bytes);
    b.extend_from_slice(b"\x86\x84\x02iI\x01\x05\x92\x84\x84\x84\x0cNSDictionary\x00\x94\x84\x01i\x01\x92\x84\x96\x96\x1d__kIMMessagePartAttributeName\x86");
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_medium_long() {
        for text in ["hi", "Résumé 👩‍👩‍👧", &"x".repeat(200), &"y".repeat(70_000), ""]
        {
            assert_eq!(
                attributed_body_text(&encode_for_tests(text)).as_deref(),
                Some(text)
            );
        }
    }

    #[test]
    fn garbage_is_none_not_panic() {
        assert_eq!(attributed_body_text(b""), None);
        assert_eq!(attributed_body_text(b"NSString"), None);
        assert_eq!(
            attributed_body_text(b"NSString\x01\x94\x84\x01+\x82\xff\xff"),
            None
        );
        assert_eq!(attributed_body_text(b"NSString\x01+\x10abc"), None);
        assert_eq!(attributed_body_text(b"NSString\x01+\x81\xff\xffabc"), None);
        assert_eq!(attributed_body_text(b"NSString\x01+\x90abc"), None);
        assert_eq!(
            attributed_body_text(b"NSString\x01+\x03a\xffc").as_deref(),
            Some("a\u{fffd}c")
        );
    }
}
