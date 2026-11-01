//! Pure text helpers shared by the OS backends: line-ending normalisation and embedded-object
//! (U+FFFC) flattening, each with a map from offsets in the text the OS reported to character
//! offsets in the text the crate reports. No OS calls, so the tests run on every platform.
//!
//! Offsets are in Unicode scalar values (`char`s) unless a function says UTF-16.
// Each backend uses the helpers its OS needs; the others are only exercised by the tests.
#![cfg_attr(not(test), allow(dead_code))]

/// The object replacement character AT-SPI and UIA put where a child object sits in the text.
pub const EMBED: char = '\u{FFFC}';

/// Text derived from a source string, with a map from source character offsets to offsets in
/// `text`. The map is monotonic and has one entry per source character plus one for the end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextMap {
    pub text: String,
    /// `map[i]`: the offset in `text` of where source character `i` landed (for a removed
    /// character, where the next kept one goes); `map[len]` is `text`'s length in chars.
    map: Vec<u32>,
}

impl TextMap {
    /// The identity map over `s`.
    pub fn identity(s: &str) -> Self {
        let n = s.chars().count() as u32;
        TextMap {
            text: s.to_string(),
            map: (0..=n).collect(),
        }
    }

    /// Characters in the source.
    pub fn source_len(&self) -> u32 {
        self.map.len().saturating_sub(1) as u32
    }

    /// Characters in `text`.
    pub fn len(&self) -> u32 {
        self.map.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Map a source character offset (clamped to the source length).
    pub fn to_text(&self, source: u32) -> u32 {
        let i = (source as usize).min(self.map.len().saturating_sub(1));
        self.map.get(i).copied().unwrap_or(0)
    }

    /// Map an offset in `text` back to the source: the last source offset that maps to it (or
    /// below it), so a caret after a `\r\n` lands after both characters.
    pub fn to_source(&self, text: u32) -> u32 {
        // Partition point of `map[i] <= text`: the first index past it, minus one.
        let n = self.map.partition_point(|&m| m <= text);
        n.saturating_sub(1) as u32
    }
}

/// Normalise line endings to `\n` (`\r\n`, a lone `\r` as RichEdit reports paragraphs, U+2028
/// and U+2029), keeping a map from source offsets. Everything else is kept as is.
pub fn normalize_newlines(raw: &str) -> TextMap {
    let mut text = String::with_capacity(raw.len());
    let mut map = Vec::with_capacity(raw.len() + 1);
    let mut n = 0u32;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        map.push(n);
        match c {
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                text.push('\n');
                n += 1;
                // The `\n` of the pair: a caret between the two sits after the line break.
                map.push(n);
            }
            '\r' | '\u{2028}' | '\u{2029}' => {
                text.push('\n');
                n += 1;
            }
            c => {
                text.push(c);
                n += 1;
            }
        }
    }
    map.push(n);
    TextMap { text, map }
}

/// UTF-16 code units to a character offset into `s` (a unit inside a surrogate pair rounds
/// up to after the character; past the end clamps to the end).
pub fn utf16_to_chars(s: &str, units: usize) -> u32 {
    let mut seen = 0usize;
    let mut chars = 0u32;
    for c in s.chars() {
        if seen >= units {
            break;
        }
        seen += c.len_utf16();
        chars += 1;
    }
    chars
}

/// A character offset into `s` to UTF-16 code units (clamped to the end).
pub fn chars_to_utf16(s: &str, chars: u32) -> usize {
    s.chars().take(chars as usize).map(char::len_utf16).sum()
}

/// The content of one embedded object, already flattened itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embedded {
    pub text: Flat,
    /// A block (paragraph, heading, list item...): separated from its neighbours by a line
    /// break, and a trailing `\n` (a placeholder `<br>`) is dropped.
    pub block: bool,
}

/// Where one embedded object's content sits in the flattened text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    /// The offset of its U+FFFC in the source.
    pub source: u32,
    pub start: u32,
    /// Characters emitted (a block's dropped trailing `\n` is not counted).
    pub len: u32,
    /// The flattened content; `None` when it could not be read (it then contributes nothing).
    pub flat: Option<Flat>,
}

/// Text with its embedded objects replaced by their content, recursively.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flat {
    pub map: TextMap,
    /// One span per U+FFFC of the source, in order.
    pub embeds: Vec<Span>,
}

impl Flat {
    /// A leaf: text without embedded objects (any U+FFFC is dropped).
    pub fn leaf(s: &str) -> Self {
        let n = s.chars().filter(|&c| c == EMBED).count();
        flatten_embedded(s, (0..n).map(|_| None).collect())
    }

    pub fn text(&self) -> &str {
        &self.map.text
    }

    /// Offset in the flattened text of an offset inside the embedded object reached by `path`
    /// (indices into `embeds` at each level), or of `offset` in this object's own text when the
    /// path is empty. `None` when the path names an object that was not read.
    pub fn offset_at(&self, path: &[usize], offset: u32) -> Option<u32> {
        let Some((&first, rest)) = path.split_first() else {
            return Some(self.map.to_text(offset));
        };
        let span = self.embeds.get(first)?;
        let inner = span.flat.as_ref()?.offset_at(rest, offset)?;
        Some(span.start + inner.min(span.len))
    }

    /// The index in `embeds` of the object whose U+FFFC is at source offset `i`.
    pub fn embed_at(&self, i: u32) -> Option<usize> {
        self.embeds.binary_search_by_key(&i, |s| s.source).ok()
    }

    /// Nesting followed by [`descend`](Self::descend).
    pub const MAX_DEPTH: usize = 16;

    /// Find the innermost object holding an offset (a caret or a selection endpoint) and the
    /// offset in its own text, for [`offset_at`](Self::offset_at).
    ///
    /// `own` is the offset this object reports, `None` when it reports none (the offset is in
    /// one of its embedded objects, or nowhere). `inner(path)` asks the embedded object at
    /// `path` for its own offset, `None` when the offset is not inside it. An object reports
    /// an offset inside a child as the child's U+FFFC (Chromium) or the offset just after it
    /// (Gecko, for the end of a selection or a caret at the end of a child), so the descent
    /// tries the embedded object at the offset first and then the one just before it. When
    /// both hold part of a selection (Chromium's end inside the second child), the one at the
    /// offset is the right one.
    pub fn descend(
        &self,
        own: Option<u32>,
        inner: &mut dyn FnMut(&[usize]) -> Option<u32>,
    ) -> Option<(Vec<usize>, u32)> {
        let mut path = Vec::new();
        let mut flat = self;
        let mut offset = match own {
            Some(o) => o,
            None => {
                // Reported by an embedded object only: find which.
                let (k, o) = (0..self.embeds.len())
                    .filter(|&k| self.embeds[k].flat.is_some())
                    .find_map(|k| inner(&[k]).map(|o| (k, o)))?;
                path.push(k);
                flat = self.embeds[k].flat.as_ref()?;
                o
            }
        };
        'down: while path.len() < Self::MAX_DEPTH {
            for i in [Some(offset), offset.checked_sub(1)].into_iter().flatten() {
                let Some(k) = flat.embed_at(i) else { continue };
                let Some(child) = flat.embeds[k].flat.as_ref() else {
                    continue;
                };
                path.push(k);
                if let Some(o) = inner(&path) {
                    flat = child;
                    offset = o;
                    continue 'down;
                }
                path.pop();
            }
            break;
        }
        Some((path, offset))
    }

    /// [`descend`](Self::descend), then the offset in the flattened text.
    pub fn resolve(
        &self,
        own: Option<u32>,
        inner: &mut dyn FnMut(&[usize]) -> Option<u32>,
    ) -> Option<u32> {
        let (path, offset) = self.descend(own, inner)?;
        self.offset_at(&path, offset)
    }
}

/// Replace each U+FFFC of `source` with the content of the matching embedded object (in
/// order). Blocks start on a line of their own and are separated from what follows by one
/// `\n`, so paragraphs come out joined by `\n` and an empty paragraph is an empty line.
pub fn flatten_embedded(source: &str, embeds: Vec<Option<Embedded>>) -> Flat {
    let mut text = String::with_capacity(source.len());
    let mut map = Vec::with_capacity(source.len() + 1);
    let mut out_embeds = Vec::with_capacity(embeds.len());
    let mut embeds = embeds.into_iter();
    let mut n = 0u32;
    let mut at_line_start = true;
    let mut after_block = false;
    let push = |text: &mut String, n: &mut u32, c: char| {
        text.push(c);
        *n += 1;
    };
    for (i, c) in source.chars().enumerate() {
        if c == EMBED {
            match embeds.next().flatten() {
                Some(e) if e.block => {
                    if after_block || !at_line_start {
                        push(&mut text, &mut n, '\n');
                    }
                    map.push(n);
                    let start = n;
                    let body = e.text.text().strip_suffix('\n').unwrap_or(e.text.text());
                    for ch in body.chars() {
                        push(&mut text, &mut n, ch);
                    }
                    out_embeds.push(Span {
                        start,
                        len: n - start,
                        source: i as u32,
                        flat: Some(e.text),
                    });
                    after_block = true;
                    at_line_start = false;
                }
                Some(e) => {
                    if after_block {
                        push(&mut text, &mut n, '\n');
                        after_block = false;
                        at_line_start = true;
                    }
                    map.push(n);
                    let start = n;
                    for ch in e.text.text().chars() {
                        push(&mut text, &mut n, ch);
                    }
                    if let Some(last) = e.text.text().chars().last() {
                        at_line_start = last == '\n';
                    }
                    out_embeds.push(Span {
                        start,
                        len: n - start,
                        source: i as u32,
                        flat: Some(e.text),
                    });
                }
                None => {
                    map.push(n);
                    out_embeds.push(Span {
                        start: n,
                        len: 0,
                        source: i as u32,
                        flat: None,
                    });
                }
            }
            continue;
        }
        if after_block {
            after_block = false;
            if c == '\n' {
                // The source's own separator after the block.
                map.push(n);
                push(&mut text, &mut n, c);
                at_line_start = true;
                continue;
            }
            push(&mut text, &mut n, '\n');
        }
        map.push(n);
        push(&mut text, &mut n, c);
        at_line_start = c == '\n';
    }
    map.push(n);
    Flat {
        map: TextMap { text, map },
        embeds: out_embeds,
    }
}

/// AT-SPI role names whose objects are blocks for [`flatten_embedded`].
pub fn atspi_block_role(role_name: &str) -> bool {
    matches!(
        role_name,
        "paragraph"
            | "section"
            | "heading"
            | "list item"
            | "list"
            | "block quote"
            | "article"
            | "form"
            | "table"
            | "table row"
            | "header"
            | "footer"
            | "landmark"
            | "description list"
            | "description term"
            | "description value"
            | "document text"
            | "caption"
            | "comment"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(s: &str) -> Option<Embedded> {
        Some(Embedded {
            text: Flat::leaf(s),
            block: true,
        })
    }

    fn inline(s: &str) -> Option<Embedded> {
        Some(Embedded {
            text: Flat::leaf(s),
            block: false,
        })
    }

    #[test]
    fn newlines_crlf_and_cr() {
        let m = normalize_newlines("a\r\nb\rc\u{2029}d");
        assert_eq!(m.text, "a\nb\nc\nd");
        assert_eq!(m.source_len(), 8);
        assert_eq!(m.len(), 7);
        // a=0 \r=1 \n=2 b=3 \r=4 c=5 U+2029=6 d=7 end=8
        assert_eq!(m.to_text(0), 0);
        assert_eq!(m.to_text(1), 1);
        assert_eq!(m.to_text(2), 2, "between \\r and \\n: after the break");
        assert_eq!(m.to_text(3), 2);
        assert_eq!(m.to_text(4), 3);
        assert_eq!(m.to_text(5), 4);
        assert_eq!(m.to_text(7), 6);
        assert_eq!(m.to_text(8), 7);
        assert_eq!(m.to_text(100), 7);
        assert_eq!(m.to_source(2), 3, "after the break: after both characters");
        assert_eq!(m.to_source(0), 0);
        assert_eq!(m.to_source(7), 8);
    }

    #[test]
    fn newlines_blank_lines_and_identity() {
        let m = normalize_newlines("one\r\n\r\nthree\r\n");
        assert_eq!(m.text, "one\n\nthree\n");
        assert_eq!(m.to_text(5), 4);
        assert_eq!(m.to_text(7), 5);
        assert_eq!(m.to_text(m.source_len()), m.len());
        let id = normalize_newlines("héllo 👩‍👩‍👧 مرحبا");
        assert_eq!(id, TextMap::identity("héllo 👩‍👩‍👧 مرحبا"));
        for i in 0..=id.source_len() {
            assert_eq!(id.to_source(id.to_text(i)), i);
        }
        assert_eq!(normalize_newlines("").len(), 0);
        assert!(normalize_newlines("").is_empty());
    }

    #[test]
    fn utf16_offsets() {
        let s = "a😀b👍🏽";
        assert_eq!(utf16_to_chars(s, 0), 0);
        assert_eq!(utf16_to_chars(s, 1), 1);
        assert_eq!(utf16_to_chars(s, 2), 2, "inside a pair rounds up");
        assert_eq!(utf16_to_chars(s, 3), 2);
        assert_eq!(utf16_to_chars(s, 4), 3);
        assert_eq!(utf16_to_chars(s, 8), 5);
        assert_eq!(utf16_to_chars(s, 99), 5);
        assert_eq!(chars_to_utf16(s, 2), 3);
        assert_eq!(chars_to_utf16(s, 5), 8);
        assert_eq!(chars_to_utf16(s, 99), 8);
    }

    #[test]
    fn paragraphs_are_joined_with_newlines() {
        let f = flatten_embedded(
            "\u{FFFC}\u{FFFC}\u{FFFC}",
            vec![block("Draft: hi"), block(""), block("second")],
        );
        assert_eq!(f.text(), "Draft: hi\n\nsecond");
        assert_eq!(
            f.embeds.iter().map(|e| e.start).collect::<Vec<_>>(),
            [0, 10, 11]
        );
        // Root offsets: each embed maps to where its content starts.
        assert_eq!(f.map.to_text(0), 0);
        assert_eq!(f.map.to_text(1), 10);
        assert_eq!(f.map.to_text(2), 11);
        assert_eq!(f.map.to_text(3), 17);
        // Caret inside a paragraph.
        assert_eq!(f.offset_at(&[0], 7), Some(7));
        assert_eq!(f.offset_at(&[2], 3), Some(14));
        assert_eq!(f.offset_at(&[1], 0), Some(10));
        assert_eq!(f.offset_at(&[3], 0), None);
    }

    #[test]
    fn first_empty_paragraph_and_trailing_br() {
        let f = flatten_embedded(
            "\u{FFFC}\u{FFFC}\u{FFFC}",
            vec![block("\n"), block("b\n"), block("c")],
        );
        assert_eq!(f.text(), "\nb\nc");
        // The caret after "b" in the second paragraph, and after its dropped `<br>`.
        assert_eq!(f.offset_at(&[1], 1), Some(2));
        assert_eq!(f.offset_at(&[1], 2), Some(2));
    }

    #[test]
    fn source_separators_are_not_doubled() {
        let f = flatten_embedded("\u{FFFC}\n\u{FFFC}", vec![block("a"), block("b")]);
        assert_eq!(f.text(), "a\nb");
        let f = flatten_embedded(
            "\u{FFFC}\n\u{FFFC}\n\u{FFFC}",
            vec![block("a"), block(""), block("b")],
        );
        assert_eq!(f.text(), "a\n\nb");
    }

    #[test]
    fn inline_text_around_blocks() {
        let f = flatten_embedded(
            "Hi \u{FFFC}!\u{FFFC}after",
            vec![inline("Maya"), block("para")],
        );
        assert_eq!(f.text(), "Hi Maya!\npara\nafter");
        assert_eq!(f.map.to_text(3), 3);
        assert_eq!(f.map.to_text(4), 7, "'!' after the link");
        assert_eq!(f.map.to_text(6), 14, "'a' of after, past the separator");
        assert_eq!(f.map.to_source(7), 4);
    }

    #[test]
    fn unreadable_embeds_and_nesting() {
        let f = flatten_embedded("a\u{FFFC}b", vec![None]);
        assert_eq!(f.text(), "ab");
        assert_eq!(f.offset_at(&[0], 0), None);
        // A list of two items inside a document, then a paragraph.
        let list = flatten_embedded("\u{FFFC}\u{FFFC}", vec![block("one"), block("two 👩🏽‍💻")]);
        let doc = flatten_embedded(
            "\u{FFFC}\u{FFFC}",
            vec![
                Some(Embedded {
                    text: list,
                    block: true,
                }),
                block("end"),
            ],
        );
        assert_eq!(doc.text(), "one\ntwo 👩🏽‍💻\nend");
        assert_eq!(doc.offset_at(&[0, 1], 4), Some(8));
        assert_eq!(doc.offset_at(&[1], 3), Some(doc.map.len()));
        assert_eq!(Flat::leaf("x\u{FFFC}y").text(), "xy");
    }

    #[test]
    fn caret_descends_into_the_paragraph_holding_it() {
        // Root "\u{FFFC}\u{FFFC}" with paragraphs "Draft: hi" and "second"; the caret is at 3
        // in the second paragraph, which the root reports as its embed offset 1.
        let f = flatten_embedded(
            "\u{FFFC}\u{FFFC}",
            vec![block("Draft: hi"), block("second")],
        );
        let mut inner = |p: &[usize]| (p == [1]).then_some(3);
        assert_eq!(f.descend(Some(1), &mut inner), Some((vec![1], 3)));
        assert_eq!(f.resolve(Some(1), &mut inner), Some(13));
        // Gecko-style "after the embed" offset for the end of a selection in the first one.
        let mut inner = |p: &[usize]| (p == [0]).then_some(5);
        assert_eq!(f.resolve(Some(1), &mut inner), Some(5));
        // The root reports nothing; a child does.
        let mut inner = |p: &[usize]| (p == [1]).then_some(6);
        assert_eq!(f.resolve(None, &mut inner), Some(16));
        // Nobody reports anything.
        assert_eq!(f.resolve(None, &mut |_| None), None);
        // No child claims the offset: the root's own offset is mapped.
        assert_eq!(f.resolve(Some(2), &mut |_| None), Some(16));
    }

    #[test]
    fn caret_descends_through_nested_objects() {
        let list = flatten_embedded("\u{FFFC}\u{FFFC}", vec![block("one"), block("two")]);
        let doc = flatten_embedded(
            "\u{FFFC}\u{FFFC}",
            vec![
                block("intro"),
                Some(Embedded {
                    text: list,
                    block: true,
                }),
            ],
        );
        assert_eq!(doc.text(), "intro\none\ntwo");
        let mut inner = |p: &[usize]| match p {
            [1] => Some(1),
            [1, 1] => Some(2),
            _ => None,
        };
        assert_eq!(doc.descend(Some(1), &mut inner), Some((vec![1, 1], 2)));
        assert_eq!(doc.resolve(Some(1), &mut inner), Some(12));
    }

    #[test]
    fn chromium_selection_end_prefers_the_child_at_the_offset() {
        // "hello" / "world"; the selection runs from inside "hello" to "wor". Chromium reports
        // the root end as 1 (the embed of "world"); both children hold part of the selection.
        let f = flatten_embedded("\u{FFFC}\u{FFFC}", vec![block("hello"), block("world")]);
        let mut end = |p: &[usize]| match p {
            [0] => Some(5),
            [1] => Some(3),
            _ => None,
        };
        assert_eq!(f.resolve(Some(1), &mut end), Some(9));
        // Gecko reports 2 (after the embed): only the child before it answers.
        assert_eq!(f.resolve(Some(2), &mut end), Some(9));
        // Offset 0: there is no object before it.
        let mut start = |p: &[usize]| (p == [0]).then_some(2);
        assert_eq!(f.descend(Some(0), &mut start), Some((vec![0], 2)));
        assert_eq!(f.descend(Some(0), &mut |_| None), Some((vec![], 0)));
    }

    #[test]
    fn descent_stops_at_the_depth_cap() {
        let mut flat = Flat::leaf("x");
        for _ in 0..Flat::MAX_DEPTH + 4 {
            flat = flatten_embedded(
                "\u{FFFC}",
                vec![Some(Embedded {
                    text: flat,
                    block: true,
                })],
            );
        }
        let (path, off) = flat.descend(Some(0), &mut |_| Some(0)).unwrap();
        assert_eq!(path.len(), Flat::MAX_DEPTH);
        assert_eq!(off, 0);
    }

    #[test]
    fn empty_and_newline_inlines_between_blocks() {
        let f = flatten_embedded(
            "\u{FFFC}\u{FFFC}\u{FFFC}",
            vec![block("a"), inline(""), block("b")],
        );
        assert_eq!(f.text(), "a\nb");
        // An inline ending in a line break before a block: no extra separator.
        let f = flatten_embedded("\u{FFFC}\u{FFFC}", vec![inline("x\n"), block("b")]);
        assert_eq!(f.text(), "x\nb");
        // A block ending in two breaks keeps one (its last `<br>` is the placeholder).
        let f = flatten_embedded("\u{FFFC}\u{FFFC}", vec![block("a\n\n"), block("b")]);
        assert_eq!(f.text(), "a\n\nb");
        // An offset inside an inline maps back to its embed character.
        let f = flatten_embedded("Hi \u{FFFC}!", vec![inline("Maya")]);
        assert_eq!(f.text(), "Hi Maya!");
        assert_eq!(f.map.to_source(5), 3);
        assert_eq!(f.map.to_source(7), 4);
    }

    #[test]
    fn newline_edge_cases() {
        let m = normalize_newlines("a\r\r\nb");
        assert_eq!(m.text, "a\n\nb");
        assert_eq!(m.to_text(2), 2);
        assert_eq!(m.to_text(4), 3);
        let m = normalize_newlines("end\r");
        assert_eq!(m.text, "end\n");
        assert_eq!(m.to_text(4), 4);
        let m = normalize_newlines("a\u{2028}b");
        assert_eq!(m.text, "a\nb");
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn rtl_and_combining_marks_keep_offsets() {
        let p = "שָׁלוֹם e\u{301}";
        let f = flatten_embedded("\u{FFFC}", vec![block(p)]);
        assert_eq!(f.text(), p);
        let n = p.chars().count() as u32;
        assert_eq!(f.offset_at(&[0], n), Some(n));
    }

    #[test]
    fn block_roles() {
        assert!(atspi_block_role("paragraph"));
        assert!(atspi_block_role("list item"));
        assert!(atspi_block_role("heading"));
        assert!(!atspi_block_role("link"));
        assert!(!atspi_block_role("static"));
    }
}
