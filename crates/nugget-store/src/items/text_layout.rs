//! Item text stored as a layout over the item's chunks, so chunked text is not stored twice.
//!
//! A layout is a compact recipe that rebuilds an item's text byte for byte: a sequence of
//! literal runs (bytes not covered by any chunk, typically the whitespace between paragraphs)
//! and references to byte ranges of the item's chunks (by `ord`). Chunks may overlap and may
//! carry a heading-path prefix line before their body (see `nugget-extract`'s chunker); only
//! the part of each chunk that matches the text and is not yet covered is referenced.
//!
//! Layouts live in the `item_text` table (schema v6) with `items.text` set to NULL. A layout
//! is only written after decoding it back and comparing with the original text, so a stored
//! layout always reproduces the exact text as long as the chunks it references are unchanged;
//! `items.rs` re-plans the layout whenever an item's chunks are rewritten.
//!
//! Encoding (all integers unsigned LEB128): `FORMAT`, total text length, then ops:
//! `OP_LITERAL len bytes` or `OP_CHUNK ord from to` (a byte range of chunk `ord`'s text).

use nugget_core::{Error, Result};

const FORMAT: u8 = 1;
const OP_LITERAL: u8 = 0;
const OP_CHUNK: u8 = 1;

/// How far past the expected position a chunk body is searched for. Bounds planning cost when
/// a chunk does not occur in the text (e.g. normalised by the caller): such chunks are skipped
/// and their bytes are stored as literals instead.
const SEARCH_SLACK: usize = 16 * 1024;

/// A layout is only worth storing when it saves at least this many bytes over the plain text
/// (it costs a row in `item_text` and a chunk read when the text is fetched).
const MIN_SAVING: usize = 64;

/// A layout for `text` over `chunks` (chunk `i` has `ord = i`), or `None` when the chunks do
/// not cover enough of the text to save space. The result is verified to decode to `text`.
pub(super) fn plan(text: &str, chunks: &[&str]) -> Option<Vec<u8>> {
    let mut out = header(text.len());
    let mut cursor = 0; // bytes of `text` emitted so far
    let mut from = 0; // where the previous located chunk body started
    let mut referenced = false;
    for (ord, chunk) in chunks.iter().enumerate() {
        let Some((body, start)) = locate(text, chunk, from, cursor) else {
            continue;
        };
        from = start;
        let end = start + (chunk.len() - body);
        if end <= cursor {
            continue; // fully inside text already emitted (overlap, duplicate chunk)
        }
        let take = if start > cursor {
            push_literal(&mut out, &text.as_bytes()[cursor..start]);
            body
        } else {
            body + (cursor - start)
        };
        out.push(OP_CHUNK);
        push_varint(&mut out, ord as u64);
        push_varint(&mut out, take as u64);
        push_varint(&mut out, chunk.len() as u64);
        cursor = end;
        referenced = true;
    }
    if !referenced {
        return None;
    }
    if cursor < text.len() {
        push_literal(&mut out, &text.as_bytes()[cursor..]);
    }
    if out.len() + MIN_SAVING > text.len() {
        return None;
    }
    let decoded = decode(&out, |ord| {
        usize::try_from(ord)
            .ok()
            .and_then(|i| chunks.get(i))
            .copied()
    });
    match decoded {
        Ok(d) if d == text => Some(out),
        _ => {
            debug_assert!(false, "text layout failed to round-trip");
            None
        }
    }
}

/// A layout holding `text` as one literal run (no chunk references).
pub(super) fn literal(text: &str) -> Vec<u8> {
    let mut out = header(text.len());
    if !text.is_empty() {
        push_literal(&mut out, text.as_bytes());
    }
    out
}

/// Rebuild the text from `layout`, looking chunk texts up by `ord`.
pub(super) fn decode<'a>(layout: &[u8], chunk: impl Fn(u64) -> Option<&'a str>) -> Result<String> {
    let bad = |what: &str| Error::Integrity(format!("stored item text layout: {what}"));
    let mut r = Reader {
        buf: layout,
        pos: 0,
    };
    if r.byte() != Some(FORMAT) {
        return Err(bad("unknown format"));
    }
    let len = r.varint().ok_or_else(|| bad("truncated header"))?;
    let len = usize::try_from(len).map_err(|_| bad("length out of range"))?;
    // Never trust the header for a large allocation; it is only checked at the end.
    let mut text = Vec::with_capacity(len.min(layout.len().saturating_mul(64)));
    while let Some(op) = r.byte() {
        match op {
            OP_LITERAL => {
                let n = r.varint().ok_or_else(|| bad("truncated literal"))?;
                let bytes = usize::try_from(n)
                    .ok()
                    .and_then(|n| r.take(n))
                    .ok_or_else(|| bad("truncated literal"))?;
                text.extend_from_slice(bytes);
            }
            OP_CHUNK => {
                let (Some(ord), Some(a), Some(b)) = (r.varint(), r.varint(), r.varint()) else {
                    return Err(bad("truncated chunk reference"));
                };
                let src = chunk(ord).ok_or_else(|| bad(&format!("chunk {ord} is missing")))?;
                let range = usize::try_from(a).ok().zip(usize::try_from(b).ok());
                let piece = range
                    .and_then(|(a, b)| src.get(a..b))
                    .ok_or_else(|| bad(&format!("chunk {ord} has no range {a}..{b}")))?;
                text.extend_from_slice(piece.as_bytes());
            }
            _ => return Err(bad("unknown op")),
        }
    }
    if text.len() != len {
        return Err(bad("length mismatch"));
    }
    String::from_utf8(text).map_err(|_| bad("invalid UTF-8"))
}

/// Find where `chunk` (or its body after a first-line prefix) occurs in `text`, starting the
/// search at `from` (the previous chunk's start: chunks come in text order but may overlap).
/// Returns `(body offset in chunk, start in text)`.
fn locate(text: &str, chunk: &str, from: usize, cursor: usize) -> Option<(usize, usize)> {
    let prefixed = chunk.find('\n').map(|i| i + 1);
    for body in [Some(0), prefixed].into_iter().flatten() {
        let needle = &chunk[body..];
        if needle.is_empty() {
            continue;
        }
        let mut end = (cursor.max(from) + needle.len() + SEARCH_SLACK).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(i) = text.get(from..end).and_then(|h| h.find(needle)) {
            return Some((body, from + i));
        }
    }
    None
}

fn header(len: usize) -> Vec<u8> {
    let mut out = vec![FORMAT];
    push_varint(&mut out, len as u64);
    out
}

fn push_literal(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(OP_LITERAL);
    push_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(text: &str, chunks: &[&str]) -> Option<Vec<u8>> {
        let l = plan(text, chunks)?;
        let d = decode(&l, |o| chunks.get(o as usize).copied()).unwrap();
        assert_eq!(d, text);
        Some(l)
    }

    #[test]
    fn exact_overlapping_and_prefixed_chunks() {
        let para = "The quick brown fox jumps over the lazy dog. ".repeat(4);
        let text = format!("{para}\n\n  {para}\n");
        let a = &text[..para.len()];
        let b = &text[para.len() - 20..];
        let b_trim = b.trim();
        let prefixed = format!("Guide > Install\n{b_trim}");
        for chunks in [vec![a, b], vec![a, b_trim], vec![a, prefixed.as_str()]] {
            let l = roundtrip(&text, &chunks).expect("covered");
            assert!(l.len() < 32, "{}", l.len());
        }
    }

    #[test]
    fn uncovered_or_tiny_text_is_not_planned() {
        let text = "x".repeat(500);
        assert!(plan(&text, &["nowhere to be found"]).is_none());
        assert!(plan(&text, &[]).is_none());
        assert!(
            plan("short", &["short"]).is_none(),
            "saving below MIN_SAVING"
        );
    }

    #[test]
    fn literal_layout_and_corruption() {
        for t in ["", "héllo wörld"] {
            assert_eq!(decode(&literal(t), |_| None).unwrap(), t);
        }
        let text = "αβγ ".repeat(40);
        let l = roundtrip(&text, &[&text]).unwrap();
        assert!(decode(&l, |_| None).is_err(), "missing chunk");
        assert!(
            decode(&l, |_| Some("short")).is_err(),
            "range outside chunk"
        );
        assert!(decode(&l[..l.len() - 1], |_| Some(text.as_str())).is_err());
        assert!(decode(&[9], |_| None).is_err());
    }
}
