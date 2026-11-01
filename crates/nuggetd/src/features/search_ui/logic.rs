//! The pure parts of `search.*`: item kinds, own dates, local day ranges and months in the
//! user's time zone (or at a fixed UTC offset), disjunctive facet counts and timeline assembly.
//! Nothing here touches the store.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use nugget_core::{Error, Result};
use nugget_store::{Item, ItemDates};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// Largest UTC offset accepted, in minutes (±18 h, as `time` allows).
pub(crate) const MAX_OFFSET_MINUTES: i32 = 18 * 60;

/// The longest time zone name accepted (IANA names are far shorter).
const MAX_ZONE_NAME: usize = 64;

/// Where local days begin: a named time zone, whose offset is that of each date (so a range
/// across a daylight-saving change, or a month bucket, uses the offset in force then), or a
/// fixed UTC offset (older clients, which only send their current offset).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Zone {
    Fixed(UtcOffset),
    Named(jiff::tz::TimeZone),
}

impl Zone {
    /// A named zone (`Europe/Berlin`) from the bundled time zone database; `None` when unknown.
    pub(crate) fn named(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_ZONE_NAME {
            return None;
        }
        jiff::tz::TimeZone::get(name).ok().map(Self::Named)
    }

    /// The UTC offset in force at `at`.
    pub(crate) fn offset_at(&self, at: OffsetDateTime) -> UtcOffset {
        match self {
            Self::Fixed(o) => *o,
            Self::Named(tz) => jiff::Timestamp::from_second(at.unix_timestamp())
                .ok()
                .and_then(|ts| UtcOffset::from_whole_seconds(tz.to_offset(ts).seconds()).ok())
                .unwrap_or(UtcOffset::UTC),
        }
    }

    /// `at` as a local date and time.
    pub(crate) fn local(&self, at: OffsetDateTime) -> OffsetDateTime {
        at.to_offset(self.offset_at(at))
    }

    /// The first instant of local day `d` (midnight, or the first time that exists when a
    /// daylight-saving change skips midnight).
    pub(crate) fn midnight(&self, d: Date) -> OffsetDateTime {
        let fixed = |o: UtcOffset| PrimitiveDateTime::new(d, Time::MIDNIGHT).assume_offset(o);
        match self {
            Self::Fixed(o) => fixed(*o),
            Self::Named(tz) => {
                let start = i16::try_from(d.year())
                    .ok()
                    .and_then(|y| {
                        jiff::civil::Date::new(y, u8::from(d.month()) as i8, d.day() as i8).ok()
                    })
                    .and_then(|day| day.to_zoned(tz.clone()).ok())
                    .and_then(|z| z.start_of_day().ok())
                    .and_then(|z| {
                        OffsetDateTime::from_unix_timestamp(z.timestamp().as_second()).ok()
                    });
                match start {
                    Some(at) => self.local(at),
                    // Outside the zone database's range: the offset at that date's UTC midnight.
                    None => fixed(self.offset_at(fixed(UtcOffset::UTC))),
                }
            }
        }
    }
}

/// What an item is, for the kind facet and the result badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ItemKind {
    Mail,
    Event,
    Chat,
    File,
}

impl ItemKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Event => "event",
            Self::Chat => "chat",
            Self::File => "file",
        }
    }

    /// One of the four names exactly (filter values).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mail" => Self::Mail,
            "event" => Self::Event,
            "chat" => Self::Chat,
            "file" => Self::File,
            _ => return None,
        })
    }

    /// A metadata `kind` value: the four names plus common synonyms.
    fn from_metadata(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "message" | "email" => Some(Self::Mail),
            "calendar" => Some(Self::Event),
            other => Self::parse(other),
        }
    }

    fn from_mime(mime: &str) -> Option<Self> {
        let mime = mime.trim().to_ascii_lowercase();
        let essence = mime.split(';').next().unwrap_or("").trim();
        if essence.starts_with("message/") {
            Some(Self::Mail)
        } else if matches!(essence, "text/calendar" | "application/ics") {
            Some(Self::Event)
        } else if essence.contains("chat") {
            Some(Self::Chat)
        } else {
            None
        }
    }

    fn from_source_kind(kind: &str) -> Option<Self> {
        let k = kind.trim().to_ascii_lowercase();
        if matches!(k.as_str(), "imap" | "gmail" | "outlook") || k.contains("mail") {
            Some(Self::Mail)
        } else if matches!(k.as_str(), "caldav" | "gcal") || k.contains("calendar") {
            Some(Self::Event)
        } else if matches!(
            k.as_str(),
            "slack" | "whatsapp" | "imessage" | "telegram" | "signal" | "discord"
        ) || k.contains("chat")
            || k.contains("messag")
        {
            Some(Self::Chat)
        } else {
            None
        }
    }
}

/// The kind of an item: its metadata `kind` (or `extracted.kind`), else its mime type, else its
/// source's kind, else `file`.
pub(crate) fn classify_kind(
    metadata: &Value,
    mime: Option<&str>,
    source_kind: Option<&str>,
) -> ItemKind {
    let meta = |v: Option<&Value>| v.and_then(Value::as_str).and_then(ItemKind::from_metadata);
    meta(metadata.get("kind"))
        .or_else(|| meta(metadata.get("extracted").and_then(|e| e.get("kind"))))
        .or_else(|| mime.and_then(ItemKind::from_mime))
        .or_else(|| source_kind.and_then(ItemKind::from_source_kind))
        .unwrap_or(ItemKind::File)
}

/// Where an item's own date came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DateSource {
    /// `metadata.extracted.date` (e.g. a mail's `Date:` or an event's start).
    Extracted,
    /// The source's modification time.
    Modified,
    /// When Nugget last indexed the item (the store's `updated_at`).
    Indexed,
}

/// Where a fact's date came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FactDateSource {
    ValidFrom,
    Evidence,
    Recorded,
}

/// A strict `YYYY-MM-DD`.
fn parse_ymd(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = &s[r];
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())
            .flatten()
    };
    let y = i32::try_from(digits(0..4)?).ok()?;
    let m = Month::try_from(u8::try_from(digits(5..7)?).ok()?).ok()?;
    let d = u8::try_from(digits(8..10)?).ok()?;
    Date::from_calendar_date(y, m, d).ok()
}

/// An item's own date, as the retriever dates it ([`nugget_retrieve::dates::item_date`]: event
/// start, else extracted date, else source modification time, else last indexed), and which of
/// those it came from. Event starts report `extracted` (the contract's value for "the item says
/// when it happened").
pub(crate) fn own_date(d: &ItemDates, zone: &Zone) -> (OffsetDateTime, DateSource) {
    // A floating date ("2024-03-31") is read at the offset in force at local midnight of that
    // day: dated at UTC first, it is that day's UTC midnight (a timestamp ignores the offset).
    let probe = nugget_retrieve::dates::item_date(d, UtcOffset::UTC);
    let offset = zone.offset_at(zone.midnight(probe.date()));
    let at = nugget_retrieve::dates::item_date(d, offset);
    let fallback = d.source_modified_at.unwrap_or(d.updated_at);
    let source = if (d.event_start.is_some() || d.extracted_date.is_some()) && at != fallback {
        DateSource::Extracted
    } else if d.source_modified_at.is_some() {
        DateSource::Modified
    } else {
        DateSource::Indexed
    };
    (at, source)
}

/// The [`ItemDates`] of a loaded item (the fields `nugget_retrieve::dates::item_date_of` reads).
pub(crate) fn dates_of(item: &Item) -> ItemDates {
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let m = &item.metadata;
    ItemDates {
        extracted_date: text(&m["extracted"]["date"]),
        event_start: text(&m["extracted"]["start"])
            .or_else(|| text(&m["start"]["date_time"]))
            .or_else(|| text(&m["start"]["date"])),
        source_modified_at: item.source_modified_at,
        created_at: item.created_at,
        updated_at: item.updated_at,
    }
}

/// A fact's date: when it became true, else its earliest evidence, else when it was recorded.
pub(crate) fn fact_date(
    valid_from: Option<OffsetDateTime>,
    evidence: impl IntoIterator<Item = OffsetDateTime>,
    created: OffsetDateTime,
) -> (OffsetDateTime, FactDateSource) {
    if let Some(t) = valid_from {
        return (t, FactDateSource::ValidFrom);
    }
    match evidence.into_iter().min() {
        Some(t) => (t, FactDateSource::Evidence),
        None => (created, FactDateSource::Recorded),
    }
}

/// RFC 3339 in UTC.
pub(crate) fn rfc3339(at: OffsetDateTime) -> String {
    at.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// A UTC offset in minutes, checked.
pub(crate) fn offset_from_minutes(minutes: i32) -> Result<UtcOffset> {
    if minutes.abs() > MAX_OFFSET_MINUTES {
        return Err(Error::Invalid(format!(
            "utc_offset_minutes: {minutes} is outside ±{MAX_OFFSET_MINUTES}"
        )));
    }
    UtcOffset::from_whole_seconds(minutes * 60)
        .map_err(|e| Error::Invalid(format!("utc_offset_minutes: {e}")))
}

/// A `YYYY-MM-DD` parameter, naming `field` when it is not one.
pub(crate) fn parse_day(field: &str, s: &str) -> Result<Date> {
    parse_ymd(s.trim()).ok_or_else(|| {
        Error::Invalid(format!(
            "{field}: expected a date as YYYY-MM-DD, got {:?}",
            s.chars().take(40).collect::<String>()
        ))
    })
}

/// A half-open time range `[start, end)`; an open end is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DayRange {
    pub start: Option<OffsetDateTime>,
    pub end: Option<OffsetDateTime>,
}

impl DayRange {
    pub(crate) fn contains(&self, at: OffsetDateTime) -> bool {
        self.start.is_none_or(|s| s <= at) && self.end.is_none_or(|e| at < e)
    }
}

/// Inclusive local dates `from`..=`to` (each optional; empty means unset) in `zone`, as a
/// half-open range of instants: each end at its own day's offset. `field` prefixes the parameter names in errors (e.g.
/// `"filters."`).
pub(crate) fn day_range(
    field: &str,
    from: Option<&str>,
    to: Option<&str>,
    zone: &Zone,
) -> Result<DayRange> {
    let day = |name: &str, v: Option<&str>| -> Result<Option<Date>> {
        match v.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => parse_day(&format!("{field}{name}"), s).map(Some),
        }
    };
    let from = day("from", from)?;
    let to = day("to", to)?;
    if let (Some(f), Some(t)) = (from, to)
        && f > t
    {
        return Err(Error::Invalid(format!(
            "{field}from: {f} is after {field}to ({t})"
        )));
    }
    let midnight = |d: Date| zone.midnight(d);
    Ok(DayRange {
        start: from.map(midnight),
        // The day after `to`; past the end of the calendar means unbounded.
        end: to.and_then(|t| t.next_day()).map(midnight),
    })
}

/// `YYYY-MM` of `at` in `zone`.
pub(crate) fn month_of(at: OffsetDateTime, zone: &Zone) -> String {
    let d = zone.local(at).date();
    format!("{:04}-{:02}", d.year(), u8::from(d.month()))
}

/// `{value, count}` of a facet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FacetCount {
    pub value: String,
    pub count: u32,
}

/// Counts per month in `zone`, ascending by month.
pub(crate) fn month_counts(
    ats: impl IntoIterator<Item = OffsetDateTime>,
    zone: &Zone,
) -> Vec<FacetCount> {
    let mut by: BTreeMap<String, u32> = BTreeMap::new();
    for at in ats {
        *by.entry(month_of(at, zone)).or_default() += 1;
    }
    by.into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect()
}

// ---------------------------------------------------------------- facets

/// What faceting needs to know about one pool item.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub source: String,
    pub kind: ItemKind,
    /// Live ids of the entities it mentions.
    pub people: Vec<String>,
    pub at: OffsetDateTime,
}

/// The filters, with people as live (merge-resolved) ids. An empty list is no filter; values
/// within one filter are alternatives (OR), filters combine with AND.
#[derive(Debug, Clone, Default)]
pub(crate) struct FacetFilters {
    pub sources: HashSet<String>,
    pub kinds: HashSet<ItemKind>,
    pub people: HashSet<String>,
    pub range: DayRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dim {
    Sources,
    Kinds,
    People,
    Dates,
}

impl FacetFilters {
    fn passes(&self, c: &Candidate, except: Option<Dim>) -> bool {
        let on = |d: Dim| except != Some(d);
        (!on(Dim::Sources) || self.sources.is_empty() || self.sources.contains(&c.source))
            && (!on(Dim::Kinds) || self.kinds.is_empty() || self.kinds.contains(&c.kind))
            && (!on(Dim::People)
                || self.people.is_empty()
                || c.people.iter().any(|p| self.people.contains(p)))
            && (!on(Dim::Dates) || self.range.contains(c.at))
    }
}

/// Raw facet counts: each facet applies every filter but its own (disjunctive faceting).
/// Sources, kinds and people are sorted by count (descending), then value; months ascending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FacetTally {
    pub sources: Vec<FacetCount>,
    pub kinds: Vec<FacetCount>,
    /// Every live entity id seen, with its count.
    pub people: Vec<FacetCount>,
    pub months: Vec<FacetCount>,
}

fn sorted_counts(by: HashMap<String, u32>) -> Vec<FacetCount> {
    let mut v: Vec<FacetCount> = by
        .into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    v
}

/// Indices of the candidates passing every filter (in order), and the facet counts.
pub(crate) fn facet(
    cands: &[Candidate],
    f: &FacetFilters,
    zone: &Zone,
) -> (Vec<usize>, FacetTally) {
    let matching = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| f.passes(c, None))
        .map(|(i, _)| i)
        .collect();
    let mut sources: HashMap<String, u32> = HashMap::new();
    let mut kinds: HashMap<String, u32> = HashMap::new();
    let mut people: HashMap<String, u32> = HashMap::new();
    let mut months = Vec::new();
    for c in cands {
        if f.passes(c, Some(Dim::Sources)) {
            *sources.entry(c.source.clone()).or_default() += 1;
        }
        if f.passes(c, Some(Dim::Kinds)) {
            *kinds.entry(c.kind.as_str().to_owned()).or_default() += 1;
        }
        if f.passes(c, Some(Dim::People)) {
            let mut seen = HashSet::new();
            for p in &c.people {
                if seen.insert(p) {
                    *people.entry(p.clone()).or_default() += 1;
                }
            }
        }
        if f.passes(c, Some(Dim::Dates)) {
            months.push(c.at);
        }
    }
    (
        matching,
        FacetTally {
            sources: sorted_counts(sources),
            kinds: sorted_counts(kinds),
            people: sorted_counts(people),
            months: month_counts(months, zone),
        },
    )
}

/// `counts` plus every selected value missing from it (count 0), so the client can show and
/// clear a selection that matches nothing.
pub(crate) fn with_selected(mut counts: Vec<FacetCount>, selected: &[String]) -> Vec<FacetCount> {
    for s in selected {
        if !counts.iter().any(|c| &c.value == s) {
            counts.push(FacetCount {
                value: s.clone(),
                count: 0,
            });
        }
    }
    counts
}

/// One entry of the people facet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PersonFacet {
    pub value: String,
    pub name: String,
    pub kind: String,
    pub count: u32,
}

/// A person filter value as the client sent it, and the live id it resolves to (`None`:
/// unknown entity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedPerson {
    pub given: String,
    pub live: Option<String>,
}

/// The people facet: the `top` live ids by count, plus every selected one. A selected value
/// keeps the id the client sent (so it can clear it) and the count of the entity it resolves
/// to. `names` gives `(name, kind)` per id (given or live); unknown ones show their id.
pub(crate) fn people_facet(
    counts: &[FacetCount],
    selected: &[SelectedPerson],
    names: &HashMap<String, (String, String)>,
    top: usize,
) -> Vec<PersonFacet> {
    let count_of = |id: &str| counts.iter().find(|c| c.value == id).map_or(0, |c| c.count);
    let label = |id: &str, fallback: &str| -> (String, String) {
        names
            .get(id)
            .or_else(|| names.get(fallback))
            .cloned()
            .unwrap_or_else(|| (id.to_owned(), "unknown".to_owned()))
    };
    let mut out: Vec<PersonFacet> = Vec::new();
    for c in counts.iter().take(top) {
        let given = selected
            .iter()
            .find(|s| s.live.as_deref() == Some(c.value.as_str()))
            .map_or(c.value.as_str(), |s| s.given.as_str());
        let (name, kind) = label(&c.value, given);
        out.push(PersonFacet {
            value: given.to_owned(),
            name,
            kind,
            count: c.count,
        });
    }
    for s in selected {
        if out.iter().any(|p| p.value == s.given) {
            continue;
        }
        let live = s.live.as_deref().unwrap_or(&s.given);
        let (name, kind) = label(live, &s.given);
        out.push(PersonFacet {
            value: s.given.clone(),
            name,
            kind,
            count: s.live.as_deref().map_or(0, count_of),
        });
    }
    out
}

// ---------------------------------------------------------------- timeline

/// One timeline event before it is rendered: `payload` is whatever the caller needs to build
/// it; `tie` orders same-instant events of the same type (a title, then anything stable).
#[derive(Debug, Clone)]
pub(crate) struct Dated<T> {
    pub at: OffsetDateTime,
    pub is_fact: bool,
    pub tie: String,
    pub payload: T,
}

/// Ascending by time; at the same instant items before facts, then by `tie`.
pub(crate) fn timeline_order<T>(a: &Dated<T>, b: &Dated<T>) -> Ordering {
    a.at.cmp(&b.at)
        .then(a.is_fact.cmp(&b.is_fact))
        .then_with(|| a.tie.cmp(&b.tie))
}

/// The events kept for a timeline page.
#[derive(Debug, Clone)]
pub(crate) struct TimelineCut<T> {
    /// Ascending; the newest `limit` of the matching events.
    pub kept: Vec<Dated<T>>,
    /// Over every matching event (before the limit).
    pub months: Vec<FacetCount>,
    pub total: usize,
    pub truncated: bool,
}

/// Keep the events in `range`, sorted; when more than `limit` match, keep the newest `limit`.
pub(crate) fn assemble<T>(
    events: Vec<Dated<T>>,
    range: &DayRange,
    limit: usize,
    zone: &Zone,
) -> TimelineCut<T> {
    let mut kept: Vec<Dated<T>> = events
        .into_iter()
        .filter(|e| range.contains(e.at))
        .collect();
    kept.sort_by(timeline_order);
    let total = kept.len();
    let months = month_counts(kept.iter().map(|e| e.at), zone);
    let truncated = total > limit;
    if truncated {
        kept.drain(..total - limit);
    }
    TimelineCut {
        kept,
        months,
        total,
        truncated,
    }
}

// ---------------------------------------------------------------- text

/// At most `max` characters of `text` (cut on a char boundary), and whether it was cut.
pub(crate) fn cut_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((i, _)) => (&text[..i], true),
        None => (text, false),
    }
}

/// A one-line preview: whitespace collapsed, at most about `max` characters (`…` when cut).
pub(crate) fn snippet(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        let sep = usize::from(!out.is_empty());
        if n + sep + len > max {
            if out.is_empty() {
                out.extend(word.chars().take(max));
            }
            out.push('…');
            return out;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(word);
        n += sep + len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dt(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    fn off(min: i32) -> Zone {
        Zone::Fixed(offset_from_minutes(min).unwrap())
    }

    #[test]
    fn kind_prefers_metadata_then_mime_then_source() {
        let none = json!({});
        assert_eq!(
            classify_kind(&json!({"kind": "email"}), None, None),
            ItemKind::Mail
        );
        assert_eq!(
            classify_kind(&json!({"kind": "Calendar"}), Some("message/rfc822"), None),
            ItemKind::Event
        );
        assert_eq!(
            classify_kind(&json!({"extracted": {"kind": "chat"}}), None, Some("imap")),
            ItemKind::Chat
        );
        assert_eq!(
            classify_kind(&json!({"kind": "file"}), Some("message/rfc822"), None),
            ItemKind::File
        );
        // An unknown metadata kind falls through to the mime type.
        assert_eq!(
            classify_kind(&json!({"kind": "note"}), Some("message/rfc822"), None),
            ItemKind::Mail
        );
        assert_eq!(
            classify_kind(&none, Some("message/partial"), None),
            ItemKind::Mail
        );
        assert_eq!(
            classify_kind(&none, Some("text/calendar; charset=utf-8"), None),
            ItemKind::Event
        );
        assert_eq!(
            classify_kind(&none, Some("application/ics"), None),
            ItemKind::Event
        );
        assert_eq!(
            classify_kind(&none, Some("application/x-slack-chat+json"), None),
            ItemKind::Chat
        );
        assert_eq!(
            classify_kind(&none, Some("text/plain"), Some("gmail")),
            ItemKind::Mail
        );
        for (src, want) in [
            ("imap", ItemKind::Mail),
            ("outlook", ItemKind::Mail),
            ("fastmail", ItemKind::Mail),
            ("caldav", ItemKind::Event),
            ("gcal", ItemKind::Event),
            ("outlook-calendar", ItemKind::Event),
            ("slack", ItemKind::Chat),
            ("whatsapp", ItemKind::Chat),
            ("imessage", ItemKind::Chat),
            ("teams-chat", ItemKind::Chat),
            ("messages", ItemKind::Chat),
            ("files", ItemKind::File),
            ("notion", ItemKind::File),
        ] {
            assert_eq!(classify_kind(&none, None, Some(src)), want, "{src}");
        }
        assert_eq!(classify_kind(&none, None, None), ItemKind::File);
        // Non-string kinds are ignored.
        assert_eq!(
            classify_kind(&json!({"kind": 3}), None, None),
            ItemKind::File
        );
    }

    #[test]
    fn kind_names_round_trip() {
        for k in [
            ItemKind::Mail,
            ItemKind::Event,
            ItemKind::Chat,
            ItemKind::File,
        ] {
            assert_eq!(ItemKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        assert_eq!(
            ItemKind::parse("email"),
            None,
            "filters take the four names only"
        );
    }

    fn dates(
        extracted: Option<&str>,
        event: Option<&str>,
        modified: Option<&str>,
        updated: &str,
    ) -> ItemDates {
        ItemDates {
            extracted_date: extracted.map(str::to_owned),
            event_start: event.map(str::to_owned),
            source_modified_at: modified.map(dt),
            created_at: dt("2020-01-01T00:00:00Z"),
            updated_at: dt(updated),
        }
    }

    #[test]
    fn own_date_follows_the_retriever_and_names_its_source() {
        let u = "2024-02-01T00:00:00Z";
        let m = Some("2024-01-05T12:00:00Z");
        assert_eq!(
            own_date(&dates(Some("2023-12-24T10:00:00Z"), None, m, u), &off(0)),
            (dt("2023-12-24T10:00:00Z"), DateSource::Extracted)
        );
        // An event's start wins over the extracted date and reports `extracted`.
        assert_eq!(
            own_date(
                &dates(
                    Some("2023-12-24T10:00:00Z"),
                    Some("2024-06-01T09:00:00Z"),
                    m,
                    u
                ),
                &off(0)
            ),
            (dt("2024-06-01T09:00:00Z"), DateSource::Extracted)
        );
        // Floating dates are read at the offset.
        assert_eq!(
            own_date(&dates(Some("2023-12-24"), None, None, u), &off(60)).0,
            dt("2023-12-23T23:00:00Z")
        );
        assert_eq!(
            own_date(&dates(Some("garbage"), None, m, u), &off(0)),
            (dt("2024-01-05T12:00:00Z"), DateSource::Modified)
        );
        assert_eq!(
            own_date(&dates(None, None, None, u), &off(0)),
            (dt(u), DateSource::Indexed)
        );
        assert_eq!(
            serde_json::to_value(DateSource::Indexed).unwrap(),
            "indexed"
        );
    }

    #[test]
    fn fact_date_falls_back_in_order() {
        let v = dt("2020-01-01T00:00:00Z");
        let e1 = dt("2024-03-01T00:00:00Z");
        let e2 = dt("2023-03-01T00:00:00Z");
        let c = dt("2025-01-01T00:00:00Z");
        assert_eq!(fact_date(Some(v), [e1], c), (v, FactDateSource::ValidFrom));
        assert_eq!(fact_date(None, [e1, e2], c), (e2, FactDateSource::Evidence));
        assert_eq!(fact_date(None, [], c), (c, FactDateSource::Recorded));
        assert_eq!(
            serde_json::to_value(FactDateSource::ValidFrom).unwrap(),
            "valid_from"
        );
    }

    #[test]
    fn day_ranges_are_local_and_inclusive() {
        let r = day_range("", Some("2024-03-01"), Some("2024-03-31"), &off(60)).unwrap();
        assert_eq!(r.start, Some(dt("2024-02-29T23:00:00Z")));
        assert_eq!(r.end, Some(dt("2024-03-31T23:00:00Z")));
        assert!(
            r.contains(dt("2024-02-29T23:30:00Z")),
            "00:30 local on the 1st"
        );
        assert!(!r.contains(dt("2024-02-29T22:59:00Z")));
        assert!(r.contains(dt("2024-03-31T22:59:00Z")));
        assert!(!r.contains(dt("2024-03-31T23:00:00Z")));

        let open = day_range("", Some(""), None, &off(0)).unwrap();
        assert_eq!(open, DayRange::default());
        let only_to = day_range("", None, Some("2024-01-01"), &off(-300)).unwrap();
        assert_eq!(only_to.start, None);
        assert_eq!(only_to.end, Some(dt("2024-01-02T05:00:00Z")));

        let err = day_range("filters.", Some("2024-1-1"), None, &off(0)).unwrap_err();
        assert!(err.to_string().contains("filters.from"), "{err}");
        let err = day_range("", None, Some("tomorrow"), &off(0)).unwrap_err();
        assert!(err.to_string().contains("to:"), "{err}");
        let err = day_range("", Some("2024-02-02"), Some("2024-02-01"), &off(0)).unwrap_err();
        assert!(err.to_string().contains("after"), "{err}");
        assert!(matches!(err, Error::Invalid(_)));
        assert_eq!(
            parse_day("x", "2024-02-29").unwrap(),
            Date::from_calendar_date(2024, Month::February, 29).unwrap()
        );
    }

    #[test]
    fn offsets_are_bounded() {
        assert!(offset_from_minutes(18 * 60).is_ok());
        assert!(offset_from_minutes(-18 * 60).is_ok());
        let err = offset_from_minutes(18 * 60 + 1).unwrap_err().to_string();
        assert!(err.contains("utc_offset_minutes"), "{err}");
    }

    #[test]
    fn months_bucket_at_the_offset() {
        let at = dt("2024-03-31T23:30:00Z");
        assert_eq!(month_of(at, &off(0)), "2024-03");
        assert_eq!(month_of(at, &off(60)), "2024-04");
        assert_eq!(month_of(dt("2024-01-01T00:30:00Z"), &off(-60)), "2023-12");
        let counts = month_counts(
            [
                dt("2024-05-01T00:00:00Z"),
                dt("2023-12-01T00:00:00Z"),
                dt("2024-05-20T00:00:00Z"),
            ],
            &off(0),
        );
        assert_eq!(
            counts,
            vec![
                FacetCount {
                    value: "2023-12".into(),
                    count: 1
                },
                FacetCount {
                    value: "2024-05".into(),
                    count: 2
                },
            ]
        );
    }

    fn cand(source: &str, kind: ItemKind, people: &[&str], at: OffsetDateTime) -> Candidate {
        Candidate {
            source: source.into(),
            kind,
            people: people.iter().map(|p| (*p).to_owned()).collect(),
            at,
        }
    }

    fn counts(v: &[FacetCount]) -> Vec<(&str, u32)> {
        v.iter().map(|c| (c.value.as_str(), c.count)).collect()
    }

    fn pool() -> Vec<Candidate> {
        vec![
            cand(
                "mail",
                ItemKind::Mail,
                &["sam", "acme"],
                dt("2024-03-02T10:00:00Z"),
            ),
            cand("mail", ItemKind::Mail, &["sam"], dt("2024-04-02T10:00:00Z")),
            cand(
                "cal",
                ItemKind::Event,
                &["sam", "sam"],
                dt("2024-03-10T10:00:00Z"),
            ),
            cand(
                "files",
                ItemKind::File,
                &["acme"],
                dt("2023-11-02T10:00:00Z"),
            ),
            cand("files", ItemKind::File, &[], dt("2024-03-31T23:30:00Z")),
        ]
    }

    #[test]
    fn facets_without_filters_count_everything() {
        let (hits, t) = facet(&pool(), &FacetFilters::default(), &off(0));
        assert_eq!(hits, [0, 1, 2, 3, 4]);
        assert_eq!(counts(&t.sources), [("files", 2), ("mail", 2), ("cal", 1)]);
        assert_eq!(counts(&t.kinds), [("file", 2), ("mail", 2), ("event", 1)]);
        // A repeated id in one item counts once.
        assert_eq!(counts(&t.people), [("sam", 3), ("acme", 2)]);
        assert_eq!(
            counts(&t.months),
            [("2023-11", 1), ("2024-03", 3), ("2024-04", 1)]
        );
        // At +01:00 the late-March file moves to April.
        let (_, t) = facet(&pool(), &FacetFilters::default(), &off(60));
        assert_eq!(
            counts(&t.months),
            [("2023-11", 1), ("2024-03", 2), ("2024-04", 2)]
        );
    }

    #[test]
    fn facet_counts_are_disjunctive() {
        let f = FacetFilters {
            kinds: [ItemKind::Mail].into(),
            people: ["acme".to_owned()].into(),
            ..Default::default()
        };
        let (hits, t) = facet(&pool(), &f, &off(0));
        assert_eq!(hits, [0]);
        // Kinds ignore the kind filter but apply people: items mentioning acme.
        assert_eq!(counts(&t.kinds), [("file", 1), ("mail", 1)]);
        // People ignore the people filter but apply kinds: the two mails.
        assert_eq!(counts(&t.people), [("sam", 2), ("acme", 1)]);
        // Sources and months apply both.
        assert_eq!(counts(&t.sources), [("mail", 1)]);
        assert_eq!(counts(&t.months), [("2024-03", 1)]);

        // Values within a filter are alternatives.
        let f = FacetFilters {
            sources: ["mail".to_owned(), "cal".to_owned()].into(),
            ..Default::default()
        };
        let (hits, t) = facet(&pool(), &f, &off(0));
        assert_eq!(hits, [0, 1, 2]);
        assert_eq!(counts(&t.sources), [("files", 2), ("mail", 2), ("cal", 1)]);
        assert_eq!(counts(&t.kinds), [("mail", 2), ("event", 1)]);
    }

    #[test]
    fn date_filter_is_its_own_facet() {
        let f = FacetFilters {
            range: day_range("", Some("2024-03-01"), Some("2024-03-31"), &off(0)).unwrap(),
            ..Default::default()
        };
        let (hits, t) = facet(&pool(), &f, &off(0));
        assert_eq!(hits, [0, 2, 4]);
        // Months ignore the date range (so the client can widen it).
        assert_eq!(
            counts(&t.months),
            [("2023-11", 1), ("2024-03", 3), ("2024-04", 1)]
        );
        assert_eq!(counts(&t.kinds), [("event", 1), ("file", 1), ("mail", 1)]);
        // At +01:00 the March range ends earlier and excludes the late file.
        let f = FacetFilters {
            range: day_range("", Some("2024-03-01"), Some("2024-03-31"), &off(60)).unwrap(),
            ..Default::default()
        };
        assert_eq!(facet(&pool(), &f, &off(60)).0, [0, 2]);
    }

    #[test]
    fn selected_values_always_appear() {
        let c = vec![FacetCount {
            value: "mail".into(),
            count: 2,
        }];
        let out = with_selected(c, &["mail".into(), "slack".into()]);
        assert_eq!(counts(&out), [("mail", 2), ("slack", 0)]);
    }

    #[test]
    fn people_facet_tops_names_and_keeps_selections() {
        let tally = vec![
            FacetCount {
                value: "ent_a".into(),
                count: 5,
            },
            FacetCount {
                value: "ent_b".into(),
                count: 3,
            },
            FacetCount {
                value: "ent_c".into(),
                count: 1,
            },
        ];
        let names: HashMap<String, (String, String)> =
            [("ent_a", "Ann"), ("ent_b", "Bob"), ("ent_c", "Cy")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), (v.to_owned(), "person".to_owned())))
                .collect();
        let selected = vec![
            // A merged-away id resolving to ent_b: shown under the given id, once.
            SelectedPerson {
                given: "ent_old".into(),
                live: Some("ent_b".into()),
            },
            // Below the top: appended with its count.
            SelectedPerson {
                given: "ent_c".into(),
                live: Some("ent_c".into()),
            },
            // Unknown: count 0, its id as name.
            SelectedPerson {
                given: "ent_zz".into(),
                live: None,
            },
        ];
        let out = people_facet(&tally, &selected, &names, 2);
        let got: Vec<(&str, &str, &str, u32)> = out
            .iter()
            .map(|p| (p.value.as_str(), p.name.as_str(), p.kind.as_str(), p.count))
            .collect();
        assert_eq!(
            got,
            [
                ("ent_a", "Ann", "person", 5),
                ("ent_old", "Bob", "person", 3),
                ("ent_c", "Cy", "person", 1),
                ("ent_zz", "ent_zz", "unknown", 0),
            ]
        );
    }

    fn ev(at: OffsetDateTime, is_fact: bool, tie: &str) -> Dated<String> {
        Dated {
            at,
            is_fact,
            tie: tie.into(),
            payload: tie.into(),
        }
    }

    fn names(c: &TimelineCut<String>) -> Vec<&str> {
        c.kept.iter().map(|e| e.payload.as_str()).collect()
    }

    #[test]
    fn timeline_sorts_ties_and_truncates_to_the_newest() {
        let t0 = dt("2024-03-02T10:00:00Z");
        let events = vec![
            ev(dt("2024-05-01T00:00:00Z"), false, "may"),
            ev(t0, true, "fact b"),
            ev(t0, false, "item z"),
            ev(dt("2023-01-01T00:00:00Z"), false, "old"),
            ev(t0, true, "fact a"),
            ev(t0, false, "item a"),
        ];
        let all = assemble(events.clone(), &DayRange::default(), 10, &off(0));
        assert_eq!(
            names(&all),
            ["old", "item a", "item z", "fact a", "fact b", "may"]
        );
        assert_eq!(all.total, 6);
        assert!(!all.truncated);
        assert_eq!(
            counts(&all.months),
            [("2023-01", 1), ("2024-03", 4), ("2024-05", 1)]
        );

        let cut = assemble(events.clone(), &DayRange::default(), 2, &off(0));
        assert_eq!(names(&cut), ["fact b", "may"]);
        assert_eq!(cut.total, 6);
        assert!(cut.truncated);
        // Months cover every matching event, not just the kept ones.
        assert_eq!(cut.months, all.months);

        // A range narrows before the limit (how the client jumps back in time).
        let range = day_range("", None, Some("2024-03-02"), &off(0)).unwrap();
        let back = assemble(events, &range, 2, &off(0));
        assert_eq!(names(&back), ["fact a", "fact b"]);
        assert_eq!(back.total, 5);
        assert!(back.truncated);
        assert_eq!(counts(&back.months), [("2023-01", 1), ("2024-03", 4)]);

        let empty = assemble(
            Vec::<Dated<String>>::new(),
            &DayRange::default(),
            5,
            &off(0),
        );
        assert!(empty.kept.is_empty() && empty.months.is_empty() && !empty.truncated);
    }

    #[test]
    fn named_zones_use_each_dates_own_offset() {
        let berlin = Zone::named("Europe/Berlin").unwrap();
        assert_eq!(Zone::named("Mars/Olympus"), None);
        assert_eq!(Zone::named(""), None);
        assert_eq!(Zone::named(&"A".repeat(200)), None);
        // March 2024 in Berlin: CET (+1) until the 31st, CEST (+2) from then on. A fixed
        // offset (today's) gets one end wrong; the zone gets both right.
        let r = day_range("", Some("2024-03-01"), Some("2024-03-31"), &berlin).unwrap();
        assert_eq!(r.start, Some(dt("2024-02-29T23:00:00Z")));
        assert_eq!(r.end, Some(dt("2024-03-31T22:00:00Z")));
        let fixed = day_range("", Some("2024-03-01"), Some("2024-03-31"), &off(120)).unwrap();
        assert_ne!(fixed.start, r.start);
        // 23:30 UTC on 31 March is already April in Berlin (CEST); 23:30 UTC on 31 January is
        // 00:30 on 1 February (CET).
        assert_eq!(month_of(dt("2024-03-31T22:30:00Z"), &berlin), "2024-04");
        assert_eq!(month_of(dt("2024-03-31T21:30:00Z"), &berlin), "2024-03");
        assert_eq!(month_of(dt("2024-01-31T23:30:00Z"), &berlin), "2024-02");
        assert_eq!(month_of(dt("2024-01-31T22:30:00Z"), &berlin), "2024-01");
        // A floating date is read at that date's offset.
        let u = "2024-08-01T00:00:00Z";
        assert_eq!(
            own_date(&dates(Some("2024-01-15"), None, None, u), &berlin).0,
            dt("2024-01-14T23:00:00Z")
        );
        assert_eq!(
            own_date(&dates(Some("2024-07-15"), None, None, u), &berlin).0,
            dt("2024-07-14T22:00:00Z")
        );
    }

    #[test]
    fn floating_dates_east_of_utc_keep_their_day_on_the_changeover() {
        // Sydney goes to summer time at 02:00 local on 6 October 2024 (15:00 UTC on the 5th):
        // an item dated that day starts at local midnight, +10, not at the later +11.
        let sydney = Zone::named("Australia/Sydney").unwrap();
        let u = "2024-12-01T00:00:00Z";
        let (at, _) = own_date(&dates(Some("2024-10-06"), None, None, u), &sydney);
        assert_eq!(at, dt("2024-10-05T14:00:00Z"));
        let r = day_range("", Some("2024-10-06"), Some("2024-10-06"), &sydney).unwrap();
        assert!(r.contains(at));
        assert_eq!(month_of(at, &sydney), "2024-10");
    }

    #[test]
    fn a_day_without_midnight_starts_when_it_starts() {
        // Havana skips midnight on its spring change (2024-03-10: 00:00 becomes 01:00, -5 to
        // -4): the day starts at 01:00 local, 05:00 UTC.
        let havana = Zone::named("America/Havana").unwrap();
        assert_eq!(
            havana.midnight(Date::from_calendar_date(2024, Month::March, 10).unwrap()),
            dt("2024-03-10T05:00:00Z")
        );
        assert_eq!(
            havana.midnight(Date::from_calendar_date(2024, Month::March, 11).unwrap()),
            dt("2024-03-11T04:00:00Z")
        );
    }

    #[test]
    fn text_cuts_respect_char_boundaries() {
        assert_eq!(cut_chars("héllo", 2), ("hé", true));
        assert_eq!(cut_chars("héllo", 5), ("héllo", false));
        assert_eq!(cut_chars("", 0), ("", false));
        assert_eq!(snippet("  a\n\n b  c ", 240), "a b c");
        assert_eq!(snippet("one two three", 7), "one two…");
        assert_eq!(snippet("ééééé", 3), "ééé…");
        assert_eq!(snippet("", 10), "");
    }
}
