//! Cards and the brief as the wire DTOs in `nugget_core::ipc` (`---- Proactive ----`), shared by
//! the daemon and Nugget Cloud so both serve the desktop the same shapes. Sensitive cards are
//! never quoted (GG4-7): no evidence quotes, no model summary.

use nugget_core::ipc::{
    BriefCardView, BriefMeetingView, BriefView, CardActionView, CardEvidence, CardFactor,
    CardLearned, CardView,
};
use time::Date;

use crate::actions::risk_name;
use crate::{Action, BriefCard, BriefMeeting, Card, Effect, EvidenceRef, Factor, MorningBrief};

pub fn factor(f: &Factor) -> CardFactor {
    CardFactor {
        name: f.name.clone(),
        value: f.value,
        weight: f.weight,
        contribution: f.contribution,
    }
}

pub fn evidence(e: &EvidenceRef) -> CardEvidence {
    CardEvidence {
        item: e.item.clone(),
        quote: e.quote.clone(),
        url: e.url.clone(),
    }
}

pub fn action_view(index: usize, a: &Action, supported: bool) -> CardActionView {
    CardActionView {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        kind: a.name().to_owned(),
        label: a.preview().summary,
        label_text: Some(a.summary_text()),
        risk: risk_name(a.risk()).to_owned(),
        supported,
    }
}

/// `supports` says whether an executor is configured for an action.
pub fn card_view(c: &Card, supports: &dyn Fn(&Action) -> bool) -> CardView {
    CardView {
        id: c.id.clone(),
        kind: c.kind.as_str().to_owned(),
        title: c.title.clone(),
        detail: c.detail.clone(),
        title_text: c.title_text.clone(),
        detail_text: c.detail_text.clone(),
        // Sensitive cards are never quoted (GG4-7): no evidence quotes, no model summary.
        summary: c
            .enrichment
            .as_ref()
            .filter(|_| !c.sensitive)
            .map(|e| e.summary.clone()),
        people: c.people.clone(),
        due: c.due,
        score: c.score,
        rank: c.rank,
        factors: c.factors.iter().map(factor).collect(),
        evidence: c
            .evidence
            .iter()
            .map(|e| CardEvidence {
                quote: e.quote.clone().filter(|_| !c.sensitive),
                ..evidence(e)
            })
            .collect(),
        actions: c
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| action_view(i, a, supports(a)))
            .collect(),
        state: c.state.as_str().to_owned(),
        sensitive: c.sensitive,
        created_at: c.created_at,
        snoozed_until: c.snoozed_until,
        learned: c.learned.iter().map(learned_effect).collect(),
    }
}

/// How feedback changed one card (the "why" line).
pub fn learned_effect(e: &Effect) -> CardLearned {
    CardLearned {
        scope: e.scope.as_str().to_owned(),
        key: e.key.clone(),
        direction: e.direction.as_str().to_owned(),
        accepted: e.accepted,
        dismissed: e.dismissed,
        reason: e.reason.clone(),
        contribution: e.contribution,
    }
}

/// `YYYY-MM-DD`.
pub fn iso_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

pub fn brief_card(c: &BriefCard) -> BriefCardView {
    BriefCardView {
        card_id: c.card_id.clone(),
        kind: c.kind.as_str().to_owned(),
        title: c.title.clone(),
        summary: c.summary.clone(),
        title_text: c.title_text.clone(),
        summary_text: c.summary_text.clone(),
        due: c.due,
        rank: c.rank,
        factors: c.factors.iter().map(factor).collect(),
        actions: c.actions.clone(),
    }
}

pub fn brief_meeting(m: &BriefMeeting) -> BriefMeetingView {
    BriefMeetingView {
        title: m.title.clone(),
        item: Some(m.item.clone()),
        url: m.url.clone(),
        start: m.start,
        end: m.end,
        all_day: m.all_day,
        location: m.location.clone(),
        attendees: m.attendees.clone(),
        external: m.external.clone(),
        prep_card: m.prep_card.clone(),
        prep: m.prep.iter().map(evidence).collect(),
    }
}

pub fn brief_view(b: &MorningBrief) -> BriefView {
    BriefView {
        date: iso_date(b.date),
        generated_at: b.generated_at,
        top: b.top.iter().map(brief_card).collect(),
        meetings: b.meetings.iter().map(brief_meeting).collect(),
        overdue: b.overdue.iter().map(brief_card).collect(),
        renewals: b.renewals.iter().map(brief_card).collect(),
        open_cards: u32::try_from(b.open_cards).unwrap_or(u32::MAX),
    }
}
