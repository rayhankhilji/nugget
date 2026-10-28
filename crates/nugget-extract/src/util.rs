//! Helpers shared by the extractors: text decoding, a size-capped text builder, heading-section
//! tracking and small string utilities.

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};

use crate::Section;

/// Decode bytes to text: BOM first (UTF-8, UTF-16 LE/BE), then UTF-8, then the declared
/// `charset`, then mostly-UTF-8 (lossy), then Windows-1252.
pub(crate) fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return UTF_16LE.decode_without_bom_handling(rest).0.into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return UTF_16BE.decode_without_bom_handling(rest).0.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    if let Some(enc) = charset.and_then(|c| Encoding::for_label(c.trim().as_bytes()))
        && enc != UTF_8
    {
        return enc.decode_without_bom_handling(bytes).0.into_owned();
    }
    // A few invalid bytes (e.g. a multi-byte char cut by truncation) in otherwise valid UTF-8:
    // decode lossily rather than mis-reading the whole text as a legacy encoding.
    let invalid: usize = bytes.utf8_chunks().map(|c| c.invalid().len()).sum();
    if invalid * 100 <= bytes.len() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    WINDOWS_1252
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

/// Heuristic binary check: NUL bytes in the first 8 KiB (UTF-16 with a BOM is text).
pub(crate) fn looks_binary(data: &[u8]) -> bool {
    if data.starts_with(b"\xFF\xFE") || data.starts_with(b"\xFE\xFF") {
        return false;
    }
    data[..data.len().min(8192)].contains(&0)
}

/// The largest char boundary `<= i` (`str::floor_char_boundary` is not stable at our MSRV).
pub(crate) fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// At most `max` chars of `s`.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// Collapse every whitespace run to one space and trim.
pub(crate) fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Push `s` unless it is empty, already present or `v` is full.
pub(crate) fn push_unique(v: &mut Vec<String>, s: &str, cap: usize) {
    let s = s.trim();
    if s.is_empty() || v.len() >= cap || v.iter().any(|x| x == s) {
        return;
    }
    v.push(s.to_owned());
}

/// Like [`push_unique`], comparing case-insensitively (tags).
pub(crate) fn push_unique_ci(v: &mut Vec<String>, s: &str, cap: usize) {
    let s = s.trim();
    if s.is_empty() || v.len() >= cap {
        return;
    }
    let lower = s.to_lowercase();
    if v.iter().any(|x| x.to_lowercase() == lower) {
        return;
    }
    v.push(s.to_owned());
}

/// Maximum number of links any extractor collects.
pub(crate) const LINK_CAP: usize = 2_000;
/// Maximum number of tags any extractor collects.
pub(crate) const TAG_CAP: usize = 500;

/// Find bare `http(s)://` URLs in `text`; calls `f(start, url)` for each.
pub(crate) fn find_urls(text: &str, mut f: impl FnMut(usize, &str)) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find("http") {
        let start = from + rel;
        from = start + 4;
        let rest = &text[start..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            continue;
        };
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'/') {
            continue;
        }
        let base = start + scheme_len;
        let tail = &text[base..];
        let stop = tail
            .char_indices()
            .find(|&(i, c)| {
                i > 2048
                    || c.is_whitespace()
                    || matches!(c, '<' | '>' | '"' | '`' | '|' | '{' | '}' | '\\')
            })
            .map_or(tail.len(), |(i, _)| i);
        let mut url = &text[start..base + stop];
        // Trailing punctuation belongs to the prose, and so does an unbalanced closing bracket.
        while let Some(last) = url.chars().last() {
            let strip = match last {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' => true,
                ')' => url.matches('(').count() < url.matches(')').count(),
                ']' => url.matches('[').count() < url.matches(']').count(),
                _ => false,
            };
            if !strip {
                break;
            }
            url = &url[..url.len() - last.len_utf8()];
        }
        if url.len() > scheme_len {
            f(start, url);
        }
        from = start + url.len().max(4);
    }
}

/// Text accumulator with a char cap. Once full, further pushes are dropped and `truncated` is set.
pub(crate) struct TextBuf {
    pub text: String,
    chars: usize,
    max_chars: usize,
    pub truncated: bool,
}

impl TextBuf {
    pub fn new(max_chars: usize) -> Self {
        Self {
            text: String::new(),
            chars: 0,
            max_chars,
            truncated: false,
        }
    }

    pub fn is_full(&self) -> bool {
        self.truncated
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn push(&mut self, s: &str) {
        if self.truncated || s.is_empty() {
            return;
        }
        let room = self.max_chars - self.chars;
        // Fast path: byte length bounds char count.
        if s.len() <= room {
            self.chars += s.chars().count();
            self.text.push_str(s);
            return;
        }
        let n = s.chars().count();
        if n <= room {
            self.chars += n;
            self.text.push_str(s);
        } else {
            self.text.push_str(&truncate_chars(s, room));
            self.chars = self.max_chars;
            self.truncated = true;
        }
    }

    /// Drop trailing spaces and tabs on the current line.
    pub fn trim_line_end(&mut self) {
        let trimmed = self.text.trim_end_matches([' ', '\t']).len();
        let removed = self.text.len() - trimmed;
        self.text.truncate(trimmed);
        self.chars -= removed;
    }

    /// End the current line (no-op at the start of a line or of the text).
    pub fn newline(&mut self) {
        self.trim_line_end();
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.push("\n");
        }
    }

    /// Ensure a blank line separates what comes next (no-op at the start of the text).
    pub fn paragraph(&mut self) {
        self.trim_line_end();
        if self.text.is_empty() || self.text.ends_with("\n\n") {
            return;
        }
        if self.text.ends_with('\n') {
            self.push("\n");
        } else {
            self.push("\n\n");
        }
    }

    pub fn at_line_start(&self) -> bool {
        self.text.is_empty() || self.text.ends_with('\n')
    }

    /// Remove trailing whitespace from the whole text.
    pub fn finish(mut self) -> (String, bool) {
        let trimmed = self.text.trim_end().len();
        self.text.truncate(trimmed);
        (self.text, self.truncated)
    }
}

/// Builds flat, non-overlapping [`Section`]s from a stream of headings.
#[derive(Default)]
pub(crate) struct SectionTracker {
    stack: Vec<(u32, String)>,
    open: Option<(Vec<String>, usize)>,
    out: Vec<Section>,
}

/// Longest heading title kept in a heading path.
const MAX_HEADING_CHARS: usize = 200;
/// Deepest heading path kept.
const MAX_HEADING_DEPTH: usize = 12;
/// Most sections kept per document.
const MAX_SECTIONS: usize = 50_000;

impl SectionTracker {
    /// A heading of `level` (1 = outermost) titled `title` starts at byte `at`.
    pub fn heading(&mut self, level: u32, title: &str, at: usize) {
        let title = truncate_chars(&normalize_ws(title), MAX_HEADING_CHARS);
        if title.is_empty() {
            return;
        }
        self.close(at);
        while self.stack.last().is_some_and(|(l, _)| *l >= level) {
            self.stack.pop();
        }
        if self.stack.len() >= MAX_HEADING_DEPTH {
            self.stack.pop();
        }
        self.stack.push((level, title));
        let path = self.stack.iter().map(|(_, t)| t.clone()).collect();
        self.open = Some((path, at));
    }

    /// A fixed-path section (page, sheet, event) starts at `at`.
    pub fn start(&mut self, path: Vec<String>, at: usize) {
        self.close(at);
        self.stack.clear();
        self.open = Some((path, at));
    }

    /// Close the open section at `at`.
    pub fn close(&mut self, at: usize) {
        if let Some((path, start)) = self.open.take()
            && at > start
            && self.out.len() < MAX_SECTIONS
        {
            self.out.push(Section {
                heading_path: path,
                start,
                end: at,
            });
        }
    }

    pub fn finish(mut self, end: usize) -> Vec<Section> {
        self.close(end);
        self.out
    }
}

/// Human-readable message from a caught panic payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// An empty JSON object.
pub(crate) fn obj() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_boms_and_legacy() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi", None), "hi");
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0", None), "hi");
        assert_eq!(decode_text(b"\xFE\xFF\0h\0i", None), "hi");
        assert_eq!(decode_text(b"caf\xE9 cr\xE8me", None), "café crème");
        assert_eq!(decode_text(b"\xE4", Some("iso-8859-1")), "ä");
    }

    #[test]
    fn finds_urls() {
        let mut v = Vec::new();
        find_urls(
            "see https://a.example/x?y=1. and (http://b.example/p) or xhttp://no",
            |_, u| v.push(u.to_owned()),
        );
        assert_eq!(v, ["https://a.example/x?y=1", "http://b.example/p"]);
    }

    #[test]
    fn textbuf_caps() {
        let mut b = TextBuf::new(5);
        b.push("héllo world");
        assert!(b.is_full());
        assert_eq!(b.text, "héllo");
    }
}
