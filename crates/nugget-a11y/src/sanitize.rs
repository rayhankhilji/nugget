//! Untrusted-text hygiene. Every string read from another app goes through here before it leaves
//! the crate: control and bidi-override characters are stripped and sizes are capped.

/// Default cap for a single short string (names, labels, authors).
pub const MAX_SHORT: usize = 256;
/// Default cap for a field value read for drafting.
pub const MAX_VALUE: usize = 64 * 1024;

/// Characters that are never kept: C0/C1 controls (except `\n` and `\t`), bidi embeddings and
/// overrides, isolates, the object replacement character and the BOM.
fn is_stripped(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        c if c.is_control() => true,
        '\u{200B}'
        | '\u{200E}'
        | '\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}' => true,
        '\u{FEFF}' | '\u{FFFC}' | '\u{FFFD}' => true,
        _ => false,
    }
}

/// Truncate to at most `max_bytes` bytes on a char boundary.
pub fn truncate(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Clean multi-line text: normalise line endings (`\r\n`, `\r`, U+2028/2029 to `\n`), drop
/// stripped characters, collapse runs of more than two blank lines, trim, and cap at `max_bytes`.
pub fn clean_text(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    let mut newlines = 0usize;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        let c = match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                '\n'
            }
            '\u{2028}' | '\u{2029}' | '\u{0085}' => '\n',
            '\u{00A0}' => ' ',
            c => c,
        };
        if is_stripped(c) {
            continue;
        }
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else {
            newlines = 0;
        }
        out.push(c);
        if out.len() >= max_bytes + 4 {
            break;
        }
    }
    let trimmed = out.trim();
    let mut s = if trimmed.len() == out.len() {
        out
    } else {
        trimmed.to_string()
    };
    truncate(&mut s, max_bytes);
    s
}

/// Clean a single-line string (names, labels, titles): whitespace runs, including newlines,
/// become one space.
pub fn clean_line(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    let mut space = false;
    for c in input.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if is_stripped(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        if out.len() >= max_bytes + 4 {
            break;
        }
    }
    truncate(&mut out, max_bytes);
    out
}

/// [`clean_line`], mapping an empty result to `None`.
pub fn clean_opt(input: Option<&str>, max_bytes: usize) -> Option<String> {
    let s = clean_line(input?, max_bytes);
    (!s.is_empty()).then_some(s)
}

/// Clean a field value without trimming or collapsing, so selection offsets into it stay valid:
/// every character maps to exactly one character. `\r` and line/paragraph separators become
/// `\n`, other stripped characters become a space (U+FFFC, an embedded object, is kept).
pub fn clean_value(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    for c in input.chars() {
        let c = if c == '\r' || c == '\u{2028}' || c == '\u{2029}' {
            '\n'
        } else if is_stripped(c) && c != '\u{FFFC}' {
            ' '
        } else {
            c
        };
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_bidi() {
        let s = clean_text("a\u{0007}b\u{202E}c\u{2066}d\u{0000}e\u{FEFF}", 100);
        assert_eq!(s, "abcde");
    }

    #[test]
    fn normalises_newlines_and_collapses_blank_runs() {
        let s = clean_text("one\r\ntwo\rthree\n\n\n\n\nfour  ", 100);
        assert_eq!(s, "one\ntwo\nthree\n\nfour");
    }

    #[test]
    fn caps_on_char_boundary() {
        let s = clean_text("ééééé", 5);
        assert_eq!(s, "éé");
        let mut t = "a✓b".to_string();
        truncate(&mut t, 3);
        assert_eq!(t, "a");
    }

    #[test]
    fn clean_line_collapses_whitespace() {
        assert_eq!(
            clean_line("  Re:\n  Lunch \t plans ", 100),
            "Re: Lunch plans"
        );
        assert_eq!(clean_opt(Some(" \n "), 10), None);
        assert_eq!(clean_opt(None, 10), None);
    }

    #[test]
    fn clean_value_keeps_offsets() {
        let s = clean_value("a\u{0007}b\r\nc", 100);
        assert_eq!(s.chars().count(), "a\u{0007}b\r\nc".chars().count());
        assert_eq!(s, "a b\n\nc");
        assert_eq!(clean_value("abcdef", 3), "abc");
    }
}
