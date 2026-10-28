//! Small, bounded text and time helpers. All input is hostile: size-capped, no panics.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub(crate) const DAY_SECS: i64 = 86_400;

/// At most `max` characters of `s`, with `…` appended when cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => {
            let mut out = s[..i].to_string();
            out.push('…');
            out
        }
    }
}

/// Drop control characters other than newline and tab; normalise line endings.
pub(crate) fn clean(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

/// The first line of `s`, whitespace collapsed, at most `max` characters.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().take(max).collect::<Vec<_>>().join(" ");
    truncate_chars(clean(&collapsed).trim(), max)
}

/// FNV-1a (64-bit) of `s` as 16 hex digits: a cheap, stable content version.
pub(crate) fn hash_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub(crate) fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// Unix seconds → RFC 3339 (UTC), or `None` when out of range.
pub(crate) fn rfc3339(unix: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

pub(crate) fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    if s.len() > 64 {
        return None;
    }
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// `YYYY-MM-DD` (UTC) of a Unix time.
pub(crate) fn date(unix: i64) -> String {
    match OffsetDateTime::from_unix_timestamp(unix) {
        Ok(t) => format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day()),
        Err(_) => "0000-00-00".into(),
    }
}

/// `HH:MM` (UTC) of a Unix time.
pub(crate) fn clock(unix: i64) -> String {
    match OffsetDateTime::from_unix_timestamp(unix) {
        Ok(t) => format!("{:02}:{:02}", t.hour(), t.minute()),
        Err(_) => "--:--".into(),
    }
}

/// Midnight UTC of the day containing `unix`.
pub(crate) fn day_start(unix: i64) -> i64 {
    unix - unix.rem_euclid(DAY_SECS)
}

/// Unix seconds of a `YYYY-MM-DD` date, or `None` when malformed.
pub(crate) fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.splitn(3, '-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    let month = time::Month::try_from(m).ok()?;
    let date = time::Date::from_calendar_date(y, month, d).ok()?;
    Some(date.midnight().assume_utc().unix_timestamp())
}

/// Whether `s` is a plausible opaque id: 1..=max chars of ASCII alphanumerics and `-_.`, and
/// not `.` or `..` (ids are interpolated into API URL paths, where those are dot-segments that
/// would walk up the path to another endpoint on the API host).
pub(crate) fn safe_id(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_and_cleaning_are_char_safe() {
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("hi", 5), "hi");
        assert_eq!(clean("a\r\nb\u{0}c\u{1b}d\te"), "a\nbcd\te");
        assert_eq!(one_line("  a \n b\tc ", 10), "a b c");
        assert_eq!(hash_hex("x"), hash_hex("x"));
        assert_ne!(hash_hex("x"), hash_hex("y"));
    }

    #[test]
    fn dates_round_trip() {
        let t = parse_date("2026-10-27").unwrap();
        assert_eq!(date(t), "2026-10-27");
        assert_eq!(day_start(t + 3_000), t);
        assert_eq!(clock(t + 3_660), "01:01");
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("x").is_none());
        assert_eq!(
            parse_rfc3339(&rfc3339(t).unwrap())
                .unwrap()
                .unix_timestamp(),
            t
        );
        assert!(safe_id("C0123-abc_.", 20));
        assert!(!safe_id("../x", 20));
        assert!(!safe_id("..", 20) && !safe_id(".", 20));
        assert!(safe_id("...", 20));
        assert!(!safe_id("", 20));
    }
}
