//! Deterministic text helpers: dates, money amounts, promise phrases, addresses, hashing.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, Duration, Month, Weekday};

/// First 16 hex chars of SHA-256 over the parts (joined with `\x1f`).
pub fn stable_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update([0x1f]);
        }
        h.update(p.as_bytes());
    }
    hex::encode(&h.finalize()[..8])
}

/// At most `max` chars, cut at a word boundary with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        None => s.to_owned(),
        Some((cut, _)) => {
            let head = &s[..cut];
            let head = head.rsplit_once(' ').map_or(head, |(h, _)| h);
            format!("{}…", head.trim_end())
        }
    }
}

/// A mail address with an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    /// Lowercased.
    pub email: String,
}

impl Address {
    /// Parse `Name <a@b>`, `<a@b>`, `a@b` or `Name <a@b> (accepted)`. `None` without an `@`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let (Some(lt), Some(gt)) = (s.find('<'), s.rfind('>'))
            && lt < gt
        {
            let email = s[lt + 1..gt].trim().trim_start_matches("mailto:");
            if !email.contains('@') {
                return None;
            }
            let name = s[..lt].trim().trim_matches('"').trim();
            return Some(Self {
                name: (!name.is_empty()).then(|| name.to_owned()),
                email: email.to_lowercase(),
            });
        }
        let first = s.split_whitespace().next()?;
        let email = first
            .trim_start_matches("mailto:")
            .trim_start_matches("MAILTO:");
        email.contains('@').then(|| Self {
            name: None,
            email: email.to_lowercase(),
        })
    }

    pub fn domain(&self) -> &str {
        self.email.rsplit_once('@').map_or("", |(_, d)| d)
    }

    pub fn local(&self) -> &str {
        self.email.split_once('@').map_or("", |(l, _)| l)
    }

    /// Display name, or the local part of the address.
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.local().to_owned())
    }
}

/// Strip `Re:`, `Fwd:` etc. and collapse whitespace; lowercase.
pub fn normalize_subject(s: &str) -> String {
    let mut s = s.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in ["re:", "fw:", "fwd:", "aw:", "sv:", "re :", "fwd :"] {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The body without quoted replies: stops at `On … wrote:` / `-----Original Message-----` and
/// drops `>` lines.
pub fn own_text(body: &str) -> String {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if (lower.starts_with("on ") && lower.ends_with("wrote:"))
            || lower.starts_with("-----original message")
            || lower.starts_with("from:") && !out.is_empty()
        {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Split into sentences (on `.`, `!`, `?` followed by whitespace, and on line breaks).
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut cur = String::new();
        let chars: Vec<char> = line.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            cur.push(c);
            if matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_owned());
                }
                cur.clear();
            }
        }
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_owned());
        }
    }
    out
}

/// Phrases that ask the reader for something (an answer, a decision, an action).
const ASKS: &[&str] = &[
    "could you",
    "can you",
    "would you",
    "will you",
    "would it be possible",
    "are you able",
    "do you have time",
    "let me know",
    "let us know",
    "i need you to",
    "need your",
    "i'd like your",
    "i would like your",
    "your thoughts",
    "get back to me",
    "can we",
    "could we",
    "shall we",
];

/// "please …" that does not request anything.
const PLEASE_NOT_ASKING: &[&str] = &[
    "please find",
    "please see attached",
    "please see below",
    "please note",
    "please ignore",
    "please disregard",
    "please do not reply",
    "please don't reply",
];

/// Whether one sentence asks the reader something: a question, or a request.
pub fn sentence_asks(sentence: &str) -> bool {
    let l = sentence.to_lowercase().replace('’', "'");
    let l = l.trim();
    let end = l.trim_end_matches(['"', '\'', ')', ']', '*', '_']);
    if end.ends_with('?') || end.ends_with('？') || l.starts_with('¿') {
        return true;
    }
    if ASKS.iter().any(|p| contains_phrase(l, p)) {
        return true;
    }
    // "Please send…" asks; "yes please", "thanks, please find attached" do not.
    if let Some(i) = l.find("please") {
        let before = l[..i].trim_end_matches([' ', ',']);
        let rest = &l[i..];
        let after = rest["please".len()..].trim_start_matches([' ', ',', '!', '.']);
        return !after.is_empty()
            && !before.ends_with("yes")
            && !before.ends_with("thanks")
            && !PLEASE_NOT_ASKING.iter().any(|p| rest.starts_with(p));
    }
    false
}

/// `phrase` in `text` at word boundaries.
fn contains_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + phrase.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Whether the author's own text (quotes removed) asks the reader something or requests an
/// action. Acknowledgements ("Done, you're on from the 1st."), FYIs and thanks do not.
pub fn asks_something(body: &str) -> bool {
    sentences(&own_text(body)).iter().any(|s| sentence_asks(s))
}

/// Subjects of mail that calendars and mail servers generate on a person's behalf: invitation
/// replies and updates, auto-replies, bounces. They never need a reply.
pub fn is_generated_subject(subject: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "accepted:",
        "declined:",
        "tentative:",
        "tentatively accepted:",
        "cancelled:",
        "canceled:",
        "cancelled event:",
        "canceled event:",
        "event cancelled",
        "event canceled",
        "invitation:",
        "updated invitation",
        "invitation updated",
        "updated event:",
        "new event:",
        "automatic reply",
        "auto reply",
        "auto-reply",
        "autoreply",
        "out of office",
        "undeliverable:",
        "undelivered mail",
        "delivery status notification",
        "read:",
    ];
    let l = subject.trim().to_lowercase();
    PREFIXES.iter().any(|p| l.starts_with(p))
}

/// Phrases with which the user promises to do something.
const PROMISES: &[&str] = &[
    "i'll send",
    "i will send",
    "i'll get back",
    "i will get back",
    "i'll follow up",
    "i will follow up",
    "i'll share",
    "i will share",
    "i'll circle back",
    "i'll check",
    "i will check",
    "let me check",
    "i'll look into",
    "i will look into",
    "i'll have",
    "i'll forward",
    "i will forward",
    "i'll get you",
    "i'll put together",
    "i'll draft",
    "i'll email",
    "i'll confirm",
    "i will confirm",
    "i'll review",
    "i will review",
    "i'll update you",
    "i'll let you know",
    "i will let you know",
];

/// Sentences of the user's own text that promise something.
pub fn find_promises(text: &str) -> Vec<String> {
    sentences(&own_text(text))
        .into_iter()
        .filter(|s| {
            let l = s.to_lowercase().replace('’', "'");
            PROMISES.iter().any(|p| l.contains(p))
        })
        .collect()
}

pub fn is_promise(text: &str) -> bool {
    let l = text.to_lowercase().replace('’', "'");
    PROMISES.iter().any(|p| l.contains(p))
}

/// A deadline phrase in a promise, relative to the day it was written ("by Friday", "tomorrow",
/// "next week", "end of day", an explicit date).
pub fn deadline_in(sentence: &str, written: Date) -> Option<Date> {
    let l = sentence.to_lowercase();
    if l.contains("today") || l.contains("tonight") || l.contains("eod") || l.contains("end of day")
    {
        return Some(written);
    }
    if l.contains("tomorrow") {
        return written.next_day();
    }
    if l.contains("end of week") || l.contains("end of the week") || l.contains("eow") {
        return Some(next_weekday(written, Weekday::Friday, true));
    }
    if l.contains("next week") {
        return Some(written + Duration::days(7));
    }
    if l.contains("end of month") || l.contains("end of the month") {
        let (y, m) = (written.year(), written.month());
        return Date::from_calendar_date(y, m, m.length(y)).ok();
    }
    for (name, wd) in WEEKDAYS {
        if l.contains(name) {
            return Some(next_weekday(written, *wd, false));
        }
    }
    find_dates(sentence, written).into_iter().next()
}

const WEEKDAYS: &[(&str, Weekday)] = &[
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

/// The next `wd` after `from` (or `from` itself when `inclusive` and it matches).
fn next_weekday(from: Date, wd: Weekday, inclusive: bool) -> Date {
    let mut d = if inclusive {
        from
    } else {
        from + Duration::days(1)
    };
    while d.weekday() != wd {
        d += Duration::days(1);
    }
    d
}

const MONTHS: &[(&str, Month)] = &[
    ("january", Month::January),
    ("february", Month::February),
    ("march", Month::March),
    ("april", Month::April),
    ("may", Month::May),
    ("june", Month::June),
    ("july", Month::July),
    ("august", Month::August),
    ("september", Month::September),
    ("october", Month::October),
    ("november", Month::November),
    ("december", Month::December),
];

fn month_of(tok: &str) -> Option<Month> {
    let t = tok
        .trim_matches(|c: char| !c.is_alphabetic())
        .to_lowercase();
    if t.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .find(|(name, _)| *name == t || (t.len() <= 4 && name.starts_with(t.as_str())))
        .map(|(_, m)| *m)
}

fn day_of(tok: &str) -> Option<u8> {
    let t = tok.trim_matches(|c: char| !c.is_alphanumeric());
    let t = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|s| t.strip_suffix(s))
        .unwrap_or(t);
    let d: u8 = t.parse().ok()?;
    (1..=31).contains(&d).then_some(d)
}

fn year_of(tok: &str) -> Option<i32> {
    let t = tok.trim_matches(|c: char| !c.is_ascii_digit());
    if t.len() != 4 {
        return None;
    }
    let y: i32 = t.parse().ok()?;
    (1990..=2100).contains(&y).then_some(y)
}

/// Parse an ISO date (`2026-11-03`, also the date part of RFC 3339).
pub fn parse_iso_date(s: &str) -> Option<Date> {
    let s = s.trim();
    let d = s.get(..10)?;
    let b = d.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = d[..4].parse().ok()?;
    let m: u8 = d[5..7].parse().ok()?;
    let day: u8 = d[8..10].parse().ok()?;
    Date::from_calendar_date(y, Month::try_from(m).ok()?, day).ok()
}

/// Every date written in `text`: ISO dates, `November 3, 2026`, `Nov 3`, `3 Nov 2026`. A date
/// without a year takes `reference`'s year, or the next year when that would be more than a
/// month before `reference`.
pub fn find_dates(text: &str, reference: Date) -> Vec<Date> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let infer_year = |m: Month, d: u8| -> Option<Date> {
        let this = Date::from_calendar_date(reference.year(), m, d).ok()?;
        if this < reference - Duration::days(31) {
            Date::from_calendar_date(reference.year() + 1, m, d).ok()
        } else {
            Some(this)
        }
    };
    while i < toks.len() {
        let t = toks[i];
        let trimmed = t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        if let Some(d) = parse_iso_date(trimmed) {
            out.push(d);
            i += 1;
            continue;
        }
        // Month day[,] [year]
        if let Some(m) = month_of(t)
            && let Some(d) = toks.get(i + 1).and_then(|x| day_of(x))
        {
            let year = toks.get(i + 2).and_then(|x| year_of(x));
            let date = match year {
                Some(y) => Date::from_calendar_date(y, m, d).ok(),
                None => infer_year(m, d),
            };
            if let Some(date) = date {
                out.push(date);
                i += if year.is_some() { 3 } else { 2 };
                continue;
            }
        }
        // day Month [year]
        if let Some(d) = day_of(t)
            && let Some(m) = toks.get(i + 1).and_then(|x| month_of(x))
        {
            let year = toks.get(i + 2).and_then(|x| year_of(x));
            let date = match year {
                Some(y) => Date::from_calendar_date(y, m, d).ok(),
                None => infer_year(m, d),
            };
            if let Some(date) = date {
                out.push(date);
                i += if year.is_some() { 3 } else { 2 };
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `in N days` → `reference + N`.
pub fn relative_days(text: &str, reference: Date) -> Option<Date> {
    let l = text.to_lowercase();
    if l.contains("tomorrow") {
        return reference.next_day();
    }
    let toks: Vec<&str> = l.split_whitespace().collect();
    for w in toks.windows(3) {
        if w[0] == "in"
            && w[2]
                .trim_matches(|c: char| !c.is_alphabetic())
                .starts_with("day")
            && let Ok(n) = w[1].parse::<i64>()
            && (0..=366).contains(&n)
        {
            return Some(reference + Duration::days(n));
        }
    }
    None
}

/// A money amount in minor units (cents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// ISO 4217 code.
    pub currency: String,
    pub minor: i64,
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sym = match self.currency.as_str() {
            "USD" => "$",
            "EUR" => "€",
            "GBP" => "£",
            "JPY" => "¥",
            "INR" => "₹",
            _ => "",
        };
        let v = format!("{}.{:02}", self.minor / 100, (self.minor % 100).abs());
        if sym.is_empty() {
            write!(f, "{v} {}", self.currency)
        } else {
            write!(f, "{sym}{v}")
        }
    }
}

const SYMBOLS: &[(&str, &str)] = &[
    ("US$", "USD"),
    ("CA$", "CAD"),
    ("C$", "CAD"),
    ("A$", "AUD"),
    ("AU$", "AUD"),
    ("$", "USD"),
    ("€", "EUR"),
    ("£", "GBP"),
    ("¥", "JPY"),
    ("₹", "INR"),
];

const CODES: &[&str] = &[
    "USD", "EUR", "GBP", "CAD", "AUD", "JPY", "INR", "CHF", "SEK", "NOK", "DKK", "NZD",
];

/// Parse `1,299.00`, `9,99`, `12` into minor units.
fn parse_number(s: &str) -> Option<i64> {
    let s = s.trim_end_matches(|c: char| !c.is_ascii_digit());
    if s.is_empty() || !s.chars().next()?.is_ascii_digit() {
        return None;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return None;
    }
    // Decimal separator: the last `.` or `,` followed by exactly two digits.
    let (int, frac) = match s.rfind(['.', ',']) {
        Some(p) if s.len() - p - 1 == 2 => (&s[..p], &s[p + 1..]),
        _ => (s, "00"),
    };
    let int: String = int.chars().filter(char::is_ascii_digit).collect();
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: i64 = frac.parse().ok()?;
    int.checked_mul(100)?.checked_add(frac)
}

/// Every money amount in `text`, in order.
pub fn find_amounts(text: &str) -> Vec<Money> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, raw) in toks.iter().enumerate() {
        let t = raw.trim_start_matches(['(', '[', '"']);
        // Symbol prefix: $12.99
        if let Some((sym, code)) = SYMBOLS.iter().find(|(s, _)| t.starts_with(s)) {
            let rest = &t[sym.len()..];
            let num = if rest.is_empty() {
                toks.get(i + 1).copied().unwrap_or("")
            } else {
                rest
            };
            if let Some(minor) = parse_number(num) {
                out.push(Money {
                    currency: (*code).into(),
                    minor,
                });
                continue;
            }
        }
        // Symbol suffix: 9,99€
        if let Some((sym, code)) = SYMBOLS
            .iter()
            .filter(|(s, _)| *s != "$")
            .find(|(s, _)| t.trim_end_matches(['.', ',', ')']).ends_with(s))
        {
            let num = t.trim_end_matches(['.', ',', ')']);
            if let Some(minor) = parse_number(&num[..num.len() - sym.len()]) {
                out.push(Money {
                    currency: (*code).into(),
                    minor,
                });
                continue;
            }
        }
        // ISO code before or after: USD 12.99 / 12.99 USD
        let bare = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if CODES.contains(&bare) {
            if let Some(minor) = toks.get(i + 1).and_then(|n| parse_number(n)) {
                out.push(Money {
                    currency: bare.into(),
                    minor,
                });
            }
        } else if let Some(code) = toks
            .get(i + 1)
            .map(|n| n.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|n| CODES.contains(n))
            && !toks
                .get(i.wrapping_sub(1))
                .is_some_and(|p| CODES.contains(&p.trim_matches(|c: char| !c.is_alphanumeric())))
            && let Some(minor) = parse_number(t)
        {
            out.push(Money {
                currency: code.into(),
                minor,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn dates() {
        let r = date!(2026 - 10 - 28);
        assert_eq!(
            find_dates("renews on November 3, 2026.", r),
            vec![date!(2026 - 11 - 03)]
        );
        assert_eq!(find_dates("on Nov 3", r), vec![date!(2026 - 11 - 03)]);
        assert_eq!(find_dates("by 3rd Nov", r), vec![date!(2026 - 11 - 03)]);
        assert_eq!(find_dates("Jan 5", r), vec![date!(2027 - 01 - 05)]);
        assert_eq!(find_dates("due 2026-11-02", r), vec![date!(2026 - 11 - 02)]);
        assert!(find_dates("may I ask", r).is_empty());
        assert_eq!(
            relative_days("trial ends in 3 days", r),
            Some(date!(2026 - 10 - 31))
        );
    }

    #[test]
    fn deadlines() {
        let wed = date!(2026 - 10 - 28);
        assert_eq!(
            deadline_in("I'll send it by Friday", wed),
            Some(date!(2026 - 10 - 30))
        );
        assert_eq!(
            deadline_in("tomorrow works", wed),
            Some(date!(2026 - 10 - 29))
        );
        assert_eq!(deadline_in("no date here", wed), None);
    }

    #[test]
    fn amounts() {
        assert_eq!(
            find_amounts("You will be charged $12.99 on"),
            vec![Money {
                currency: "USD".into(),
                minor: 1299
            }]
        );
        assert_eq!(find_amounts("Total: 9,99€")[0].minor, 999);
        assert_eq!(find_amounts("EUR 1,299.00")[0].minor, 129_900);
        assert_eq!(find_amounts("120 GBP per year")[0].currency, "GBP");
        assert_eq!(find_amounts("$ 5")[0].minor, 500);
        assert_eq!(
            Money {
                currency: "USD".into(),
                minor: 1299
            }
            .to_string(),
            "$12.99"
        );
    }

    #[test]
    fn addresses_and_promises() {
        let a = Address::parse("\"Ana Ruiz\" <Ana@Acme.io>").unwrap();
        assert_eq!(a.email, "ana@acme.io");
        assert_eq!(a.name.as_deref(), Some("Ana Ruiz"));
        assert_eq!(
            Address::parse("bob@x.org (accepted)").unwrap().email,
            "bob@x.org"
        );
        assert!(Address::parse("Room 4").is_none());
        let p = find_promises(
            "Thanks! I'll send the deck by Friday.\n\nOn Mon, Ana wrote:\n> I'll check",
        );
        assert_eq!(p, vec!["I'll send the deck by Friday.".to_string()]);
        assert_eq!(normalize_subject("Re: Fwd:  Q3  Plan"), "q3 plan");
    }

    #[test]
    fn asking() {
        for yes in [
            "Hi, could you send the numbers before Friday?",
            "Any update?",
            "Please send the signed copy by Monday.",
            "Let me know which variant you prefer.",
            "I'd like your view before Friday.",
            "Hi,\n\nThoughts on the draft? \n\nOn Mon, Bob wrote:\n> ok",
        ] {
            assert!(asks_something(yes), "{yes}");
        }
        for no in [
            "Done, you're on from the 1st.",
            "Yes please, the usual.",
            "Thanks for the help yesterday, it worked.",
            "Please find attached the minutes from the call.",
            "Heads up: the office is closed on Monday.",
            "Sharing the agenda ahead of our meeting. Talk soon.",
            "Got them!\n\nOn Sun, Carol wrote:\n> Did you get the photos?",
            "See https://example.com/a?b=c for details.",
            "We scanned it as best we can, you're all set.",
        ] {
            assert!(!asks_something(no), "{no}");
        }
        assert!(is_generated_subject("Accepted: Weekly sync @ Mon"));
        assert!(is_generated_subject("Updated invitation: All hands"));
        assert!(is_generated_subject("Automatic reply: Out of office"));
        assert!(!is_generated_subject("Re: Contract renewal terms"));
    }
}
