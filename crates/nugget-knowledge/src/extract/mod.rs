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

/// When the item's content was written: a mail's `Date`, else the source's modification time,
/// else when it was stored.
pub(crate) fn item_date(item: &Item) -> OffsetDateTime {
    item.metadata
        .get("extracted")
        .and_then(|e| e.get("date"))
        .and_then(Value::as_str)
        .and_then(parse_when)
        .or(item.source_modified_at)
        .unwrap_or(item.updated_at)
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
    use super::parse_when;

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
