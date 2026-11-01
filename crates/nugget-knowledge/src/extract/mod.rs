//! Deterministic extractors: always on, no model. Each reads the item's metadata as ingest
//! stores it (`extracted` format metadata, `tags`, connector `source` metadata) plus its chunks
//! for evidence locations.

mod calendar;
mod contacts;
mod mail;
mod markdown;

use std::collections::HashSet;

use nugget_graph::Evidence;
use nugget_store::{Chunk, Item};
use serde_json::Value;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::config::KnowledgeConfig;
use crate::spec::Extraction;
use crate::text::{MAX_QUOTE_CHARS, truncate_chars};

/// Everything an extractor sees about one item.
pub(crate) struct ItemCtx<'a> {
    pub item: &'a Item,
    pub chunks: &'a [Chunk],
    /// Lower-cased addresses known to be the user's.
    pub me_emails: &'a HashSet<String>,
    /// How claim texts name the user.
    pub me_label: &'a str,
    pub cfg: &'a KnowledgeConfig,
    /// When the item is processed (`Knowledge::set_clock`, else the wall clock): an event that
    /// has not ended by then was not attended yet.
    pub now: OffsetDateTime,
}

impl ItemCtx<'_> {
    pub fn meta(&self, key: &str) -> Option<&Value> {
        self.item.metadata.get(key)
    }

    /// `metadata.extracted.<key>`.
    pub fn extracted(&self, key: &str) -> Option<&Value> {
        self.meta("extracted").and_then(|e| e.get(key))
    }

    /// `metadata.source.<key>` (connector metadata).
    pub fn source_meta(&self, key: &str) -> Option<&Value> {
        self.meta("source").and_then(|e| e.get(key))
    }

    pub fn mime(&self) -> String {
        self.item
            .mime
            .clone()
            .or_else(|| self.meta("mime").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    pub fn evidence(&self, chunk_id: Option<i64>, quote: &str) -> Evidence {
        let q = quote.trim();
        Evidence {
            item: self.item.item.clone(),
            chunk_id,
            quote: (!q.is_empty()).then(|| truncate_chars(q, MAX_QUOTE_CHARS)),
        }
    }

    /// Evidence at the first chunk mentioning `needle`, else a synthesised quote (e.g. a header
    /// line, which is part of the source but not of its chunks).
    pub fn evidence_at(&self, needle: &str, fallback_quote: &str) -> Evidence {
        match crate::text::locate(self.chunks, needle) {
            Some((id, line)) => self.evidence(Some(id), &line),
            None => self.evidence(None, fallback_quote),
        }
    }

    pub fn is_me(&self, email: &str, extra: &[String]) -> bool {
        let e = email.to_lowercase();
        self.me_emails.contains(&e) || extra.contains(&e)
    }
}

/// Run every deterministic extractor that applies to the item.
pub(crate) fn deterministic(ctx: &ItemCtx<'_>) -> Extraction {
    let mut ex = Extraction::default();
    if mail::applies(ctx) {
        mail::extract(ctx, &mut ex);
    }
    calendar::extract(ctx, &mut ex);
    if markdown::applies(ctx) {
        markdown::extract(ctx, &mut ex);
    }
    contacts::extract(ctx, &mut ex);
    ex
}

/// When an item's content is from: the date its facts are as of. The one rule for "when is
/// this item from", used for currentness (claims' `valid_from`, which value of a single-valued
/// predicate is current) by the extractors, the session and its date cache (through
/// [`DateSources::date`] on the store's date projection, see `feed.rs`); retrieval may share it.
///
/// The first of, in order:
/// 1. a mail's `Date` header (`metadata.extracted.date`);
/// 2. a Markdown note's frontmatter `date:`, then `created:`;
/// 3. a calendar item's event start (`DTSTART`; a recurring event's first occurrence);
/// 4. a `YYYY-MM-DD` date in the file name (`Notes/2026-03-14 Standup.md`), then in the title;
/// 5. the source's modification time (a file's mtime);
/// 6. when the store last wrote the item.
///
/// So a copied or restored note (mtime = now) keeps its own date. Dates are not clamped here:
/// callers clamp far-future dates (a forged mail `Date`, an upcoming event) to their horizon
/// (`Session::clamp`).
pub(crate) fn item_date(item: &Item) -> OffsetDateTime {
    DateSources::of(item).date()
}

/// What an item's date is read from (see [`item_date`]): built from a loaded [`Item`]
/// ([`DateSources::of`]) or from the store's date projection (`nugget_store::ItemDates`), so
/// both paths date an item the same way.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DateSources<'a> {
    /// `metadata.extracted.date`: a mail's `Date` header.
    pub mail_date: Option<&'a str>,
    /// `metadata.extracted.frontmatter.date`.
    pub frontmatter_date: Option<&'a str>,
    /// `metadata.extracted.frontmatter.created`.
    pub frontmatter_created: Option<&'a str>,
    /// `metadata.extracted.start` (an .ics file's first event's `DTSTART`), else
    /// `metadata.source.start.date_time` / `.date` (a Google Calendar event).
    pub event_start: Option<&'a str>,
    /// The item key (a file's path).
    pub key: &'a str,
    pub title: Option<&'a str>,
    /// The source's modification time.
    pub modified: Option<OffsetDateTime>,
    /// When the store last wrote the item.
    pub stored: OffsetDateTime,
}

impl<'a> DateSources<'a> {
    pub fn of(item: &'a Item) -> Self {
        let text = |v: Option<&'a Value>| v.and_then(Value::as_str);
        let ex = item.metadata.get("extracted");
        let fm = ex.and_then(|e| e.get("frontmatter"));
        let gcal = item.metadata.get("source").and_then(|s| s.get("start"));
        Self {
            mail_date: text(ex.and_then(|e| e.get("date"))),
            frontmatter_date: text(fm.and_then(|f| f.get("date"))),
            frontmatter_created: text(fm.and_then(|f| f.get("created"))),
            event_start: text(ex.and_then(|e| e.get("start")))
                .or_else(|| text(gcal.and_then(|s| s.get("date_time"))))
                .or_else(|| text(gcal.and_then(|s| s.get("date")))),
            key: item.item.key.as_str(),
            title: item.title.as_deref(),
            modified: item.source_modified_at,
            stored: item.updated_at,
        }
    }

    /// The item's date ([`item_date`]).
    pub fn date(&self) -> OffsetDateTime {
        [
            self.mail_date,
            self.frontmatter_date,
            self.frontmatter_created,
            self.event_start,
        ]
        .into_iter()
        .flatten()
        .find_map(parse_when)
        .or_else(|| name_date(file_leaf(self.key)))
        .or_else(|| self.title.and_then(name_date))
        .or(self.modified)
        .unwrap_or(self.stored)
    }
}

/// The last path segment of an item key.
fn file_leaf(key: &str) -> &str {
    key.rsplit(['/', '\\']).next().unwrap_or(key)
}

/// The first `YYYY-MM-DD` (or `YYYY_MM_DD`) date in a name that is not part of a longer number,
/// with a year from 1900 to 2199.
fn name_date(name: &str) -> Option<OffsetDateTime> {
    let b = name.as_bytes();
    let digits = |from: usize, n: usize| {
        b.get(from..from + n)
            .is_some_and(|x| x.iter().all(u8::is_ascii_digit))
    };
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let sep = b[i + 4];
        let ok = (sep == b'-' || sep == b'_')
            && b[i + 7] == sep
            && digits(i, 4)
            && digits(i + 5, 2)
            && digits(i + 8, 2)
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && b.get(i + 10).is_none_or(|c| !c.is_ascii_digit())
            && matches!(&b[i..i + 2], b"19" | b"20" | b"21");
        if !ok {
            return None;
        }
        let d = std::str::from_utf8(&b[i..i + 10]).ok()?.replace('_', "-");
        parse_when(&d)
    })
}

/// Parse an RFC 3339 timestamp, a floating `YYYY-MM-DDTHH:MM[:SS]` (taken as UTC) or a date.
pub(crate) fn parse_when(s: &str) -> Option<OffsetDateTime> {
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return Some(t);
    }
    let num = |a: &str| a.parse::<u32>().ok();
    let (date_part, time_part) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let compact = !date_part.contains('-') && date_part.len() == 8;
    let (y, m, d) = if compact {
        (
            num(&date_part[..4])?,
            num(&date_part[4..6])?,
            num(&date_part[6..8])?,
        )
    } else {
        let mut it = date_part.split('-');
        (num(it.next()?)?, num(it.next()?)?, num(it.next()?)?)
    };
    let date = Date::from_calendar_date(
        i32::try_from(y).ok()?,
        Month::try_from(u8::try_from(m).ok()?).ok()?,
        u8::try_from(d).ok()?,
    )
    .ok()?;
    let time = match time_part {
        None => Time::MIDNIGHT,
        Some(t) => {
            let t = t.trim_end_matches('Z');
            let digits: String = t.chars().filter(char::is_ascii_digit).take(6).collect();
            let hh = digits.get(0..2).and_then(num).unwrap_or(0);
            let mm = digits.get(2..4).and_then(num).unwrap_or(0);
            let ss = digits.get(4..6).and_then(num).unwrap_or(0);
            Time::from_hms(
                u8::try_from(hh).ok()?,
                u8::try_from(mm).ok()?,
                u8::try_from(ss).ok()?,
            )
            .ok()?
        }
    };
    Some(PrimitiveDateTime::new(date, time).assume_utc())
}

/// Strings of a JSON value: a string, or the strings of an array.
pub(crate) fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::Number(n)) => vec![n.to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_store::Item;
    use serde_json::{Value, json};

    use super::{item_date, name_date, parse_when};

    fn item(key: &str, title: Option<&str>, metadata: Value) -> Item {
        let mtime = parse_when("2026-10-30T19:00:00Z").unwrap();
        Item {
            item: ItemRef {
                source: SourceId::new("files"),
                key: ItemKey::new(key),
            },
            title: title.map(str::to_owned),
            content_hash: "h".into(),
            mime: Some("text/markdown".into()),
            text: None,
            metadata,
            source_modified_at: Some(mtime),
            version: None,
            created_at: mtime,
            updated_at: mtime,
        }
    }

    fn day(s: &str) -> time::OffsetDateTime {
        parse_when(s).unwrap()
    }

    #[test]
    fn an_items_own_date_beats_its_mtime() {
        let mtime = day("2026-10-30T19:00:00Z");
        let fm = |v: Value| json!({"extracted": {"frontmatter": v}});
        let cases = [
            // A mail's Date header first.
            (
                item(
                    "Mail/x.eml",
                    None,
                    json!({"extracted": {"date": "2026-08-14T09:00:00Z",
                                         "frontmatter": {"date": "2026-01-01"}}}),
                ),
                day("2026-08-14T09:00:00Z"),
            ),
            // Frontmatter `date:`, then `created:`, over a date in the file name.
            (
                item(
                    "Notes/2026-05-05 Dana.md",
                    None,
                    fm(json!({"date": "2026-03-01", "created": "2026-02-01"})),
                ),
                day("2026-03-01"),
            ),
            (
                item(
                    "Notes/Dana.md",
                    None,
                    fm(json!({"created": "2026-02-01T08:00"})),
                ),
                day("2026-02-01T08:00:00Z"),
            ),
            // A calendar item's event start.
            (
                item(
                    "Calendar/launch.ics",
                    None,
                    json!({"extracted": {"start": "2026-12-03T09:00:00Z", "events": []}}),
                ),
                day("2026-12-03T09:00:00Z"),
            ),
            // A Google Calendar event's start.
            (
                item(
                    "evt1",
                    None,
                    json!({"source": {"start": {"date": "2026-04-16"}}}),
                ),
                day("2026-04-16"),
            ),
            // The file name, then the title.
            (
                item(
                    "Notes/Meetings/2026-06-03-1on1-with-Bob.md",
                    None,
                    json!({}),
                ),
                day("2026-06-03"),
            ),
            (
                item("Notes/standup.md", Some("Standup 2026_03_14"), json!({})),
                day("2026-03-14"),
            ),
            // A non-string or unparsable date falls through to the mtime.
            (
                item("Notes/Dana.md", None, fm(json!({"date": "someday"}))),
                mtime,
            ),
        ];
        for (it, want) in cases {
            assert_eq!(item_date(&it), want, "{}", it.item.key.as_str());
        }
    }

    #[test]
    fn name_dates_are_delimited() {
        assert_eq!(name_date("2026-03-14 Standup"), Some(day("2026-03-14")));
        assert_eq!(name_date("notes-2026-03-14.md"), Some(day("2026-03-14")));
        assert_eq!(name_date("Standup 2026_03_14"), Some(day("2026-03-14")));
        assert_eq!(name_date("12026-03-14"), None);
        assert_eq!(name_date("2026-03-145"), None);
        assert_eq!(name_date("2026-13-01"), None);
        assert_eq!(name_date("2026-03_14"), None);
        assert_eq!(name_date("v1234-56-78"), None);
        assert_eq!(name_date("Dana (Brightwell)"), None);
        assert_eq!(name_date("é2026-03-14"), Some(day("2026-03-14")));
    }

    #[test]
    fn dates_parse() {
        assert!(parse_when("2026-03-01T10:00:00Z").is_some());
        assert!(parse_when("2026-03-01T10:00:00+02:00").is_some());
        assert_eq!(
            parse_when("2026-03-01T10:30:00").unwrap(),
            parse_when("2026-03-01T10:30:00Z").unwrap()
        );
        assert!(parse_when("2026-03-01").is_some());
        assert!(parse_when("20260301T103000").is_some());
        assert!(parse_when("nonsense").is_none());
    }
}
