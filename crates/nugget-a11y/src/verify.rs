//! Read-back verification of an insertion.

/// The editable state of a field: its value and selection (character offsets).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldState {
    pub value: String,
    pub selection: Option<(u32, u32)>,
    /// The value was cut at the read cap, so exact comparison is impossible.
    pub truncated: bool,
}

/// Bidi marks and embeddings. The read side (`sanitize::clean_value`) turns them into spaces,
/// and editors may drop them, so an expected mark matches a space or nothing.
fn is_bidi_mark(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Line endings, NBSP, zero-width characters and U+FFFC normalised, trailing newlines dropped.
/// Bidi marks become spaces (as they read back) unless `keep_marks`.
fn normalize_with(s: &str, keep_marks: bool) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\r' | '\u{2028}' | '\u{2029}' | '\u{0085}' => '\n',
            '\u{00A0}' => ' ',
            c if !keep_marks && is_bidi_mark(c) => ' ',
            c => c,
        })
        // Rich editors add or drop a trailing newline and zero-width characters freely.
        .filter(|c| !matches!(c, '\u{200B}' | '\u{2060}' | '\u{FEFF}' | '\u{FFFC}'))
        .collect::<String>()
        .trim_end_matches('\n')
        .to_string()
}

fn normalize(s: &str) -> String {
    normalize_with(s, false)
}

/// In [`loose`] forms: a separator made only of bidi marks, which matches one space or nothing.
const OPTIONAL_SPACE: char = '\u{E000}';

/// [`normalize`], then blank lines dropped and spaces collapsed within each line: what is left
/// is the non-whitespace content in order, with line breaks where there were any. Rich editors
/// report paragraphs with `\n` or `\n\n` between them (or drop an empty paragraph, or pad one
/// with an NBSP), so this is the form their values are compared in when the exact comparison
/// fails. A run of bidi marks with no space in it becomes [`OPTIONAL_SPACE`]; compare with
/// [`loose_eq`].
fn loose(s: &str) -> String {
    let n = normalize_with(s, true);
    let mut out = String::with_capacity(n.len());
    for line in n.split('\n') {
        let mut words: Vec<&str> = Vec::new();
        let mut seps: Vec<char> = Vec::new();
        let mut rest = line;
        loop {
            let start = rest
                .find(|c: char| !(c == ' ' || c == '\t' || is_bidi_mark(c)))
                .unwrap_or(rest.len());
            let sep = &rest[..start];
            rest = &rest[start..];
            if rest.is_empty() {
                break;
            }
            if !words.is_empty() {
                seps.push(if sep.contains([' ', '\t']) {
                    ' '
                } else {
                    OPTIONAL_SPACE
                });
            }
            let end = rest
                .find(|c: char| c == ' ' || c == '\t' || is_bidi_mark(c))
                .unwrap_or(rest.len());
            words.push(&rest[..end]);
            rest = &rest[end..];
        }
        if words.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                out.push(seps[i - 1]);
            }
            out.push_str(w);
        }
    }
    out
}

/// Equality of two [`loose`] forms, where [`OPTIONAL_SPACE`] on either side matches a space or
/// nothing on the other.
fn loose_eq(a: &str, b: &str) -> bool {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return true,
            (Some(OPTIONAL_SPACE), Some(OPTIONAL_SPACE)) => {
                a.next();
                b.next();
            }
            (Some(OPTIONAL_SPACE), other) => {
                a.next();
                if other == Some(' ') {
                    b.next();
                }
            }
            (other, Some(OPTIONAL_SPACE)) => {
                b.next();
                if other == Some(' ') {
                    a.next();
                }
            }
            (Some(x), Some(y)) if x == y => {
                a.next();
                b.next();
            }
            _ => return false,
        }
    }
}

/// [`loose`] for counting occurrences (no optional separators).
fn loose_plain(s: &str) -> String {
    loose(s).replace(OPTIONAL_SPACE, "")
}

fn char_to_byte(s: &str, idx: u32) -> usize {
    s.char_indices()
        .nth(idx as usize)
        .map_or(s.len(), |(b, _)| b)
}

/// The value the field should hold after inserting `text` over `before`'s selection, or `None`
/// when the selection is unknown.
pub fn expected_after_insert(before: &FieldState, text: &str) -> Option<String> {
    let (start, end) = before.selection?;
    let (start, end) = (start.min(end), start.max(end));
    let s = char_to_byte(&before.value, start);
    let e = char_to_byte(&before.value, end);
    let mut out = String::with_capacity(before.value.len() + text.len());
    out.push_str(&before.value[..s]);
    out.push_str(text);
    out.push_str(&before.value[e..]);
    Some(out)
}

/// Whether `after` shows `text` inserted into `before`, exactly once.
///
/// 1. Exact: when the selection was known and neither value was truncated, the expected value
///    and `after` are compared after normalising line endings, U+2028/2029, NBSP, zero-width
///    characters, U+FFFC, bidi marks (which read back as spaces) and trailing newlines.
/// 2. Loose (rich editors): when that fails and the field changed, both are compared with blank
///    lines dropped and spaces collapsed, so `\n` against `\n\n` between paragraphs, a lost
///    empty paragraph or a dropped trailing newline still verify, and a bidi mark may have
///    become a space or nothing. The whole value must match, so every non-whitespace character
///    of `text` is present, in order, at the caret, with the text around it intact. With a
///    known selection and nothing truncated, these two are the only checks.
/// 3. Occurrences (selection unknown or a value truncated): `text` (exact, then loose form)
///    must appear in `after` more often than in `before`, and not more often than in the
///    expected value, so a second copy is rejected. With a known selection, the text before
///    the caret must still start the field.
///
/// A partial insertion (one paragraph of several, text cut at a length limit), an insertion
/// elsewhere, a replaced field or a selection that was not replaced fails.
pub fn verify_insert(before: &FieldState, text: &str, after: &FieldState) -> bool {
    let text_n = normalize(text);
    if text_n.is_empty() {
        return true;
    }
    let expected = expected_after_insert(before, text);
    let complete = !before.truncated && !after.truncated;
    if let Some(expected) = &expected {
        if complete {
            return normalize(expected) == normalize(&after.value)
                || (changed(before, after)
                    && !loose(text).is_empty()
                    && loose_eq(&loose(expected), &loose(&after.value)));
        }
        // Truncated: the text before the caret must have survived.
        let (start, end) = before.selection.unwrap_or_default();
        let start = start.min(end) as usize;
        if start <= before.value.chars().count() {
            let prefix: String = before.value.chars().take(start).collect();
            if !loose_plain(&after.value).starts_with(&loose_plain(&prefix)) {
                return false;
            }
        }
    }
    let expected = expected.as_deref();
    occurs_once_more(before, &text_n, after, expected, normalize)
        || occurs_once_more(before, &loose_plain(text), after, expected, loose_plain)
}

/// `needle` (already in `form`) appears in `after` more often than in `before`, and no more
/// often than in the expected value (or once more than before when that is unknown).
fn occurs_once_more(
    before: &FieldState,
    needle: &str,
    after: &FieldState,
    expected: Option<&str>,
    form: fn(&str) -> String,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    let count = |hay: &str| form(hay).matches(needle).count();
    let (b, a) = (count(&before.value), count(&after.value));
    let ceiling = expected.map_or(b + 1, count).max(b + 1);
    a > b && a <= ceiling
}

/// Whether the field may have changed (used to avoid a second, duplicate insertion by another
/// method after an unverified one). A truncated read cannot show that nothing changed, so it
/// counts as changed.
pub fn changed(before: &FieldState, after: &FieldState) -> bool {
    before.truncated || after.truncated || normalize(&before.value) != normalize(&after.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(v: &str, sel: Option<(u32, u32)>) -> FieldState {
        FieldState {
            value: v.into(),
            selection: sel,
            truncated: false,
        }
    }

    #[test]
    fn expected_replaces_selection() {
        let b = st("Hello world", Some((6, 11)));
        assert_eq!(expected_after_insert(&b, "there").unwrap(), "Hello there");
        let b = st("héllo", Some((5, 5)));
        assert_eq!(expected_after_insert(&b, " ✓").unwrap(), "héllo ✓");
        let b = st("abc", Some((2, 0)));
        assert_eq!(expected_after_insert(&b, "X").unwrap(), "Xc");
        assert_eq!(expected_after_insert(&st("abc", None), "X"), None);
    }

    #[test]
    fn exact_verification() {
        let b = st("Hello ", Some((6, 6)));
        assert!(verify_insert(&b, "there", &st("Hello there", None)));
        assert!(verify_insert(&b, "a\r\nb", &st("Hello a\nb\n", None)));
        assert!(!verify_insert(&b, "there", &st("Hello ", None)));
        assert!(!verify_insert(&b, "there", &st("Hello ther", None)));
    }

    #[test]
    fn fallback_counts_occurrences() {
        // Unknown selection: the text must appear more often than before.
        let b = st("ok ok", None);
        assert!(verify_insert(&b, "ok", &st("ok ok ok", None)));
        assert!(!verify_insert(&b, "ok", &st("ok ok", None)));
        // Rich editor reflowed whitespace around the insertion.
        let b = st("Hi\n", Some((3, 3)));
        assert!(verify_insert(
            &b,
            "Thanks!",
            &st("Hi\u{00A0}\nThanks!\n\n", None)
        ));
    }

    /// Before: "Hi " with the caret at the end.
    fn hi() -> FieldState {
        st("Hi ", Some((3, 3)))
    }

    const PARAS: &str = "Para one.\n\nPara two.\n\nPara three.";

    #[test]
    fn rich_paragraph_separators_verify() {
        // Paragraphs reported with blank lines as inserted.
        assert!(verify_insert(
            &hi(),
            PARAS,
            &st(&format!("Hi {PARAS}"), None)
        ));
        // One `\n` per paragraph (the blank lines are paragraph boundaries in the model).
        let one = "Hi Para one.\nPara two.\nPara three.";
        assert!(verify_insert(&hi(), PARAS, &st(one, None)));
        // `\n\n` reported where `\n` was inserted.
        let text = "a\nb";
        assert!(verify_insert(&hi(), text, &st("Hi a\n\nb", None)));
        // U+2029, U+FFFC, CRLF, NBSP-padded empty paragraphs.
        let odd = "Hi Para one.\u{2029}\u{00A0}\u{2029}Para two.\r\n\u{FFFC}\r\nPara three.";
        assert!(verify_insert(&hi(), PARAS, &st(odd, None)));
    }

    #[test]
    fn lost_blank_line_and_trailing_newline_verify() {
        let text = "Line one\n\n\nLine two\n";
        assert!(verify_insert(
            &hi(),
            text,
            &st("Hi Line one\n\nLine two", None)
        ));
        assert!(verify_insert(
            &hi(),
            text,
            &st("Hi Line one\nLine two\n\n", None)
        ));
        // Inserted in the middle: the trailing text stays after it.
        let b = st("Hi  bye", Some((3, 3)));
        assert!(verify_insert(&b, "x\n\ny", &st("Hi x\ny\u{00A0}bye", None)));
    }

    #[test]
    fn partial_insertions_are_rejected() {
        // Only the first paragraph landed.
        assert!(!verify_insert(&hi(), PARAS, &st("Hi Para one.", None)));
        // Paragraphs lost their order.
        assert!(!verify_insert(
            &hi(),
            PARAS,
            &st("Hi Para two.\nPara one.\nPara three.", None)
        ));
        // Same without a known selection.
        assert!(!verify_insert(
            &st("Hi ", None),
            PARAS,
            &st("Hi Para one.", None)
        ));
        // A whitespace-only change does not count as the text.
        assert!(!verify_insert(&hi(), PARAS, &st("Hi \n", None)));
    }

    #[test]
    fn duplicates_are_rejected() {
        let twice = format!("Hi {PARAS}{PARAS}");
        assert!(!verify_insert(&hi(), PARAS, &st(&twice, None)));
        let twice = format!("Hi {PARAS}\n{PARAS}");
        assert!(!verify_insert(&st("Hi ", None), PARAS, &st(&twice, None)));
        let b = st("ok ok", None);
        assert!(!verify_insert(&b, "ok", &st("ok ok ok ok", None)));
        // A loose second copy (different separators) is caught too.
        let loose_twice = "Hi Para one.\nPara two.\nPara three.Para one.\nPara two.\nPara three.";
        assert!(!verify_insert(&hi(), PARAS, &st(loose_twice, None)));
    }

    fn long_text() -> String {
        // ~5000 characters in paragraphs of mixed scripts.
        let mut t = String::new();
        let mut i = 0;
        while t.chars().count() < 5000 {
            t.push_str(&format!(
                "Paragraph {i}: the quick brown fox jumps over the lazy dog. "
            ));
            if i % 5 == 4 {
                t.push_str("\n\n");
            }
            i += 1;
        }
        t
    }

    #[test]
    fn long_text_verifies_and_truncation_is_rejected() {
        let text = long_text();
        assert!(text.chars().count() >= 5000);
        let after = format!("Hi {text}");
        assert!(verify_insert(&hi(), &text, &st(&after, None)));
        let rich = format!("Hi {}", text.replace("\n\n", "\n"));
        assert!(verify_insert(&hi(), &text, &st(&rich, None)));
        // Read back truncated (the read cap): the occurrence check still sees all of it.
        let mut cut = st(&format!("{after} and more"), None);
        cut.truncated = true;
        assert!(verify_insert(&hi(), &text, &cut));
        // The editor kept only the first 4096 characters.
        let kept: String = text.chars().take(4096).collect();
        assert!(!verify_insert(
            &hi(),
            &text,
            &st(&format!("Hi {kept}"), None)
        ));
        let mut kept_t = st(&format!("Hi {kept}"), None);
        kept_t.truncated = true;
        assert!(!verify_insert(&hi(), &text, &kept_t));
    }

    #[test]
    fn unicode_verifies_exactly() {
        for text in [
            // Emoji ZWJ sequences, skin tones, flags, keycaps.
            "Family: 👩\u{200D}👩\u{200D}👧\u{200D}👦 and 👍🏽 🇯🇵 1\u{FE0F}\u{20E3}",
            // Combining marks (decomposed é, Vietnamese stacked diacritics, Devanagari).
            "Cafe\u{0301} Vie\u{0323}\u{0302}t नमस्ते",
            // Right-to-left text, mixed with Latin and digits.
            "שלום עולם, see you at 10:30 مرحبا بالعالم",
        ] {
            let after = st(&format!("Hi {text}"), None);
            assert!(verify_insert(&hi(), text, &after), "{text}");
            // One grapheme missing is a different text.
            let mut short = text.to_string();
            short.pop();
            assert!(
                !verify_insert(&hi(), text, &st(&format!("Hi {short}"), None)),
                "{text}"
            );
        }
        // Multi-paragraph RTL through a rich editor.
        let rtl = "שלום\n\nمرحبا";
        assert!(verify_insert(&hi(), rtl, &st("Hi שלום\nمرحبا", None)));
    }

    #[test]
    fn known_selection_rejects_misplaced_insertions() {
        let b = st("Hello world", Some((11, 11)));
        // The paste replaced the whole field.
        assert!(!verify_insert(&b, " there", &st(" there", None)));
        // Inserted at the start instead of the caret.
        assert!(!verify_insert(&b, " there", &st(" thereHello world", None)));
        // The selection was not replaced.
        let b = st("Hello world", Some((6, 11)));
        assert!(!verify_insert(&b, "there", &st("Hello worldthere", None)));
        assert!(!verify_insert(&b, "there", &st("Hello there world", None)));
        assert!(verify_insert(&b, "there", &st("Hello there", None)));
        // Truncated reads: the text before the caret must survive.
        let mut b = st("Hello world", Some((6, 11)));
        b.truncated = true;
        let mut a = st("Hello there and more", None);
        a.truncated = true;
        assert!(verify_insert(&b, "there", &a));
        let mut replaced = st("there and more", None);
        replaced.truncated = true;
        assert!(!verify_insert(&b, "there", &replaced));
    }

    #[test]
    fn bidi_marks_read_back_as_spaces_or_nothing() {
        let text = "שלום\u{200F} world \u{202B}مرحبا\u{202C}!";
        // `clean_value` turned each mark into a space.
        let spaced = "Hi שלום  world  مرحبا !";
        assert!(verify_insert(&hi(), text, &st(spaced, None)));
        // The editor dropped the marks.
        let dropped = "Hi שלום world مرحبا!";
        assert!(verify_insert(&hi(), text, &st(dropped, None)));
        // A mark is not a licence for missing letters or an extra word.
        assert!(!verify_insert(
            &hi(),
            text,
            &st("Hi שלום world مرحب!", None)
        ));
        assert!(!verify_insert(
            &hi(),
            text,
            &st("Hi שלום x world مرحبا!", None)
        ));
        // A real space stays required.
        assert!(!verify_insert(&hi(), "a b", &st("Hi ab", None)));
    }

    #[test]
    fn truncated_reads_count_as_changed() {
        let mut a = st("same", None);
        assert!(!changed(&st("same", None), &a));
        a.truncated = true;
        assert!(changed(&st("same", None), &a));
    }
    #[test]
    fn change_detection() {
        assert!(!changed(&st("a\r\n", None), &st("a", None)));
        assert!(changed(&st("a", None), &st("ab", None)));
    }
}
