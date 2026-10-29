//! Date phrases in queries ("last week", "in march 2025"), parsed deterministically.
//!
//! [`parse_date_query`] finds the first date phrase in a query and returns the time range it
//! names plus the query with the phrase removed. Nothing here guesses: a phrase either matches
//! one of the forms below exactly or the query is left alone.
//!
//! | phrase | range (end exclusive) |
//! |---|---|
//! | `today`, `yesterday` | that day |
//! | `this week`, `last week` / `previous week` | the ISO week (Monday start) |
//! | `this month`, `last month`, `this year`, `last year` | the calendar month / year |
//! | `past week`, `past month`, `past year` | rolling: that long before today, through today |
//! | `past N days`, `last N weeks`, ... (`N` digits or `one`–`twelve`) | rolling, through today |
//! | `last monday` | the most recent Monday before today |
//! | `2026-03-02` (ISO date, `on` optional) | that day |
//! | `march 2025` | that month |
//! | `in`/`during` `march`, `in`/`during` `2025` | the most recent March up to this month; the year |
//! | `on monday` | the most recent Monday up to today |
//! | `since <any of the above>` | from the start of that range through today |
//!
//! Month names (full, or three-letter abbreviations and `sept`), bare weekdays and bare years
//! are only dates after a cue word (`in`, `during`, `since`, and `on` for weekdays), so "you
//! may need" and "2025 tax code" are left alone. Years must be 1990–2100.
//!
//! **Time zone.** Days begin at midnight at a fixed UTC offset, UTC unless configured otherwise
//! ([`RetrieveConfig::date_utc_offset_minutes`](crate::RetrieveConfig::date_utc_offset_minutes)).
//! The daemon has no reliable notion of the user's time zone (and a fixed offset cannot follow
//! daylight saving), so UTC is the predictable default; near midnight a "yesterday" can be off by
//! the user's offset, which a caller that knows the offset can correct.

use serde::{Deserialize, Serialize};
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset, Weekday};

/// A half-open time range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
}

impl DateRange {
    pub fn contains(&self, at: OffsetDateTime) -> bool {
        self.start <= at && at < self.end
    }
}

/// A date phrase found in a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateQuery {
    pub range: DateRange,
    /// The phrase as written in the query, e.g. `"since Monday"`.
    pub phrase: String,
    /// The query without the phrase (whitespace collapsed). May be empty.
    pub cleaned: String,
}

/// Find the first date phrase in `query` (see the module docs), relative to `now`, with days
/// starting at midnight at `offset`.
pub fn parse_date_query(query: &str, now: OffsetDateTime, offset: UtcOffset) -> Option<DateQuery> {
    let toks = tokens(query);
    let today = now.to_offset(offset).date();
    for i in 0..toks.len() {
        let Some((len, start, end)) = phrase_at(query, &toks, i, today) else {
            continue;
        };
        if start >= end {
            continue;
        }
        let first = &toks[i];
        let mut last = &toks[i + len - 1];
        // "last month's invoice": the possessive goes with the phrase.
        if let Some(next) = toks.get(i + len)
            && next.text == "s"
            && matches!(&query[last.end..next.start], "'" | "\u{2019}")
        {
            last = next;
        }
        let range = DateRange {
            start: local_midnight(start, offset)?,
            end: local_midnight(end, offset)?,
        };
        let cleaned = format!("{} {}", &query[..first.start], &query[last.end..]);
        return Some(DateQuery {
            range,
            phrase: query[first.start..last.end].to_owned(),
            cleaned: cleaned.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    None
}

#[derive(Debug)]
struct Tok {
    /// Lowercased.
    text: String,
    start: usize,
    end: usize,
}

/// Maximal runs of alphanumeric characters, with byte spans into the query.
fn tokens(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push(Tok {
                    text: s[st..i].to_lowercase(),
                    start: st,
                    end: i,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(Tok {
            text: s[st..].to_lowercase(),
            start: st,
            end: s.len(),
        });
    }
    out
}

/// Local calendar range `[start, end)` in days.
type Span = (usize, Date, Date);

/// A phrase starting at token `i`: `(tokens used, first day, day after the last)`.
fn phrase_at(q: &str, toks: &[Tok], i: usize, today: Date) -> Option<Span> {
    let word = toks[i].text.as_str();
    let tomorrow = today.next_day()?;
    match word {
        "since" => {
            let (n, start, _) = anchor(q, toks, i + 1, today, Cue::Since)?;
            (start <= today).then_some((n + 1, start, tomorrow))
        }
        "in" | "during" => {
            let (n, start, end) = anchor(q, toks, i + 1, today, Cue::In)?;
            Some((n + 1, start, end))
        }
        "on" => {
            let (n, start, end) = anchor(q, toks, i + 1, today, Cue::On)?;
            Some((n + 1, start, end))
        }
        _ => anchor(q, toks, i, today, Cue::None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cue {
    None,
    /// `in` / `during`.
    In,
    On,
    Since,
}

/// A date expression at token `j`. Forms that are only dates after a cue word are accepted only
/// with the matching `cue`.
fn anchor(q: &str, toks: &[Tok], j: usize, today: Date, cue: Cue) -> Option<Span> {
    let w = |k: usize| toks.get(j + k).map(|t| t.text.as_str());
    let first = w(0)?;
    let tomorrow = today.next_day()?;
    let week_start = monday(today);
    let (year, month) = (today.year(), today.month());

    match first {
        "today" => return Some((1, today, tomorrow)),
        "yesterday" => return Some((1, today.previous_day()?, today)),
        "this" | "current" => {
            return match w(1)? {
                "week" => Some((2, week_start, week_start.checked_add(Duration::days(7))?)),
                "month" => {
                    let (ny, nm) = add_months(year, month, 1)?;
                    Some((2, first_of(year, month)?, first_of(ny, nm)?))
                }
                "year" => Some((
                    2,
                    first_of(year, Month::January)?,
                    first_of(year + 1, Month::January)?,
                )),
                _ => None,
            };
        }
        "last" | "previous" | "past" => {
            let rolling = first == "past";
            match w(1)? {
                "week" if rolling => {
                    return Some((2, today.checked_sub(Duration::days(7))?, tomorrow));
                }
                "week" => {
                    return Some((2, week_start.checked_sub(Duration::days(7))?, week_start));
                }
                "month" if rolling => {
                    return Some((2, back_months(today, 1)?, tomorrow));
                }
                "month" => {
                    let (py, pm) = add_months(year, month, -1)?;
                    return Some((2, first_of(py, pm)?, first_of(year, month)?));
                }
                "year" if rolling => return Some((2, back_months(today, 12)?, tomorrow)),
                "year" => {
                    return Some((
                        2,
                        first_of(year - 1, Month::January)?,
                        first_of(year, Month::January)?,
                    ));
                }
                other => {
                    if let Some(n) = number(other) {
                        let start = match w(2)? {
                            "day" | "days" => today.checked_sub(Duration::days(n))?,
                            "week" | "weeks" => today.checked_sub(Duration::days(n * 7))?,
                            "month" | "months" => back_months(today, n)?,
                            "year" | "years" => back_months(today, n * 12)?,
                            _ => return None,
                        };
                        return Some((3, start, tomorrow));
                    }
                    if first == "last"
                        && let Some(wd) = weekday(other)
                    {
                        // Strictly before today.
                        let d = most_recent(today.previous_day()?, wd)?;
                        return Some((2, d, d.next_day()?));
                    }
                    return None;
                }
            }
        }
        _ => {}
    }

    // ISO date: `2026-03-02` (three tokens joined by single hyphens).
    if let Some(d) = iso_date(q, toks, j) {
        return Some((3, d, d.next_day()?));
    }

    // `march 2025` needs no cue; a bare month does.
    if let Some(m) = month_name(first, cue != Cue::None) {
        if let Some(y) = w(1).and_then(year_number) {
            let (ny, nm) = add_months(y, m, 1)?;
            return Some((2, first_of(y, m)?, first_of(ny, nm)?));
        }
        if matches!(cue, Cue::In | Cue::Since) {
            // The most recent such month up to this one.
            let y = if m as u8 <= month as u8 {
                year
            } else {
                year - 1
            };
            let (ny, nm) = add_months(y, m, 1)?;
            return Some((1, first_of(y, m)?, first_of(ny, nm)?));
        }
        return None;
    }

    if matches!(cue, Cue::In | Cue::Since)
        && let Some(y) = year_number(first)
    {
        return Some((
            1,
            first_of(y, Month::January)?,
            first_of(y + 1, Month::January)?,
        ));
    }

    if matches!(cue, Cue::On | Cue::Since)
        && let Some(wd) = weekday(first)
    {
        let d = most_recent(today, wd)?;
        return Some((1, d, d.next_day()?));
    }
    None
}

fn iso_date(q: &str, toks: &[Tok], j: usize) -> Option<Date> {
    let (y, m, d) = (toks.get(j)?, toks.get(j + 1)?, toks.get(j + 2)?);
    let digits = |t: &Tok, lens: &[usize]| {
        lens.contains(&t.text.len()) && t.text.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(y, &[4]) || !digits(m, &[1, 2]) || !digits(d, &[1, 2]) {
        return None;
    }
    if &q[y.end..m.start] != "-" || &q[m.end..d.start] != "-" {
        return None;
    }
    // Not part of a longer hyphenated run (`2026-03-02-backup` is fine, `1-2026-03-02` is not).
    if j > 0 && &q[toks[j - 1].end..y.start] == "-" {
        return None;
    }
    let year = year_number(&y.text)?;
    let month = Month::try_from(m.text.parse::<u8>().ok()?).ok()?;
    Date::from_calendar_date(year, month, d.text.parse().ok()?).ok()
}

fn year_number(s: &str) -> Option<i32> {
    if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i32 = s.parse().ok()?;
    (1990..=2100).contains(&y).then_some(y)
}

/// A count for "past N days": digits (1–999) or `one`–`twelve`.
fn number(s: &str) -> Option<i64> {
    const WORDS: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    if let Some(i) = WORDS.iter().position(|w| *w == s) {
        return Some(i as i64 + 1);
    }
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = s.parse().ok()?;
    (n >= 1).then_some(n)
}

fn month_name(s: &str, allow_abbrev: bool) -> Option<Month> {
    use Month::*;
    let full = match s {
        "january" => Some(January),
        "february" => Some(February),
        "march" => Some(March),
        "april" => Some(April),
        "may" => Some(May),
        "june" => Some(June),
        "july" => Some(July),
        "august" => Some(August),
        "september" => Some(September),
        "october" => Some(October),
        "november" => Some(November),
        "december" => Some(December),
        _ => None,
    };
    if full.is_some() || !allow_abbrev {
        return full;
    }
    match s {
        "jan" => Some(January),
        "feb" => Some(February),
        "mar" => Some(March),
        "apr" => Some(April),
        "jun" => Some(June),
        "jul" => Some(July),
        "aug" => Some(August),
        "sep" | "sept" => Some(September),
        "oct" => Some(October),
        "nov" => Some(November),
        "dec" => Some(December),
        _ => None,
    }
}

fn weekday(s: &str) -> Option<Weekday> {
    use Weekday::*;
    Some(match s {
        "monday" => Monday,
        "tuesday" => Tuesday,
        "wednesday" => Wednesday,
        "thursday" => Thursday,
        "friday" => Friday,
        "saturday" => Saturday,
        "sunday" => Sunday,
        _ => return None,
    })
}

/// Monday of `d`'s ISO week.
fn monday(d: Date) -> Date {
    d - Duration::days(i64::from(d.weekday().number_days_from_monday()))
}

/// The latest `wd` on or before `d`.
fn most_recent(d: Date, wd: Weekday) -> Option<Date> {
    let back = (7 + i64::from(d.weekday().number_days_from_monday())
        - i64::from(wd.number_days_from_monday()))
        % 7;
    d.checked_sub(Duration::days(back))
}

fn first_of(year: i32, month: Month) -> Option<Date> {
    Date::from_calendar_date(year, month, 1).ok()
}

/// `(year, month)` moved by `delta` months.
fn add_months(year: i32, month: Month, delta: i64) -> Option<(i32, Month)> {
    let idx = i64::from(year) * 12 + i64::from(month as u8 - 1) + delta;
    let y = i32::try_from(idx.div_euclid(12)).ok()?;
    let m = Month::try_from(u8::try_from(idx.rem_euclid(12) + 1).ok()?).ok()?;
    Some((y, m))
}

/// The same day `n` months earlier, clamped to the month's length (Mar 31 → Feb 29 in 2024).
fn back_months(d: Date, n: i64) -> Option<Date> {
    let (y, m) = add_months(d.year(), d.month(), -n)?;
    let (ny, nm) = add_months(y, m, 1)?;
    let last = first_of(ny, nm)?.previous_day()?.day();
    Date::from_calendar_date(y, m, d.day().min(last)).ok()
}

fn local_midnight(d: Date, offset: UtcOffset) -> Option<OffsetDateTime> {
    Some(d.midnight().assume_offset(offset).to_offset(UtcOffset::UTC))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc3339;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    /// `(start date, end date)` of the parsed range, in UTC.
    fn days(q: &str, now: &str) -> Option<(String, String)> {
        let p = parse_date_query(q, at(now), UtcOffset::UTC)?;
        Some((
            p.range.start.date().to_string(),
            p.range.end.date().to_string(),
        ))
    }

    // Wednesday 4 March 2026.
    const NOW: &str = "2026-03-04T15:30:00Z";

    #[test]
    fn phrases_map_to_ranges() {
        let cases: &[(&str, &str, &str)] = &[
            ("notes from today", "2026-03-04", "2026-03-05"),
            ("what did I write yesterday", "2026-03-03", "2026-03-04"),
            ("meetings this week", "2026-03-02", "2026-03-09"),
            ("meetings last week", "2026-02-23", "2026-03-02"),
            ("previous week standup", "2026-02-23", "2026-03-02"),
            ("past week", "2026-02-25", "2026-03-05"),
            ("invoices this month", "2026-03-01", "2026-04-01"),
            ("invoices last month", "2026-02-01", "2026-03-01"),
            ("past month", "2026-02-04", "2026-03-05"),
            ("this year", "2026-01-01", "2027-01-01"),
            ("taxes last year", "2025-01-01", "2026-01-01"),
            ("past year", "2025-03-04", "2026-03-05"),
            ("emails in the past 3 days", "2026-03-01", "2026-03-05"),
            ("last 2 weeks of standups", "2026-02-18", "2026-03-05"),
            ("past six months", "2025-09-04", "2026-03-05"),
            ("past 1 day", "2026-03-03", "2026-03-05"),
            ("since monday", "2026-03-02", "2026-03-05"),
            ("since Wednesday", "2026-03-04", "2026-03-05"),
            ("since thursday", "2026-02-26", "2026-03-05"),
            ("since last week", "2026-02-23", "2026-03-05"),
            ("on monday", "2026-03-02", "2026-03-03"),
            ("last monday", "2026-03-02", "2026-03-03"),
            ("last wednesday", "2026-02-25", "2026-02-26"),
            ("call on 2026-03-02", "2026-03-02", "2026-03-03"),
            ("2025-12-31 notes", "2025-12-31", "2026-01-01"),
            ("trip in march", "2026-03-01", "2026-04-01"),
            ("trip in April", "2025-04-01", "2025-05-01"),
            ("trip during march 2025", "2025-03-01", "2025-04-01"),
            ("march 2025 budget", "2025-03-01", "2025-04-01"),
            ("in sept", "2025-09-01", "2025-10-01"),
            ("since january", "2026-01-01", "2026-03-05"),
            ("since 2025", "2025-01-01", "2026-03-05"),
            ("photos in 2024", "2024-01-01", "2025-01-01"),
            ("in may", "2025-05-01", "2025-06-01"),
        ];
        for (q, start, end) in cases {
            assert_eq!(
                days(q, NOW),
                Some((start.to_string(), end.to_string())),
                "{q:?}"
            );
        }
    }

    #[test]
    fn non_dates_do_not_match() {
        for q in [
            "you may need a permit",
            "May the force be with you",
            "the march on Washington",
            "2025 tax code",
            "room 2025",
            "in 1850",
            "in 2101",
            "last chance",
            "the last one",
            "past tense",
            "this one",
            "on the monday list", // `on the` is not `on monday`
            "monday meeting notes",
            "mar a lago",
            "on 2025-02-29",
            "1-2026-03-02",
            "since forever",
            "past 0 days",
            "past 1000 days",
            "",
        ] {
            assert_eq!(days(q, NOW), None, "{q:?}");
        }
    }

    #[test]
    fn week_boundaries_are_iso_mondays() {
        // Sunday: the week started six days ago.
        assert_eq!(
            days("this week", "2026-03-08T23:00:00Z"),
            Some(("2026-03-02".into(), "2026-03-09".into()))
        );
        // Monday: a new week.
        assert_eq!(
            days("this week", "2026-03-09T00:00:00Z"),
            Some(("2026-03-09".into(), "2026-03-16".into()))
        );
        // Last week across a year boundary (Friday 2 Jan 2026).
        assert_eq!(
            days("last week", "2026-01-02T10:00:00Z"),
            Some(("2025-12-22".into(), "2025-12-29".into()))
        );
        assert_eq!(
            days("since sunday", "2026-03-08T12:00:00Z"),
            Some(("2026-03-08".into(), "2026-03-09".into()))
        );
    }

    #[test]
    fn month_and_year_rollovers_and_leap_years() {
        assert_eq!(
            days("last month", "2026-01-15T00:00:00Z"),
            Some(("2025-12-01".into(), "2026-01-01".into()))
        );
        assert_eq!(
            days("this month", "2025-12-31T23:59:59Z"),
            Some(("2025-12-01".into(), "2026-01-01".into()))
        );
        assert_eq!(
            days("this month", "2024-02-10T00:00:00Z"),
            Some(("2024-02-01".into(), "2024-03-01".into()))
        );
        // Clamped to the shorter month, leap and non-leap.
        assert_eq!(
            days("past month", "2024-03-31T00:00:00Z"),
            Some(("2024-02-29".into(), "2024-04-01".into()))
        );
        assert_eq!(
            days("past month", "2025-03-31T00:00:00Z"),
            Some(("2025-02-28".into(), "2025-04-01".into()))
        );
        assert_eq!(
            days("past year", "2024-02-29T00:00:00Z"),
            Some(("2023-02-28".into(), "2024-03-01".into()))
        );
        assert_eq!(
            days("on 2024-02-29", NOW),
            Some(("2024-02-29".into(), "2024-03-01".into()))
        );
        assert_eq!(
            days("in february 2024", NOW),
            Some(("2024-02-01".into(), "2024-03-01".into()))
        );
        // A month in the future this year means last year's.
        assert_eq!(
            days("in december", "2026-01-05T00:00:00Z"),
            Some(("2025-12-01".into(), "2026-01-01".into()))
        );
        assert_eq!(
            days("yesterday", "2026-01-01T08:00:00Z"),
            Some(("2025-12-31".into(), "2026-01-01".into()))
        );
    }

    #[test]
    fn phrase_is_removed_from_the_query() {
        let p = parse_date_query(
            "Boiler invoices from last month's post",
            at(NOW),
            UtcOffset::UTC,
        )
        .unwrap();
        assert_eq!(p.phrase, "last month's");
        assert_eq!(p.cleaned, "Boiler invoices from post");
        let p =
            parse_date_query("what did Ana say since Monday?", at(NOW), UtcOffset::UTC).unwrap();
        assert_eq!(p.phrase, "since Monday");
        assert_eq!(p.cleaned, "what did Ana say ?");
        let p = parse_date_query("last week", at(NOW), UtcOffset::UTC).unwrap();
        assert_eq!(p.cleaned, "");
        // Only the first phrase is used.
        let p = parse_date_query("today vs yesterday", at(NOW), UtcOffset::UTC).unwrap();
        assert_eq!(p.phrase, "today");
        assert_eq!(p.cleaned, "vs yesterday");
    }

    #[test]
    fn offset_shifts_day_boundaries() {
        // 23:30 UTC on Wednesday is already Thursday at UTC+2.
        let now = at("2026-03-04T23:30:00Z");
        let plus2 = UtcOffset::from_hms(2, 0, 0).unwrap();
        let p = parse_date_query("today", now, plus2).unwrap();
        assert_eq!(p.range.start, at("2026-03-04T22:00:00Z"));
        assert_eq!(p.range.end, at("2026-03-05T22:00:00Z"));
        assert!(p.range.contains(now));
        assert!(!p.range.contains(at("2026-03-04T21:59:59Z")));
        let utc = parse_date_query("today", now, UtcOffset::UTC).unwrap();
        assert_eq!(utc.range.start, at("2026-03-04T00:00:00Z"));
    }
}
