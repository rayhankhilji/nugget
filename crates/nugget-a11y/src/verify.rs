//! Read-back verification of an insertion.

/// The editable state of a field: its value and selection (character offsets).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldState {
    pub value: String,
    pub selection: Option<(u32, u32)>,
    /// The value was cut at the read cap, so exact comparison is impossible.
    pub truncated: bool,
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\r' | '\u{2028}' | '\u{2029}' => '\n',
            '\u{00A0}' => ' ',
            c => c,
        })
        // Rich editors add or drop a trailing newline and zero-width characters freely.
        .filter(|c| !matches!(c, '\u{200B}' | '\u{FEFF}' | '\u{FFFC}'))
        .collect::<String>()
        .trim_end_matches('\n')
        .to_string()
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

/// Whether `after` shows `text` inserted into `before`.
///
/// Exact when the selection was known and neither value was truncated (after normalising line
/// endings, NBSP and trailing newlines). Otherwise, the inserted text must appear in `after`
/// more often than in `before`.
pub fn verify_insert(before: &FieldState, text: &str, after: &FieldState) -> bool {
    let text_n = normalize(text);
    if text_n.is_empty() {
        return true;
    }
    if !before.truncated
        && !after.truncated
        && let Some(expected) = expected_after_insert(before, text)
        && normalize(&expected) == normalize(&after.value)
    {
        return true;
    }
    let count = |hay: &str| normalize(hay).matches(text_n.as_str()).count();
    count(&after.value) > count(&before.value)
}

/// Whether the field changed at all (used to avoid a second, duplicate insertion by another
/// method after an unverified one).
pub fn changed(before: &FieldState, after: &FieldState) -> bool {
    normalize(&before.value) != normalize(&after.value)
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

    #[test]
    fn change_detection() {
        assert!(!changed(&st("a\r\n", None), &st("a", None)));
        assert!(changed(&st("a", None), &st("ab", None)));
    }
}
