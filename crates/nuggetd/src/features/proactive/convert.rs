//! `nugget-proactive` types to the wire DTOs in `nugget_core::ipc` (`---- Proactive (M5) ----`).

use nugget_core::ipc::{
    BriefCardView, BriefMeetingView, BriefView, CardActionView, CardEvidence, CardExecuteResult,
    CardFactor, CardLearned, CardPreview, CardView, KindMetricsView, LearnedView, PreviewLine,
    ProactiveMetrics, ResetLearningRequest,
};
use nugget_core::{Error, Result};
use nugget_proactive::actions::risk_name;
use nugget_proactive::{
    Action, BriefCard, BriefMeeting, CandidateKind, Card, DiffOp, DismissReason, Effect,
    EvidenceRef, ExecOutcome, Factor, LearnedEntry, Metrics, MorningBrief, PreparedAction,
    ResetScope, Scope,
};
use time::Date;

pub(crate) fn factor(f: &Factor) -> CardFactor {
    CardFactor {
        name: f.name.clone(),
        value: f.value,
        weight: f.weight,
        contribution: f.contribution,
    }
}

pub(crate) fn evidence(e: &EvidenceRef) -> CardEvidence {
    CardEvidence {
        item: e.item.clone(),
        quote: e.quote.clone(),
        url: e.url.clone(),
    }
}

pub(crate) fn action_view(index: usize, a: &Action, supported: bool) -> CardActionView {
    CardActionView {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        kind: a.name().to_owned(),
        label: a.preview().summary,
        risk: risk_name(a.risk()).to_owned(),
        supported,
    }
}

/// `supports` says whether an executor is configured for an action.
pub(crate) fn card_view(c: &Card, supports: &dyn Fn(&Action) -> bool) -> CardView {
    CardView {
        id: c.id.clone(),
        kind: c.kind.as_str().to_owned(),
        title: c.title.clone(),
        detail: c.detail.clone(),
        summary: c.enrichment.as_ref().map(|e| e.summary.clone()),
        people: c.people.clone(),
        due: c.due,
        score: c.score,
        rank: c.rank,
        factors: c.factors.iter().map(factor).collect(),
        evidence: c.evidence.iter().map(evidence).collect(),
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
pub(crate) fn learned_effect(e: &Effect) -> CardLearned {
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

/// One entry of `proactive.learning`.
pub(crate) fn learned_view(e: &LearnedEntry) -> LearnedView {
    LearnedView {
        scope: e.scope.as_str().to_owned(),
        key: e.key.clone(),
        label: e.label.clone(),
        accepted: e.accepted,
        dismissed: e.dismissed,
        adjustment: e.adjustment,
        suppresses: e.suppresses,
        last_at: e.last_at,
    }
}

/// `proactive.reset_learning`'s request. `key` is required (non-empty) unless `scope` is `all`;
/// sender and domain keys are matched case-insensitively, a `kind` key must be a card kind's wire
/// name.
pub(crate) fn reset_scope(req: &ResetLearningRequest) -> Result<ResetScope> {
    let scope = req.scope.trim();
    if scope == "all" {
        return Ok(ResetScope::All);
    }
    let Some(scope) = Scope::parse(scope) else {
        return Err(Error::Invalid(format!(
            "unknown learning scope {:?} (all, sender, domain, kind, thread)",
            req.scope
        )));
    };
    let key = req.key.as_deref().map(str::trim).unwrap_or_default();
    if key.is_empty() {
        return Err(Error::Invalid(format!(
            "a key is required to reset the {} scope",
            scope.as_str()
        )));
    }
    let key = match scope {
        Scope::Sender | Scope::Domain => key.to_lowercase(),
        Scope::Kind => {
            if !CandidateKind::ALL.iter().any(|k| k.as_str() == key) {
                return Err(Error::Invalid(format!("unknown card kind {key:?}")));
            }
            key.to_owned()
        }
        Scope::Thread => key.to_owned(),
    };
    Ok(ResetScope::One(scope, key))
}

/// The audit target of a reset: `all`, or `<scope>:<key>`.
pub(crate) fn reset_target(scope: &ResetScope) -> String {
    match scope {
        ResetScope::All => "all".to_owned(),
        ResetScope::One(s, k) => format!("{}:{k}", s.as_str()),
    }
}

fn op_name(op: DiffOp) -> &'static str {
    match op {
        DiffOp::Add => "add",
        DiffOp::Remove => "remove",
        DiffOp::Context => "context",
    }
}

pub(crate) fn preview(p: &PreparedAction, action: u32) -> CardPreview {
    CardPreview {
        card_id: p.card_id.clone(),
        action,
        kind: p.action.name().to_owned(),
        summary: p.preview.summary.clone(),
        risk: risk_name(p.preview.risk).to_owned(),
        target: p.preview.target.clone(),
        diff: p
            .preview
            .diff
            .iter()
            .map(|l| PreviewLine {
                op: op_name(l.op).to_owned(),
                text: l.text.clone(),
            })
            .collect(),
        digest: p.digest.clone(),
    }
}

pub(crate) fn outcome(o: ExecOutcome) -> CardExecuteResult {
    let (outcome, detail) = match o {
        ExecOutcome::Executed(id) => ("executed", Some(id)),
        ExecOutcome::NeedsConfirmation(r) => ("needs_confirmation", Some(r)),
        ExecOutcome::Denied(r) => ("denied", Some(r)),
        ExecOutcome::Halted => ("halted", None),
    };
    CardExecuteResult {
        outcome: outcome.to_owned(),
        detail,
        artifact: None,
    }
}

/// `reason` (and `text` for `other`) from `cards.feedback`. Unset means `other`.
pub(crate) fn dismiss_reason(reason: Option<&str>, text: Option<&str>) -> Result<DismissReason> {
    Ok(match reason.unwrap_or("other") {
        "not_relevant" => DismissReason::NotRelevant,
        "already_handled" => DismissReason::AlreadyHandled,
        "wrong_info" => DismissReason::WrongInfo,
        "not_important" => DismissReason::NotImportant,
        "other" => DismissReason::Other(text.unwrap_or_default().trim().to_owned()),
        r => return Err(Error::Invalid(format!("unknown dismiss reason {r:?}"))),
    })
}

/// `YYYY-MM-DD`.
pub(crate) fn iso_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn brief_card(c: &BriefCard) -> BriefCardView {
    BriefCardView {
        card_id: c.card_id.clone(),
        kind: c.kind.as_str().to_owned(),
        title: c.title.clone(),
        summary: c.summary.clone(),
        due: c.due,
        rank: c.rank,
        factors: c.factors.iter().map(factor).collect(),
        actions: c.actions.clone(),
    }
}

fn brief_meeting(m: &BriefMeeting) -> BriefMeetingView {
    BriefMeetingView {
        title: m.title.clone(),
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

pub(crate) fn brief(b: &MorningBrief) -> BriefView {
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

pub(crate) fn metrics(m: Metrics) -> ProactiveMetrics {
    ProactiveMetrics {
        total: m.total,
        by_state: m.by_state,
        shown: m.shown,
        accepted: m.accepted,
        dismissed: m.dismissed,
        snoozed: m.snoozed,
        precision: m.precision,
        by_kind: m
            .by_kind
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    KindMetricsView {
                        shown: v.shown,
                        accepted: v.accepted,
                        dismissed: v.dismissed,
                        precision: v.precision,
                    },
                )
            })
            .collect(),
        median_time_to_action_secs: m.median_time_to_action_secs,
        mean_time_to_action_secs: m.mean_time_to_action_secs,
        dismiss_reasons: m.dismiss_reasons,
        model_updates: m.model_updates,
    }
}
