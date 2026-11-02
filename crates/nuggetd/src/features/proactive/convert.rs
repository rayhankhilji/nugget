//! `nugget-proactive` types to the wire DTOs in `nugget_core::ipc` (`---- Proactive ----`).

use nugget_core::ipc::{
    CardExecuteResult, CardPreview, KindMetricsView, LearnedView, LocalText, PreviewLine,
    ProactiveMetrics, ResetLearningRequest,
};
use nugget_core::{Error, Result};
use nugget_proactive::actions::risk_name;
pub(crate) use nugget_proactive::view::{brief_view as brief, card_view, iso_date};
use nugget_proactive::{
    Action, CandidateKind, DiffOp, DismissReason, ExecOutcome, LearnedEntry, Metrics,
    PreparedAction, ResetScope, Scope,
};

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
        summary_text: Some(p.action.summary_text()),
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

/// `action` is the one that ran, for a localisable confirmation question.
pub(crate) fn outcome(o: ExecOutcome, action: &Action) -> CardExecuteResult {
    let detail_text = match &o {
        ExecOutcome::NeedsConfirmation(r) => Some(
            action
                .confirmation_text()
                .filter(|_| action.requires_confirmation() == Some(r.as_str()))
                .unwrap_or_else(|| {
                    LocalText::new("confirm.risk").arg("risk", risk_name(action.risk()))
                }),
        ),
        _ => None,
    };
    let (outcome, detail) = match o {
        ExecOutcome::Executed(id) => ("executed", Some(id)),
        ExecOutcome::NeedsConfirmation(r) => ("needs_confirmation", Some(r)),
        ExecOutcome::Denied(r) => ("denied", Some(r)),
        ExecOutcome::Halted => ("halted", None),
    };
    CardExecuteResult {
        outcome: outcome.to_owned(),
        detail,
        detail_text,
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
