//! Small, bounded text helpers. All input is treated as hostile: size-capped, no panics.

/// Longest HTML input scanned (the rest is ignored).
const MAX_HTML_SCAN: usize = 2 * 1024 * 1024;

/// HTML → text for event bodies (Outlook sends full documents with `<head>` and `<style>`):
/// line breaks for block tags, `<head>`/`<style>`/`<script>` contents dropped, other tags
/// dropped, common entities decoded. Output capped at `max` characters.
pub(crate) fn strip_html(s: &str, max: usize) -> String {
    let s = truncate_bytes(s, MAX_HTML_SCAN);
    if !s.contains('<') && !s.contains('&') {
        return collapse(&truncate_chars(s, max), max);
    }
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut chars = s.chars().peekable();
    // Inside <head>, <style> or <script>: text is dropped until the matching close tag.
    let mut skip: Option<&'static str> = None;
    while let Some(c) = chars.next() {
        if out.len() >= max * 4 {
            break;
        }
        match c {
            '<' => {
                let mut tag = String::new();
                for t in chars.by_ref() {
                    if t == '>' {
                        break;
                    }
                    if tag.len() < 16 {
                        tag.push(t.to_ascii_lowercase());
                    }
                }
                let closing = tag.starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                if let Some(open) = skip {
                    if closing && name == open {
                        skip = None;
                    }
                    continue;
                }
                if !closing && !tag.ends_with('/') {
                    skip = match name {
                        "head" => Some("head"),
                        "style" => Some("style"),
                        "script" => Some("script"),
                        _ => None,
                    };
                }
                if matches!(
                    name,
                    "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "table"
                ) {
                    out.push('\n');
                }
            }
            _ if skip.is_some() => {}
            '&' => {
                let mut ent = String::new();
                while let Some(&n) = chars.peek() {
                    if n == ';' || ent.len() > 8 || !(n.is_ascii_alphanumeric() || n == '#') {
                        break;
                    }
                    ent.push(n);
                    chars.next();
                }
                let decoded = match ent.as_str() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" | "#39" => Some('\''),
                    "nbsp" | "#160" => Some(' '),
                    _ => None,
                };
                match decoded {
                    Some(d) if chars.peek() == Some(&';') => {
                        chars.next();
                        out.push(d);
                    }
                    _ => {
                        out.push('&');
                        out.push_str(&ent);
                    }
                }
            }
            '\r' => {}
            c => out.push(c),
        }
    }
    collapse(&out, max)
}

/// Plain text (no markup): lines trimmed, blank runs collapsed, capped at `max` characters.
pub(crate) fn plain_text(s: &str, max: usize) -> String {
    collapse(&truncate_chars(truncate_bytes(s, MAX_HTML_SCAN), max), max)
}

/// Trim lines, collapse runs of blank lines, cap at `max` characters.
fn collapse(s: &str, max: usize) -> String {
    let mut cleaned = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    truncate_chars(cleaned.trim(), max)
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Strip control characters (except tab and newline) and cap the length: for names and other
/// short strings that end up in titles and metadata.
pub(crate) fn clean_line(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate_chars(cleaned.trim(), max)
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn strips_outlook_html_bodies() {
        let html = "<html><head><meta charset=\"utf-8\"><style>p{color:red}</style><title>t</title></head>\
            <body><div>Agenda:</div><p>1. <b>Plan</b> &amp; review</p><script>alert(1)</script>Notes&nbsp;&lt;x&gt;</body></html>";
        assert_eq!(
            strip_html(html, 1000),
            "Agenda:\n\n1. Plan & review\nNotes <x>"
        );
        assert_eq!(strip_html("<<<>>>&&&;", 100), ">>&&&;");
        assert_eq!(strip_html("plain", 3), "pla");
        assert_eq!(strip_html("<br/>a<style/>b", 10), "ab");
    }

    #[test]
    fn hostile_html_never_panics() {
        for s in [
            "<a href='x",
            "&#",
            "<style>never closed",
            "<",
            "&",
            "<\u{0}>",
            "\u{feff}<p>é",
        ] {
            let _ = strip_html(s, 10);
        }
        let big = "<p>x</p>".repeat(1_000_000);
        assert!(strip_html(&big, 100).chars().count() <= 100);
        let multibyte = "é".repeat(MAX_HTML_SCAN);
        let _ = strip_html(&multibyte, 10);
    }

    #[test]
    fn clean_line_drops_controls() {
        assert_eq!(clean_line(" a\u{0}b\nc ", 10), "a b c");
        assert_eq!(clean_line("abcdef", 3), "abc");
    }
}
