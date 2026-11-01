//! Calendar time and recurrence: event zones (TZID, via the bundled tz database), RRULE /
//! RDATE / EXDATE expansion, and the version of an event that counts (UID, RECURRENCE-ID,
//! SEQUENCE, cancellations, the user's own decline).
//!
//! RRULE support: FREQ DAILY/WEEKLY/MONTHLY/YEARLY with INTERVAL, COUNT, UNTIL, BYDAY (with
//! ordinals for monthly/yearly), BYMONTHDAY and BYMONTH. A rule using other parts (BYSETPOS,
//! BYWEEKNO, BYHOUR, ...) yields only its first occurrence.

use jiff::civil;
use jiff::tz::{Offset, TimeZone};
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday};

pub(crate) fn fixed_zone(o: UtcOffset) -> TimeZone {
    TimeZone::fixed(Offset::from_seconds(o.whole_seconds()).unwrap_or(Offset::UTC))
}

/// The offset of `tz` at instant `t`.
pub(crate) fn offset_at(tz: &TimeZone, t: OffsetDateTime) -> UtcOffset {
    let ts = jiff::Timestamp::from_second(t.unix_timestamp()).unwrap_or_default();
    UtcOffset::from_whole_seconds(tz.to_offset(ts).seconds()).unwrap_or(UtcOffset::UTC)
}

/// Wall time `dt` in `tz` (RFC 5545: the earlier instant in a fold, shifted forward in a gap),
/// in that zone's offset.
pub(crate) fn wall_time(tz: &TimeZone, dt: PrimitiveDateTime) -> OffsetDateTime {
    let fallback = || dt.assume_offset(offset_at(tz, dt.assume_utc()));
    let Ok(c) = civil::DateTime::new(
        dt.year() as i16,
        dt.month() as i8,
        dt.day() as i8,
        dt.hour() as i8,
        dt.minute() as i8,
        dt.second() as i8,
        0,
    ) else {
        return fallback();
    };
    match tz.to_ambiguous_timestamp(c).compatible() {
        Ok(ts) => match OffsetDateTime::from_unix_timestamp(ts.as_second()) {
            Ok(t) => t.to_offset(offset_at(tz, t)),
            Err(_) => fallback(),
        },
        Err(_) => fallback(),
    }
}

/// A TZID as a zone: IANA names, vendor-prefixed paths (`/mozilla.org/.../Europe/Berlin`) and
/// the common Windows names Outlook writes.
pub(crate) fn zone_named(name: &str) -> Option<TimeZone> {
    let n = name.trim().trim_matches('"');
    if let Ok(z) = TimeZone::get(n) {
        return Some(z);
    }
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
    for k in [3, 2] {
        if parts.len() > k
            && let Ok(z) = TimeZone::get(&parts[parts.len() - k..].join("/"))
        {
            return Some(z);
        }
    }
    const WINDOWS: &[(&str, &str)] = &[
        ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"),
        ("Mountain Standard Time", "America/Denver"),
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("GMT Standard Time", "Europe/London"),
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Romance Standard Time", "Europe/Paris"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("India Standard Time", "Asia/Kolkata"),
        ("China Standard Time", "Asia/Shanghai"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
    ];
    let iana = WINDOWS.iter().find(|(w, _)| w.eq_ignore_ascii_case(n))?.1;
    TimeZone::get(iana).ok()
}

/// An extractor time (`2026-10-28T14:00:00`, `...Z`, RFC 3339 or `2026-10-28`) read in `tz`
/// when it carries no offset. Returns the instant, whether it is a date, and its wall time.
pub(crate) fn resolve(s: &str, tz: &TimeZone) -> Option<(OffsetDateTime, bool, PrimitiveDateTime)> {
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return Some((t, false, PrimitiveDateTime::new(t.date(), t.time())));
    }
    let date = crate::text::parse_iso_date(s)?;
    if s.len() == 10 {
        let wall = PrimitiveDateTime::new(date, Time::MIDNIGHT);
        return Some((wall_time(tz, wall), true, wall));
    }
    let t = s.get(11..19).or_else(|| s.get(11..16))?;
    let mut parts = t.split(':').map(|p| p.parse::<u8>().ok());
    let (h, m) = (parts.next()??, parts.next()??);
    let sec = parts.next().flatten().unwrap_or(0);
    let wall = PrimitiveDateTime::new(date, Time::from_hms(h, m, sec).ok()?);
    if s.ends_with(['Z', 'z']) {
        return Some((wall.assume_utc(), false, wall));
    }
    Some((wall_time(tz, wall), false, wall))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A parsed RRULE plus the series' start, zone and exceptions.
#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    freq: Freq,
    interval: u32,
    count: Option<u32>,
    until: Option<OffsetDateTime>,
    /// `(ordinal, weekday)`; ordinal 0 is every such weekday.
    by_day: Vec<(i32, Weekday)>,
    by_month_day: Vec<i8>,
    by_month: Vec<u8>,
    /// Rule parts we do not expand: only the first occurrence is kept.
    unsupported: bool,
    start: PrimitiveDateTime,
    zone: TimeZone,
    /// Excluded occurrence starts (EXDATE, overridden or cancelled instances).
    pub(crate) exdates: Vec<OffsetDateTime>,
    rdates: Vec<OffsetDateTime>,
}

const MAX_STEPS: usize = 50_000;

impl Recurrence {
    pub(crate) fn parse(
        rrule: &str,
        start: PrimitiveDateTime,
        zone: TimeZone,
        exdates: Vec<OffsetDateTime>,
        rdates: Vec<OffsetDateTime>,
    ) -> Option<Self> {
        let mut r = Self {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            unsupported: false,
            start,
            zone,
            exdates,
            rdates,
        };
        let mut freq = None;
        for part in rrule.trim().trim_start_matches("RRULE:").split(';') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = match v.to_ascii_uppercase().as_str() {
                        "DAILY" => Some(Freq::Daily),
                        "WEEKLY" => Some(Freq::Weekly),
                        "MONTHLY" => Some(Freq::Monthly),
                        "YEARLY" => Some(Freq::Yearly),
                        _ => None,
                    }
                }
                "INTERVAL" => r.interval = v.parse().ok().filter(|n| *n > 0)?,
                "COUNT" => r.count = v.parse().ok(),
                "UNTIL" => r.until = resolve(&until_iso(v)?, &r.zone).map(|(t, ..)| t),
                "BYDAY" => {
                    for d in v.split(',') {
                        let d = d.trim().to_ascii_uppercase();
                        let (n, wd) = d.split_at(d.len().saturating_sub(2));
                        let n = if n.is_empty() { 0 } else { n.parse().ok()? };
                        r.by_day.push((n, weekday(wd)?));
                    }
                }
                "BYMONTHDAY" => {
                    for d in v.split(',') {
                        r.by_month_day.push(d.trim().parse().ok()?);
                    }
                }
                "BYMONTH" => {
                    for d in v.split(',') {
                        r.by_month
                            .push(d.trim().parse().ok().filter(|m| (1..=12).contains(m))?);
                    }
                }
                "WKST" => {}
                _ => r.unsupported = true,
            }
        }
        r.freq = freq?;
        Some(r)
    }

    /// Occurrence starts in `[from, to)`, oldest first.
    pub(crate) fn between(&self, from: OffsetDateTime, to: OffsetDateTime) -> Vec<OffsetDateTime> {
        let mut out: Vec<OffsetDateTime> = Vec::new();
        let mut n = 0u32;
        let first = wall_time(&self.zone, self.start);
        let mut emit = |t: OffsetDateTime, out: &mut Vec<OffsetDateTime>| -> bool {
            if self.until.is_some_and(|u| t > u) || self.count.is_some_and(|c| n >= c) || t >= to {
                return false;
            }
            n += 1;
            if t >= from && !self.exdates.contains(&t) {
                out.push(t);
            }
            true
        };
        if self.unsupported {
            emit(first, &mut out);
        } else {
            'periods: for step in 0..MAX_STEPS {
                let mut days = self.days_of_period(step);
                days.sort();
                for d in days {
                    let t = wall_time(&self.zone, PrimitiveDateTime::new(d, self.start.time()));
                    if t < first {
                        continue;
                    }
                    if !emit(t, &mut out) {
                        break 'periods;
                    }
                }
            }
        }
        for &r in &self.rdates {
            if r >= from && r < to && !self.exdates.contains(&r) && !out.contains(&r) {
                out.push(r);
            }
        }
        out.sort();
        out
    }

    /// The candidate days of the `step`-th period (day, week, month or year) of the series.
    fn days_of_period(&self, step: usize) -> Vec<Date> {
        let s = self.start.date();
        let k = step as i64 * i64::from(self.interval);
        let month_ok =
            |d: &Date| self.by_month.is_empty() || self.by_month.contains(&(d.month() as u8));
        match self.freq {
            Freq::Daily => {
                let d = s + Duration::days(k);
                let ok = month_ok(&d)
                    && (self.by_day.is_empty()
                        || self.by_day.iter().any(|(_, w)| *w == d.weekday()))
                    && (self.by_month_day.is_empty() || self.month_day_matches(d));
                if ok { vec![d] } else { Vec::new() }
            }
            Freq::Weekly => {
                let monday = s - Duration::days(i64::from(s.weekday().number_days_from_monday()));
                let week = monday + Duration::weeks(k);
                let wanted: Vec<Weekday> = if self.by_day.is_empty() {
                    vec![s.weekday()]
                } else {
                    self.by_day.iter().map(|(_, w)| *w).collect()
                };
                (0..7)
                    .map(|i| week + Duration::days(i))
                    .filter(|d| wanted.contains(&d.weekday()) && month_ok(d))
                    .collect()
            }
            Freq::Monthly => {
                let Some((y, m)) = add_months(s.year(), s.month(), k) else {
                    return Vec::new();
                };
                if !self.by_month.is_empty() && !self.by_month.contains(&(m as u8)) {
                    return Vec::new();
                }
                self.days_in_month(y, m, s.day())
            }
            Freq::Yearly => {
                let y = s.year() + k as i32;
                let months: Vec<Month> = if self.by_month.is_empty() {
                    vec![s.month()]
                } else {
                    self.by_month
                        .iter()
                        .filter_map(|m| Month::try_from(*m).ok())
                        .collect()
                };
                months
                    .into_iter()
                    .flat_map(|m| self.days_in_month(y, m, s.day()))
                    .collect()
            }
        }
    }

    fn month_day_matches(&self, d: Date) -> bool {
        let len = d.month().length(d.year()) as i8;
        self.by_month_day
            .iter()
            .any(|&md| md == d.day() as i8 || (md < 0 && len + md + 1 == d.day() as i8))
    }

    /// Days of month `m` that BYMONTHDAY / BYDAY select, else day `default` of it.
    fn days_in_month(&self, y: i32, m: Month, default: u8) -> Vec<Date> {
        let len = m.length(y);
        let all = (1..=len).filter_map(|d| Date::from_calendar_date(y, m, d).ok());
        if !self.by_month_day.is_empty() {
            return all.filter(|d| self.month_day_matches(*d)).collect();
        }
        if !self.by_day.is_empty() {
            let days: Vec<Date> = all.collect();
            let mut out = Vec::new();
            for &(n, w) in &self.by_day {
                let of: Vec<Date> = days.iter().copied().filter(|d| d.weekday() == w).collect();
                match n {
                    0 => out.extend(of),
                    n if n > 0 => out.extend(of.get(n as usize - 1)),
                    n => out.extend(
                        of.len()
                            .checked_sub(n.unsigned_abs() as usize)
                            .map(|i| of[i]),
                    ),
                }
            }
            return out;
        }
        Date::from_calendar_date(y, m, default)
            .into_iter()
            .collect()
    }
}

fn add_months(y: i32, m: Month, k: i64) -> Option<(i32, Month)> {
    let idx = i64::from(y) * 12 + i64::from(m as u8 - 1) + k;
    let y = i32::try_from(idx.div_euclid(12)).ok()?;
    Some((y, Month::try_from((idx.rem_euclid(12) + 1) as u8).ok()?))
}

/// `20261130T235959Z` / `20261130` (RRULE form) → the extractor's ISO form.
fn until_iso(v: &str) -> Option<String> {
    let d = v.get(..8)?;
    let date = format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]);
    match v.get(8..).and_then(|r| r.strip_prefix('T')) {
        Some(t) if t.len() >= 6 => Some(format!(
            "{date}T{}:{}:{}{}",
            &t[..2],
            &t[2..4],
            &t[4..6],
            if t.ends_with(['Z', 'z']) { "Z" } else { "" }
        )),
        _ => Some(format!("{date}T23:59:59")),
    }
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}
