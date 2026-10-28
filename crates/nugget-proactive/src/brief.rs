//! The morning brief: top cards, today's meetings with prep, overdue commitments, renewals.

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, OffsetDateTime};

use crate::candidate::{CandidateKind, EvidenceRef};
use crate::cards::{Card, CardState, ProactiveState};
use crate::config::ProactiveConfig;
use crate::corpus::Corpus;
use crate::score::{Factor, shown};

/// How many cards the brief leads with.
pub const BRIEF_TOP: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefCard {
    pub card_id: String,
    pub kind: CandidateKind,
    pub title: String,
    /// The judge's summary when available, else the card's detail.
    pub summary: String,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub due: Option<OffsetDateTime>,
    pub rank: f64,
    /// Why it ranks here.
    pub factors: Vec<Factor>,
    /// Offered action names, best first.
    pub actions: Vec<String>,
}

impl BriefCard {
    fn of(c: &Card) -> Self {
        Self {
            card_id: c.id.clone(),
            kind: c.kind,
            title: c.title.clone(),
            summary: c
                .enrichment
                .as_ref()
                .map_or_else(|| c.detail.clone(), |e| e.summary.clone()),
            due: c.due,
            rank: c.rank,
            factors: c.factors.clone(),
            actions: c.actions.iter().map(|a| a.name().to_owned()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefMeeting {
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub end: Option<OffsetDateTime>,
    pub all_day: bool,
    pub location: Option<String>,
    pub attendees: Vec<String>,
    /// Attendees outside the user's organisation.
    pub external: Vec<String>,
    /// The meeting-prep card, when one exists.
    pub prep_card: Option<String>,
    /// Related threads and notes.
    pub prep: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MorningBrief {
    /// The user's local date.
    pub date: Date,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// Highest-ranked open cards at or above `min_score` (meetings and overdue items have their
    /// own sections).
    pub top: Vec<BriefCard>,
    pub meetings: Vec<BriefMeeting>,
    /// Commitments and promises past due.
    pub overdue: Vec<BriefCard>,
    /// Renewals and trial ends in the next 7 days.
    pub renewals: Vec<BriefCard>,
    /// Open cards in the inbox.
    pub open_cards: usize,
}

fn open(c: &Card) -> bool {
    !c.state.is_terminal() && c.state != CardState::Snoozed
}

pub fn compose(
    state: &ProactiveState,
    corpus: &Corpus,
    cfg: &ProactiveConfig,
    now: OffsetDateTime,
) -> MorningBrief {
    let local_now = now.to_offset(cfg.utc_offset);
    let today = local_now.date();
    let mut cards: Vec<&Card> = state.cards.values().filter(|c| open(c)).collect();
    cards.sort_by(|a, b| b.rank.total_cmp(&a.rank).then_with(|| a.id.cmp(&b.id)));

    let mut meetings = Vec::new();
    for ev in &corpus.events {
        if ev.cancelled || ev.start.to_offset(cfg.utc_offset).date() != today {
            continue;
        }
        let prep_card = cards.iter().find(|c| {
            c.kind == CandidateKind::MeetingPrep
                && c.evidence.first().is_some_and(|e| e.item == ev.item)
        });
        meetings.push(BriefMeeting {
            title: ev.title.clone(),
            start: ev.start,
            end: ev.end,
            all_day: ev.all_day,
            location: ev.location.clone(),
            attendees: ev
                .attendees
                .iter()
                .filter(|a| !a.resource && !corpus.is_user(&a.address.email))
                .map(|a| a.address.display())
                .collect(),
            external: ev
                .attendees
                .iter()
                .filter(|a| !a.resource && corpus.is_external(&a.address))
                .map(|a| a.address.email.clone())
                .collect(),
            prep_card: prep_card.map(|c| c.id.clone()),
            prep: prep_card
                .map(|c| c.evidence[1..].to_vec())
                .unwrap_or_default(),
        });
    }

    let is_overdue = |c: &Card| {
        matches!(c.kind, CandidateKind::Commitment | CandidateKind::FollowUp)
            && c.due.is_some_and(|d| d < now)
    };
    let overdue: Vec<BriefCard> = cards
        .iter()
        .filter(|c| is_overdue(c))
        .map(|c| BriefCard::of(c))
        .collect();
    let renewals: Vec<BriefCard> = cards
        .iter()
        .filter(|c| {
            c.kind == CandidateKind::Renewal && c.due.is_some_and(|d| d <= now + Duration::days(7))
        })
        .map(|c| BriefCard::of(c))
        .collect();
    let top: Vec<BriefCard> = cards
        .iter()
        .filter(|c| {
            c.state.is_open()
                && c.kind != CandidateKind::MeetingPrep
                && !is_overdue(c)
                && shown(c, cfg)
        })
        .take(BRIEF_TOP)
        .map(|c| BriefCard::of(c))
        .collect();
    MorningBrief {
        date: today,
        generated_at: now,
        top,
        meetings,
        overdue,
        renewals,
        open_cards: cards.iter().filter(|c| c.state.is_open()).count(),
    }
}

impl MorningBrief {
    /// Plain Markdown (for notifications, the vault, or a quick look in a terminal).
    pub fn to_markdown(&self) -> String {
        let t = |x: OffsetDateTime| x.format(&Rfc3339).unwrap_or_default();
        let mut s = format!("# Morning brief — {}\n", self.date);
        if !self.meetings.is_empty() {
            s.push_str("\n## Today's meetings\n");
            for m in &self.meetings {
                s.push_str(&format!("- {} — {}", t(m.start), m.title));
                if !m.external.is_empty() {
                    s.push_str(&format!(" (with {})", m.external.join(", ")));
                }
                s.push('\n');
                for p in &m.prep {
                    if let Some(q) = &p.quote {
                        s.push_str(&format!("  - {q}\n"));
                    }
                }
            }
        }
        if !self.overdue.is_empty() {
            s.push_str("\n## Overdue\n");
            for c in &self.overdue {
                s.push_str(&format!("- {}\n", c.title));
            }
        }
        if !self.top.is_empty() {
            s.push_str("\n## Top\n");
            for c in &self.top {
                s.push_str(&format!("- {} — {}\n", c.title, c.summary));
            }
        }
        if !self.renewals.is_empty() {
            s.push_str("\n## Renewals\n");
            for c in &self.renewals {
                s.push_str(&format!("- {}\n", c.title));
            }
        }
        s
    }
}
