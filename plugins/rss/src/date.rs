//! Tolerant feed date parsing: RFC 822/2822 (RSS `pubDate`) and RFC 3339 / ISO 8601 (Atom,
//! Dublin Core `dc:date`). Feeds in the wild get both wrong in many small ways, so this accepts
//! missing weekdays, full month names, two-digit years, missing seconds, named US zones and a
//! missing zone (taken as UTC). Returns milliseconds since the Unix epoch.

/// Parse a feed date in either family.
pub fn parse(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.as_bytes()[0].is_ascii_digit() && s.len() >= 10 && s.as_bytes()[4] == b'-' {
        parse_iso(s)
    } else {
        parse_rfc2822(s)
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn to_ms(date: (i64, u32, u32), time: (u32, u32, u32, u32), offset_min: i64) -> Option<i64> {
    let ((y, mo, d), (h, mi, s, frac_ms)) = (date, time);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 24 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let secs =
        days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s.min(59) as i64 - offset_min * 60;
    Some(secs * 1000 + frac_ms as i64)
}

fn num(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `±HH:MM`, `±HHMM`, `±HH`, `Z`.
fn parse_offset(z: &str) -> Option<i64> {
    if z.is_empty() || z.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match z.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = z[1..].chars().filter(|c| *c != ':').collect();
    let (h, m) = match digits.len() {
        2 => (num(&digits)?, 0),
        4 => (num(&digits[..2])?, num(&digits[2..])?),
        _ => return None,
    };
    Some(sign * (h as i64 * 60 + m as i64))
}

fn parse_iso(s: &str) -> Option<i64> {
    let y: i64 = num(&s[..4])? as i64;
    let mo = num(s.get(5..7)?)?;
    if s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let d = num(s.get(8..10)?)?;
    let rest = &s[10..];
    if rest.is_empty() {
        return to_ms((y, mo, d), (0, 0, 0, 0), 0);
    }
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    // Split time from zone at the first `Z`, `+` or `-`.
    let zpos = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(zpos);
    let mut parts = time.trim().split(':');
    let h = num(parts.next()?)?;
    let mi = num(parts.next().unwrap_or("0"))?;
    let (sec, frac) = match parts.next() {
        Some(sp) => match sp.split_once(['.', ',']) {
            Some((s, f)) => {
                let f: String = f.chars().take(3).collect();
                let ms = num(&f)? * 10u32.pow(3 - f.len() as u32);
                (num(s)?, ms)
            }
            None => (num(sp)?, 0),
        },
        None => (0, 0),
    };
    to_ms((y, mo, d), (h, mi, sec, frac), parse_offset(zone.trim())?)
}

fn month(s: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let l = s.to_ascii_lowercase();
    let l = l.trim_end_matches('.');
    if l.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| l.starts_with(m))
        .map(|i| i as u32 + 1)
}

fn named_zone(z: &str) -> Option<i64> {
    Some(match z.to_ascii_uppercase().as_str() {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        "EST" => -5 * 60,
        "EDT" => -4 * 60,
        "CST" => -6 * 60,
        "CDT" => -5 * 60,
        "MST" => -7 * 60,
        "MDT" => -6 * 60,
        "PST" => -8 * 60,
        "PDT" => -7 * 60,
        "CET" => 60,
        "CEST" => 2 * 60,
        "BST" => 60,
        "JST" => 9 * 60,
        _ => return None,
    })
}

fn parse_rfc2822(s: &str) -> Option<i64> {
    // Drop an optional weekday ("Tue," or "Tuesday").
    let s = match s.split_once(',') {
        Some((_, rest)) => rest,
        None => s,
    };
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(|c| c.is_ascii_alphabetic()) && month(t).is_none())
    {
        tokens.remove(0);
    }
    if tokens.len() < 3 {
        return None;
    }
    // "02 Jan 2006" or (rarely) "Jan 02 2006".
    let (d, mo) = match (num(tokens[0]), month(tokens[1])) {
        (Some(d), Some(m)) => (d, m),
        _ => (num(tokens[1])?, month(tokens[0])?),
    };
    let mut y = num(tokens[2])? as i64;
    if tokens[2].len() == 2 {
        y += if y < 50 { 2000 } else { 1900 };
    }
    let (mut h, mut mi, mut sec) = (0, 0, 0);
    let mut offset = 0;
    if let Some(t) = tokens.get(3) {
        let mut p = t.split(':');
        h = num(p.next()?)?;
        mi = num(p.next().unwrap_or("0"))?;
        sec = num(p.next().unwrap_or("0"))?;
        if let Some(z) = tokens.get(4) {
            offset = parse_offset(z)
                .or_else(|| named_zone(z))
                // Military single letters and unknown names: treated as UTC.
                .unwrap_or(0);
        }
    }
    to_ms((y, mo, d), (h, mi, sec, 0), offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_136_214_245_000; // 2006-01-02T15:04:05Z

    #[test]
    fn rfc2822_variants() {
        for s in [
            "Mon, 02 Jan 2006 15:04:05 GMT",
            "Mon, 02 Jan 2006 15:04:05 +0000",
            "Mon, 2 Jan 2006 15:04:05 Z",
            "02 Jan 2006 15:04:05 UT",
            "Monday, 02 January 2006 15:04:05 GMT",
            "Mon, 02 Jan 06 15:04:05 GMT",
            "Mon, 02 Jan 2006 10:04:05 EST",
            "Mon, 02 Jan 2006 16:04:05 +01:00",
            "Mon, 02 Jan 2006 15:04:05",
            "Mon 02 Jan 2006 15:04:05 GMT",
        ] {
            assert_eq!(parse(s), Some(T), "{s}");
        }
        assert_eq!(parse("Mon, 02 Jan 2006"), Some(T - 54_245_000));
    }

    #[test]
    fn iso_variants() {
        for s in [
            "2006-01-02T15:04:05Z",
            "2006-01-02T15:04:05+00:00",
            "2006-01-02T17:04:05+02:00",
            "2006-01-02T10:04:05-05:00",
            "2006-01-02 15:04:05Z",
            "2006-01-02T15:04:05",
            "2006-01-02T15:04:05.000Z",
            "2006-01-02t15:04:05z",
        ] {
            assert_eq!(parse(s), Some(T), "{s}");
        }
        assert_eq!(parse("2006-01-02T15:04:05.5Z"), Some(T + 500));
        assert_eq!(parse("2006-01-02"), Some(T - 54_245_000));
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("1969-12-31T23:59:59Z"), Some(-1000));
    }

    #[test]
    fn garbage_is_none() {
        for s in [
            "",
            "yesterday",
            "2006-13-02T00:00:00Z",
            "Mon, 99 Foo 2006",
            "2006-01-02T15:04:05+0",
            "2006/01/02",
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }
}
