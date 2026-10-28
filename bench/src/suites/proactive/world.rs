//! The fixture world: one fictional user's mail, calendar, notes and receipts over 30 days, and
//! the gold labels that say which cards a good engine shows on which days.
//!
//! The world lives in `bench/fixtures/proactive/world.json` and the labels in `gold.json`. Both
//! are produced by [`super::generate`] and committed, so they can be read and reviewed; a test
//! fails if the committed files and the generator drift apart (set `NUGGET_BENCH_REGEN=1` to
//! rewrite them). Engines only ever see [`World`]; [`Gold`] is for scoring.
//!
//! Days are integers: day 1 is [`START_DATE`] and the benchmark runs days `1..=DAYS`. Items with a
//! day of 0 or less are history the engine has already seen before day 1. Every address uses
//! `example.com` (the user's employer) or a `.test` domain.

use serde::{Deserialize, Serialize};

/// Calendar date of day 1.
pub const START_DATE: (i32, u8, u8) = (2026, 9, 1);
/// Days simulated.
pub const DAYS: i32 = 30;

pub const WORLD_JSON: &str = include_str!("../../../fixtures/proactive/world.json");
pub const GOLD_JSON: &str = include_str!("../../../fixtures/proactive/gold.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    pub user: Person,
    pub start_date: String,
    pub days: i32,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// Something that arrives in the user's world on a given day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Mail(Mail),
    Event(Event),
    EventCancelled(EventCancelled),
    Note(Note),
    Receipt(Receipt),
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::Mail(m) => &m.id,
            Item::Event(e) => &e.id,
            Item::EventCancelled(c) => &c.id,
            Item::Note(n) => &n.id,
            Item::Receipt(r) => &r.id,
        }
    }

    /// The day the item becomes visible.
    pub fn day(&self) -> i32 {
        match self {
            Item::Mail(m) => m.day,
            Item::Event(e) => e.day,
            Item::EventCancelled(c) => c.day,
            Item::Note(n) => n.day,
            Item::Receipt(r) => r.day,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mail {
    pub id: String,
    pub day: i32,
    pub thread: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    /// The message carries a `List-Unsubscribe` header.
    #[serde(default, skip_serializing_if = "is_false")]
    pub list_unsubscribe: bool,
    /// The message carries `Auto-Submitted: auto-generated`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_submitted: bool,
}

/// A calendar event as the invite was received on `day`. A later [`EventCancelled`] may cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub day: i32,
    pub start_day: i32,
    pub start_hour: u8,
    pub minutes: u16,
    pub title: String,
    pub organizer: String,
    /// Everyone invited, the user included. Empty for a personal block.
    pub attendees: Vec<String>,
    /// Recurring series id, for one occurrence of a recurring event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    pub location: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCancelled {
    pub id: String,
    pub day: i32,
    pub event: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub id: String,
    pub day: i32,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Monthly,
    Annual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub id: String,
    pub day: i32,
    pub merchant: String,
    pub merchant_domain: String,
    pub amount_cents: u32,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<Cadence>,
    /// For a subscription: the day it renews next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renews_on: Option<i32>,
}

/// What a card is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// Someone asked the user something and is still waiting. Subject: `thread:<id>`.
    UnansweredThread,
    /// The user asked someone something and got no answer. Subject: `thread:<id>`.
    FollowUp,
    /// The user promised something by a date. Subject: `commit:<mail or note id>`.
    CommitmentDue,
    /// A meeting worth preparing for. Subject: `event:<id>`.
    MeetingPrep,
    /// A subscription about to renew. Subject: `sub:<merchant domain>`.
    Renewal,
}

impl CardKind {
    pub const ALL: [CardKind; 5] = [
        CardKind::UnansweredThread,
        CardKind::FollowUp,
        CardKind::CommitmentDue,
        CardKind::MeetingPrep,
        CardKind::Renewal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CardKind::UnansweredThread => "unanswered_thread",
            CardKind::FollowUp => "follow_up",
            CardKind::CommitmentDue => "commitment_due",
            CardKind::MeetingPrep => "meeting_prep",
            CardKind::Renewal => "renewal",
        }
    }
}

pub fn thread_subject(thread: &str) -> String {
    format!("thread:{thread}")
}
pub fn event_subject(event: &str) -> String {
    format!("event:{event}")
}
pub fn commit_subject(item: &str) -> String {
    format!("commit:{item}")
}
pub fn sub_subject(domain: &str) -> String {
    format!("sub:{domain}")
}

/// Scoring labels. Engines never see these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gold {
    /// Cards a good engine shows, each on some day in `first_day..=last_day`.
    pub cards: Vec<GoldCard>,
    /// Items that must never produce a card. Any shown card whose subject is in `subjects` is a
    /// false positive on that distractor.
    pub distractors: Vec<Distractor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldCard {
    pub id: String,
    pub kind: CardKind,
    pub subject: String,
    pub first_day: i32,
    pub last_day: i32,
    /// The item that gives rise to the card.
    pub source: String,
    pub why: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistractorCategory {
    /// A newsletter with a `List-Unsubscribe` header.
    Newsletter,
    /// An automated notification (`Auto-Submitted`, or a no-reply style sender).
    Notification,
    /// Bulk mail that looks personal: no list or auto headers, a normal sender, often a question.
    BulkUnflagged,
    /// A question the user already answered, or a request answered by a reply.
    AnsweredThread,
    /// A commitment the user already delivered.
    CompletedCommitment,
    /// A meeting cancelled before it would need preparing.
    CancelledMeeting,
    /// A renewal or charge notice for a subscription the user never bought.
    SpamRenewal,
}

impl DistractorCategory {
    pub const ALL: [DistractorCategory; 7] = [
        DistractorCategory::Newsletter,
        DistractorCategory::Notification,
        DistractorCategory::BulkUnflagged,
        DistractorCategory::AnsweredThread,
        DistractorCategory::CompletedCommitment,
        DistractorCategory::CancelledMeeting,
        DistractorCategory::SpamRenewal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DistractorCategory::Newsletter => "newsletter",
            DistractorCategory::Notification => "notification",
            DistractorCategory::BulkUnflagged => "bulk_unflagged",
            DistractorCategory::AnsweredThread => "answered_thread",
            DistractorCategory::CompletedCommitment => "completed_commitment",
            DistractorCategory::CancelledMeeting => "cancelled_meeting",
            DistractorCategory::SpamRenewal => "spam_renewal",
        }
    }

    /// Flagged by headers or sender: no engine should need feedback to drop these.
    pub fn header_flagged(self) -> bool {
        matches!(
            self,
            DistractorCategory::Newsletter | DistractorCategory::Notification
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distractor {
    pub item: String,
    pub category: DistractorCategory,
    pub subjects: Vec<String>,
}

/// The committed fixture world.
pub fn load_world() -> Result<World, serde_json::Error> {
    serde_json::from_str(WORLD_JSON)
}

/// The committed gold labels.
pub fn load_gold() -> Result<Gold, serde_json::Error> {
    serde_json::from_str(GOLD_JSON)
}

/// `YYYY-MM-DD` for a day number.
pub fn iso_date(day: i32) -> String {
    let d = calendar_date(day);
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// `Fri 18 Sep` for a day number.
pub fn pretty_date(day: i32) -> String {
    let d = calendar_date(day);
    let wd = &d.weekday().to_string()[..3];
    let mon = &d.month().to_string()[..3];
    format!("{wd} {} {mon}", d.day())
}

/// Parse a `YYYY-MM-DD` date back to a day number.
pub fn day_of(iso: &str) -> Option<i32> {
    let mut parts = iso.splitn(3, '-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next()?.parse().ok()?;
    let date = time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()?;
    Some((date - start()).whole_days() as i32 + 1)
}

/// Monday is 0. Day 1 (2026-09-01) is a Tuesday.
pub fn weekday(day: i32) -> u8 {
    calendar_date(day).weekday().number_days_from_monday()
}

fn start() -> time::Date {
    let (y, m, d) = START_DATE;
    time::Date::from_calendar_date(y, time::Month::try_from(m).expect("month"), d)
        .expect("start date")
}

fn calendar_date(day: i32) -> time::Date {
    start() + time::Duration::days(i64::from(day) - 1)
}

/// Render a value as JSON with each element of the named top-level arrays on its own line, so
/// the fixture files diff line by line.
pub fn to_fixture_json<T: Serialize>(value: &T, arrays: &[&str]) -> String {
    let v = serde_json::to_value(value).expect("serializable");
    let obj = v.as_object().expect("object");
    let mut out = String::from("{\n");
    let n = obj.len();
    for (i, (k, val)) in obj.iter().enumerate() {
        out.push_str(&format!("  {}: ", serde_json::to_string(k).expect("key")));
        match val.as_array() {
            Some(items) if arrays.contains(&k.as_str()) => {
                out.push_str("[\n");
                for (j, item) in items.iter().enumerate() {
                    out.push_str("    ");
                    out.push_str(&serde_json::to_string(item).expect("item"));
                    out.push_str(if j + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str("  ]");
            }
            _ => out.push_str(&serde_json::to_string(val).expect("value")),
        }
        out.push_str(if i + 1 < n { ",\n" } else { "\n" });
    }
    out.push_str("}\n");
    out
}
