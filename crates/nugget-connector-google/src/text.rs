//! Small, bounded text helpers. All input is treated as hostile: size-capped, no panics.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Largest header block scanned for From/Subject.
const MAX_HEADER_SCAN: usize = 64 * 1024;
/// Longest header value kept.
const MAX_HEADER_VALUE: usize = 1000;

/// `From` and `Subject` from an RFC 822 message (unfolded, RFC 2047 decoded where possible).
pub(crate) fn mail_headers(raw: &[u8]) -> (Option<String>, Option<String>) {
    let head = &raw[..raw.len().min(MAX_HEADER_SCAN)];
    let text = String::from_utf8_lossy(head);
    let mut from = None;
    let mut subject = None;
    let mut current: Option<(String, String)> = None;
    let mut finish = |cur: Option<(String, String)>| {
        if let Some((name, value)) = cur {
            let slot = match name.to_ascii_lowercase().as_str() {
                "from" => &mut from,
                "subject" => &mut subject,
                _ => return,
            };
            if slot.is_none() {
                let v = truncate_chars(decode_rfc2047(value.trim()).trim(), MAX_HEADER_VALUE);
                *slot = Some(v);
            }
        }
    };
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = current.as_mut()
                && v.len() < MAX_HEADER_VALUE * 4
            {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        finish(current.take());
        if let Some((name, value)) = line.split_once(':') {
            current = Some((name.trim().to_string(), value.to_string()));
        }
    }
    finish(current);
    (
        from.filter(|s| !s.is_empty()),
        subject.filter(|s| !s.is_empty()),
    )
}

/// Decode RFC 2047 encoded words (`=?UTF-8?B?...?=`, `=?utf-8?Q?...?=`) for UTF-8, US-ASCII and
/// ISO-8859-1. Anything else is left as is.
pub(crate) fn decode_rfc2047(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let (before, after) = rest.split_at(start);
        let decoded = parse_word(&after[2..]);
        match decoded {
            Some((text, consumed)) => {
                // Whitespace between adjacent encoded words is dropped.
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&text);
                rest = &after[2 + consumed..];
                last_was_word = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &after[2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse `charset?enc?text?=`; returns the decoded text and bytes consumed.
fn parse_word(s: &str) -> Option<(String, usize)> {
    let mut parts = s.splitn(3, '?');
    let charset = parts.next()?;
    let enc = parts.next()?;
    let tail = parts.next()?;
    let end = tail.find("?=")?;
    let payload = &tail[..end];
    if charset.len() > 40 || enc.len() != 1 || payload.contains(' ') {
        return None;
    }
    let consumed = charset.len() + 1 + enc.len() + 1 + end + 2;
    let bytes = match enc {
        "B" | "b" => STANDARD
            .decode(payload.trim_end_matches('='))
            .ok()
            .or_else(|| {
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(payload.trim_end_matches('='))
                    .ok()
            })?,
        "Q" | "q" => decode_q(payload),
        _ => return None,
    };
    let charset = charset.split('*').next().unwrap_or("").to_ascii_lowercase();
    let text = match charset.as_str() {
        "utf-8" | "utf8" | "us-ascii" | "ascii" => String::from_utf8_lossy(&bytes).into_owned(),
        "iso-8859-1" | "latin1" | "latin-1" => bytes.iter().map(|&b| b as char).collect(),
        _ => return None,
    };
    Some((text, consumed))
}

fn decode_q(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'_' => out.push(b' '),
            b'=' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 2;
                }
                _ => out.push(b'='),
            },
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Crude HTML → text for calendar descriptions: line breaks for block tags, tags dropped, the
/// common entities decoded. Output capped at `max` characters.
pub(crate) fn strip_html(s: &str, max: usize) -> String {
    if !s.contains('<') && !s.contains('&') {
        return truncate_chars(s, max);
    }
    let mut out = String::with_capacity(s.len().min(max));
    let mut chars = s.chars().peekable();
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
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                if matches!(name, "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3") {
                    out.push('\n');
                }
            }
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
                    "nbsp" => Some(' '),
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
            c => out.push(c),
        }
    }
    // Collapse runs of blank lines.
    let mut cleaned = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
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

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn parses_folded_and_encoded_headers() {
        let raw = b"Received: x\r\nFrom: =?UTF-8?B?w4lsb2lzZQ==?= <e@x.org>\r\nSubject: Hello\r\n  world =?utf-8?Q?caf=C3=A9_time?=\r\n\r\nSubject: not a header\r\n";
        let (from, subject) = mail_headers(raw);
        assert_eq!(from.as_deref(), Some("\u{c9}loise <e@x.org>"));
        assert_eq!(subject.as_deref(), Some("Hello world caf\u{e9} time"));
    }

    #[test]
    fn adjacent_encoded_words_join() {
        assert_eq!(decode_rfc2047("=?utf-8?Q?a?= =?utf-8?Q?b?="), "ab");
        assert_eq!(
            decode_rfc2047("=?koi8-r?B?xxx?= plain"),
            "=?koi8-r?B?xxx?= plain"
        );
        assert_eq!(decode_rfc2047("=?ISO-8859-1?Q?caf=E9?="), "caf\u{e9}");
    }

    #[test]
    fn hostile_header_input_never_panics() {
        for raw in [
            &b""[..],
            b"\xff\xfe\x00",
            b"Subject: =?",
            b"Subject: =?utf-8?Q?=",
            b"Subject: =?utf-8?Q?=F?=",
            b"Subject: =?utf-8?B?!!!?=",
            b"Subject: =?\xc3?B?x?=",
            b":::\r\n\t\r\n",
        ] {
            let _ = mail_headers(raw);
        }
        let long = format!("Subject: {}\r\n\r\n", "x".repeat(200_000));
        let (_, s) = mail_headers(long.as_bytes());
        assert!(s.unwrap().chars().count() <= MAX_HEADER_VALUE);
    }

    #[test]
    fn strips_html_descriptions() {
        let s = strip_html(
            "Agenda:<br>1. <b>Plan</b> &amp; review<p>Notes &copy; &lt;x&gt;</p>",
            1000,
        );
        assert_eq!(s, "Agenda:\n1. Plan & review\nNotes &copy; <x>");
        assert_eq!(strip_html("<<<>>>&&&;", 100), ">>&&&;");
        assert_eq!(strip_html("plain", 3), "pla");
        let _ = strip_html("<a href='x", 10);
        let _ = strip_html("&#", 10);
    }
}
