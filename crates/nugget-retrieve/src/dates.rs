//! Date phrases in queries ("last week", "in march 2025"), parsed deterministically.

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

/// A date expression at token `j`.
fn anchor(q: &str, toks: &[Tok], j: usize, today: Date, cue: Cue) -> Option<Span> {
    let w = |k: usize| toks.get(j + k).map(|t| t.text.as_str());
    let first = w(0)?;
    let tomorrow = today.next_day()?;
    let week_start = monday(today);
    let (year, month) = (today.year(), today.month());

    match first {
        "today" => return Some((1, today, tomorrow)),
        "yesterday" => return Some((1, today.previous_day()?, today)),
        "tomorrow" => return Some((1, tomorrow, tomorrow.next_day()?)),
        "upcoming" => return Some((1, today, today.checked_add(Duration::days(UPCOMING_DAYS))?)),
        "coming" if w(1) == Some("up") => {
            return Some((2, today, today.checked_add(Duration::days(UPCOMING_DAYS))?));
        }
        "next" | "coming" => {
            let next_week = week_start.checked_add(Duration::days(7))?;
            match w(1)? {
                "week" if first == "coming" => {
                    return Some((2, today, today.checked_add(Duration::days(7))?));
                }
                "week" => return Some((2, next_week, next_week.checked_add(Duration::days(7))?)),
                "weekend" => {
                    let sat = next_week.checked_add(Duration::days(5))?;
                    return Some((2, sat, sat.checked_add(Duration::days(2))?));
                }
                "month" => {
                    let (ny, nm) = add_months(year, month, 1)?;
                    let (ay, am) = add_months(year, month, 2)?;
                    return Some((2, first_of(ny, nm)?, first_of(ay, am)?));
                }
                "year" => {
                    return Some((
                        2,
                        first_of(year + 1, Month::January)?,
                        first_of(year + 2, Month::January)?,
                    ));
                }
                other => {
                    if let Some(n) = number(other) {
                        let end = match w(2)? {
                            "day" | "days" => today.checked_add(Duration::days(n))?,
                            "week" | "weeks" => today.checked_add(Duration::days(n * 7))?,
                            _ => return None,
                        };
                        return Some((3, today, end.next_day()?));
                    }
                    if let Some(wd) = weekday(other) {
                        // The first such day after today.
                        let d = next_on(tomorrow, wd)?;
                        return Some((2, d, d.next_day()?));
                    }
                    return None;
                }
            }
        }
        "this" | "current" => {
            if let Some(wd) = w(1).and_then(weekday) {
                // That day of this week.
                let d = week_start
                    .checked_add(Duration::days(i64::from(wd.number_days_from_monday())))?;
                return Some((2, d, d.next_day()?));
            }
            return match w(1)? {
                "weekend" => {
                    let sat = week_start.checked_add(Duration::days(5))?;
                    Some((2, sat, sat.checked_add(Duration::days(2))?))
                }
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

    // `5 november 2026`, `november 5, 2026`, `5 nov`, `november 5`, and after a weekday
    // (`thursday 5 november`), which then names the same day.
    if let Some((n, d)) = day_month(toks, j, today) {
        return Some((n, d, d.next_day()?));
    }
    if weekday(first).is_some()
        && let Some((n, d)) = day_month(toks, j + 1, today)
    {
        return Some((n + 1, d, d.next_day()?));
    }

    // `march 2025` needs no cue; a bare month does.
    if let Some(m) = month_name(first, cue != Cue::None) {
        if let Some(y) = w(1).and_then(year_number) {
            let (ny, nm) = add_months(y, m, 1)?;
            return Some((2, first_of(y, m)?, first_of(ny, nm)?));
        }
        if matches!(cue, Cue::In | Cue::Since) {
            // The most recent such month up to this one; for `in`, one of the next two months
            // is the coming one ("in november", asked in october).
            let ahead = (i32::from(m as u8) - i32::from(month as u8)).rem_euclid(12);
            let y = if cue == Cue::In && (1..=UPCOMING_MONTHS).contains(&ahead) {
                if m as u8 > month as u8 {
                    year
                } else {
                    year + 1
                }
            } else if m as u8 <= month as u8 {
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

/// How far ahead `upcoming` and `coming up` look.
const UPCOMING_DAYS: i64 = 30;
/// A bare month after `in` that is at most this many months ahead means the coming one.
const UPCOMING_MONTHS: i32 = 2;

/// The first `wd` on or after `from`.
fn next_on(from: Date, wd: Weekday) -> Option<Date> {
    let ahead = (i64::from(wd.number_days_from_monday())
        - i64::from(from.weekday().number_days_from_monday()))
    .rem_euclid(7);
    from.checked_add(Duration::days(ahead))
}

/// A day and a month name, either order, with an optional year: `(tokens used, date)`.
fn day_month(toks: &[Tok], j: usize, today: Date) -> Option<(usize, Date)> {
    let w = |k: usize| toks.get(j + k).map(|t| t.text.as_str());
    let day = |s: &str| {
        let s = s
            .trim_end_matches("st")
            .trim_end_matches("nd")
            .trim_end_matches("rd")
            .trim_end_matches("th");
        (s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u8>().ok())
            .flatten()
            .filter(|d| (1..=31).contains(d))
    };
    let (d, m, n) = match (w(0).and_then(day), w(1).and_then(|t| month_name(t, true))) {
        (Some(d), Some(m)) => (d, m, 2),
        _ => match (w(0).and_then(|t| month_name(t, true)), w(1).and_then(day)) {
            (Some(m), Some(d)) => (d, m, 2),
            _ => return None,
        },
    };
    if let Some(y) = w(n).and_then(year_number) {
        return Some((n + 1, Date::from_calendar_date(y, m, d).ok()?));
    }
    let is_may = |k: usize| w(k) == Some("may");
    if is_may(0) || is_may(1) {
        return None;
    }
    let this_year = Date::from_calendar_date(today.year(), m, d).ok()?;
    let date = if this_year < today.checked_sub(Duration::days(183))? {
        Date::from_calendar_date(today.year() + 1, m, d).ok()?
    } else {
        this_year
    };
    Some((n, date))
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

/// Whether the query asks for the newest of something ("my bill now", "the latest invoice"),
/// so recent items should rank first.
pub fn wants_latest(query: &str) -> bool {
    tokens(query).iter().any(|t| {
        matches!(
            t.text.as_str(),
            "now" | "latest" | "current" | "currently" | "recent" | "recently" | "newest"
        )
    })
}

// ---------------------------------------------------------------- an item's own date

/// The date an item is about, for date filters, recency and citations: a calendar item's start,
/// else a mail's sent `Date`, else when the source last modified it, else when it was stored.
pub fn item_date(dates: &nugget_store::ItemDates, offset: UtcOffset) -> OffsetDateTime {
    dates
        .event_start
        .as_deref()
        .and_then(|s| parse_stored_date(s, offset))
        .or_else(|| {
            dates
                .extracted_date
                .as_deref()
                .and_then(|s| parse_stored_date(s, offset))
        })
        .or(dates.source_modified_at)
        .unwrap_or(dates.updated_at)
}

/// [`item_date`] of a loaded item.
pub fn item_date_of(item: &nugget_store::Item, offset: UtcOffset) -> OffsetDateTime {
    let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let m = &item.metadata;
    item_date(
        &nugget_store::ItemDates {
            extracted_date: text(&m["extracted"]["date"]),
            frontmatter_date: None,
            frontmatter_created: None,
            title: None,
            event_start: text(&m["extracted"]["start"])
                .or_else(|| text(&m["start"]["date_time"]))
                .or_else(|| text(&m["start"]["date"])),
            source_modified_at: item.source_modified_at,
            created_at: item.created_at,
            updated_at: item.updated_at,
        },
        offset,
    )
}

/// RFC 3339, `YYYY-MM-DDTHH:MM[:SS]` without a zone (at `offset`), or `YYYY-MM-DD` (midnight at
/// `offset`).
fn parse_stored_date(s: &str, offset: UtcOffset) -> Option<OffsetDateTime> {
    use time::format_description::well_known::Rfc3339;
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &Rfc3339) {
        return Some(t);
    }
    let day = s.get(..10)?;
    let mut parts = day.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next()?.parse().ok()?;
    let date = Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok()?;
    let rest = s[10..].trim_start_matches(['T', ' ']);
    let mut hms = rest
        .split(':')
        .map(|p| p.get(..2).and_then(|p| p.parse::<u8>().ok()));
    let time = match (hms.next().flatten(), hms.next().flatten()) {
        (Some(h), Some(mi)) => {
            time::Time::from_hms(h, mi, hms.next().flatten().unwrap_or(0)).ok()?
        }
        _ => time::Time::MIDNIGHT,
    };
    Some(
        time::PrimitiveDateTime::new(date, time)
            .assume_offset(offset)
            .to_offset(UtcOffset::UTC),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc3339;

    #[test]
    fn an_item_is_dated_by_its_event_then_its_mail_date_then_its_mtime() {
        let mtime = at("2026-10-30T09:00:00Z");
        let d = |event: Option<&str>, mail: Option<&str>| nugget_store::ItemDates {
            extracted_date: mail.map(str::to_owned),
            frontmatter_date: None,
            frontmatter_created: None,
            title: None,
            event_start: event.map(str::to_owned),
            source_modified_at: Some(mtime),
            created_at: mtime,
            updated_at: mtime,
        };
        let utc = UtcOffset::UTC;
        // A folder copied today: the event keeps its start, the mail its Date.
        assert_eq!(
            item_date(&d(Some("2026-10-19T09:30:00Z"), None), utc),
            at("2026-10-19T09:30:00Z")
        );
        assert_eq!(
            item_date(&d(None, Some("2026-10-02T08:15:00+01:00")), utc),
            at("2026-10-02T07:15:00Z")
        );
        // Floating ICS times and all-day events, at the configured offset.
        let plus2 = UtcOffset::from_hms(2, 0, 0).unwrap();
        assert_eq!(
            item_date(&d(Some("2026-11-07T10:00:00"), None), plus2),
            at("2026-11-07T08:00:00Z")
        );
        assert_eq!(
            item_date(&d(Some("2026-11-07"), None), utc),
            at("2026-11-07T00:00:00Z")
        );
        // The event wins over the mail date; garbage falls through; plain files use the mtime.
        assert_eq!(
            item_date(&d(Some("2026-11-07"), Some("2026-10-01T00:00:00Z")), utc),
            at("2026-11-07T00:00:00Z")
        );
        assert_eq!(item_date(&d(Some("soon"), Some("n/a")), utc), mtime);
        assert_eq!(item_date(&d(None, None), utc), mtime);
    }

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
            ("meetings last week", "2026-02-23", "2026-03-02"),
            ("invoices last month", "2026-02-01", "2026-03-01"),
            ("emails in the past 3 days", "2026-03-01", "2026-03-05"),
            ("past six months", "2025-09-04", "2026-03-05"),
            ("since monday", "2026-03-02", "2026-03-05"),
            ("since last week", "2026-02-23", "2026-03-05"),
            ("last wednesday", "2026-02-25", "2026-02-26"),
            ("call on 2026-03-02", "2026-03-02", "2026-03-03"),
            ("trip in march", "2026-03-01", "2026-04-01"),
            // April is the coming month (at most two months ahead), not last year's.
            ("trip in April", "2026-04-01", "2026-05-01"),
            ("march 2025 budget", "2025-03-01", "2025-04-01"),
            ("since 2025", "2025-01-01", "2026-03-05"),
        ];
        for (q, start, end) in cases {
            assert_eq!(
                days(q, NOW),
                Some((start.to_string(), end.to_string())),
                "{q:?}"
            );
        }
    }

    /// GG2-12: future phrases, explicit dates and the coming month (Wednesday 4 March 2026).
    #[test]
    fn future_phrases_and_explicit_dates() {
        let cases: &[(&str, &str, &str)] = &[
            ("What do I have next week?", "2026-03-09", "2026-03-16"),
            ("dinner next friday", "2026-03-06", "2026-03-07"),
            ("this weekend", "2026-03-07", "2026-03-09"),
            ("upcoming appointments", "2026-03-04", "2026-04-03"),
            ("next 3 days", "2026-03-04", "2026-03-08"),
            ("Thursday 5 November 2026", "2026-11-05", "2026-11-06"),
            ("call on 12th march", "2026-03-12", "2026-03-13"),
            // A bare month: the coming one if it is at most two months ahead.
            ("in april", "2026-04-01", "2026-05-01"),
            ("in june", "2025-06-01", "2025-07-01"),
        ];
        for (q, start, end) in cases {
            assert_eq!(
                days(q, NOW),
                Some((start.to_string(), end.to_string())),
                "{q:?}"
            );
        }
        // Asked in October: "in november" is the coming one (GG2-12); a day more than six
        // months ago this year is next year's.
        assert_eq!(
            days("on 5 march", "2026-10-30T10:00:00Z"),
            Some(("2027-03-05".into(), "2027-03-06".into()))
        );
        assert_eq!(
            days("in november", "2026-10-30T10:00:00Z"),
            Some(("2026-11-01".into(), "2026-12-01".into()))
        );
        for q in [
            "next steps",
            "coming soon",
            "3 may apply",
            "on the 5 of us",
            "upcoming",
        ] {
            let got = days(q, NOW);
            assert!(q == "upcoming" || got.is_none(), "{q:?}: {got:?}");
        }
    }

    #[test]
    fn non_dates_do_not_match() {
        for q in [
            "you may need a permit",
            "2025 tax code",
            "last chance",
            "on the monday list", // `on the` is not `on monday`
            "on 2025-02-29",
            "past 0 days",
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
    }

    #[test]
    fn month_and_year_rollovers_and_leap_years() {
        assert_eq!(
            days("last month", "2026-01-15T00:00:00Z"),
            Some(("2025-12-01".into(), "2026-01-01".into()))
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
            days("on 2024-02-29", NOW),
            Some(("2024-02-29".into(), "2024-03-01".into()))
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
}
