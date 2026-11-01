mod common;

use std::sync::Arc;

use common::*;
use nugget_core::capability::Locality;
use nugget_policy::{Decision, RiskClass};
use nugget_proactive::cards::CardStore;
use nugget_proactive::{
    Action, ActionRunner, CandidateKind, Card, CardState, CreateEvent, DiffOp, DismissReason,
    ExecOutcome, Executors, LlmJudge, MemCardStore, Proactive, ProactiveConfig, ProactiveState,
    Reminder, ResetScope, Scope, ScoringModel, StoreCardStore, judge_allowed,
};
use nugget_store::Store;
use time::Duration;

fn card_of(p: &Proactive, kind: CandidateKind) -> Card {
    let cards = p.all_cards().unwrap();
    cards.into_iter().find(|c| c.kind == kind).unwrap()
}

/// The fixture, refreshed once.
async fn refreshed() -> (Fixture, Proactive) {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    (f, p)
}

// ---- persistence, dedup, state machine ----

#[tokio::test]
async fn refresh_persists_dedups_and_never_resurrects_dismissed_cards() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let r = p.refresh(now()).await.unwrap();
    assert_eq!((r.generated, r.created), (7, 7), "{r:?}");
    // A second engine over the same store sees the same cards (cursor-backed state).
    assert_eq!(
        Proactive::new(f.store.clone(), cfg())
            .all_cards()
            .unwrap()
            .len(),
        7
    );
    let cursor = nugget_core::ids::SourceId::new("proactive:state");
    assert_eq!(f.store.get_cursor(&cursor).unwrap().unwrap()["version"], 1);

    let r = p.refresh(now() + Duration::hours(1)).await.unwrap();
    assert_eq!((r.created, r.updated), (0, 7), "{r:?}");
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let later = now() + Duration::hours(1);
    p.dismiss(&ana.id, DismissReason::AlreadyHandled, later)
        .unwrap();
    let r = p.refresh(now() + Duration::hours(2)).await.unwrap();
    assert_eq!((r.suppressed, r.created), (1, 0));
    assert_eq!(p.card(&ana.id).unwrap().state, CardState::Dismissed);
    assert!(p.inbox().unwrap().iter().all(|c| c.id != ana.id));
    // Terminal cards are compacted after 14 days and dropped after `tombstone_days`.
    p.refresh(now() + Duration::days(15)).await.unwrap();
    assert!(p.card(&ana.id).unwrap().evidence.is_empty());
    p.refresh(now() + Duration::days(91)).await.unwrap();
    assert!(p.card(&ana.id).is_err());
}

#[tokio::test]
async fn answered_threads_and_started_meetings_close_their_cards() {
    let (f, p) = refreshed().await;
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let meeting = card_of(&p, CandidateKind::MeetingPrep);
    put_mails(
        &f.store,
        "m-ana-reply||ana@acme.io|Re: Pricing for the pilot|0.5||m-ana-1||Attached.",
    );
    let r = p.refresh(now()).await.unwrap();
    assert!(r.resolved >= 1, "{r:?}");
    assert_eq!(p.card(&ana.id).unwrap().state, CardState::Expired);
    let r = p.refresh(now() + Duration::hours(27)).await.unwrap();
    assert!(r.expired >= 1, "{r:?}");
    assert_eq!(p.card(&meeting.id).unwrap().state, CardState::Expired);
}

#[tokio::test]
async fn state_machine() {
    let (_f, p) = refreshed().await;
    let ids: Vec<String> = p.inbox().unwrap().iter().map(|c| c.id.clone()).collect();
    p.mark_seen(&ids, now()).unwrap();
    assert!(
        p.inbox()
            .unwrap()
            .iter()
            .all(|c| c.state == CardState::Seen)
    );

    let renewal = card_of(&p, CandidateKind::Renewal);
    // Snooze, then it wakes up on a later refresh.
    p.snooze(&renewal.id, now() + Duration::hours(3), now())
        .unwrap();
    assert_eq!(p.card(&renewal.id).unwrap().state, CardState::Snoozed);
    assert!(p.inbox().unwrap().iter().all(|c| c.id != renewal.id));
    assert!(p.refresh(now() + Duration::hours(4)).await.unwrap().woken >= 1);
    assert_eq!(p.card(&renewal.id).unwrap().state, CardState::New);

    // Accept → done; done is terminal.
    p.accept(&renewal.id, now() + Duration::hours(5)).unwrap();
    p.complete(&renewal.id, now() + Duration::hours(5)).unwrap();
    let c = p.card(&renewal.id).unwrap();
    assert_eq!(c.state, CardState::Done);
    assert!(c.was_accepted);
    assert!(p.accept(&renewal.id, now()).is_err());
    assert!(
        p.dismiss(&renewal.id, DismissReason::NotRelevant, now())
            .is_err()
    );
    let states: Vec<CardState> = c.history.iter().map(|t| t.to).collect();
    use CardState::*;
    assert_eq!(states, [Seen, Snoozed, New, Accepted, Done]);
    // Completing an unaccepted card, or snoozing into the past, is rejected.
    let fu = card_of(&p, CandidateKind::FollowUp);
    assert!(p.complete(&fu.id, now()).is_err());
    assert!(p.snooze(&fu.id, now() - Duration::hours(1), now()).is_err());
}

/// GG7-6: the app localises card and action text from a key and raw arguments (ISO dates, a
/// number with its currency); cards stored before the fields existed still load.
#[tokio::test]
async fn card_and_action_text_is_structured_for_the_app() {
    let (_f, p) = refreshed().await;
    let renewal = p.all_cards().unwrap();
    let renewal = renewal
        .into_iter()
        .find(|c| c.title.starts_with("Cloudy Billing renews"))
        .unwrap();
    let text = serde_json::to_value(&renewal.title_text).unwrap();
    assert_eq!(
        text,
        serde_json::json!({ "key": "renewal.renews_amount", "args": {
            "vendor": "Cloudy Billing", "due": "2026-11-02", "amount": 49.0, "currency": "USD" } })
    );
    assert_eq!(renewal.detail_text.as_ref().unwrap().key, "renewal.detail");
    let reminder = renewal
        .actions
        .iter()
        .find(|a| a.name() == "reminder")
        .unwrap();
    let label = reminder.summary_text();
    assert_eq!(label.key, "action.reminder");
    let Action::Reminder(r) = reminder else {
        unreachable!()
    };
    let at =
        r.at.format(&time::format_description::well_known::Rfc3339)
            .unwrap();
    assert_eq!(label.args["at"], at.as_str());

    let mut old = serde_json::to_value(&renewal).unwrap();
    old.as_object_mut().unwrap().remove("title_text");
    old.as_object_mut().unwrap().remove("detail_text");
    let old: Card = serde_json::from_value(old).unwrap();
    assert_eq!((old.title_text, old.title), (None, renewal.title));
}

// ---- scoring and metrics ----

#[tokio::test]
async fn scoring_order_explanations_and_threshold() {
    let (f, p) = refreshed().await;
    let inbox = p.inbox().unwrap();
    assert!(inbox.windows(2).all(|w| w[0].rank >= w[1].rank));
    // The overdue commitment outranks the 5-day renewal.
    let pos = |k: &str| inbox.iter().position(|c| c.title.contains(k)).unwrap();
    assert!(pos("Signed pilot") < pos("Cloudy"));
    for c in &inbox {
        assert!(!c.factors.is_empty() && c.factors.len() <= 3, "{c:?}");
        assert!(c.score > 0.0 && c.score < 1.0 && c.rank >= cfg().min_score);
    }
    // Below `min_score`: kept but not offered.
    let strict = ProactiveConfig {
        min_score: 0.99,
        ..cfg()
    };
    let p = Proactive::new(f.store.clone(), strict);
    assert!(!p.all_cards().unwrap().is_empty());
    assert!(p.inbox().unwrap().is_empty());
    assert!(p.brief(now()).unwrap().top.is_empty());
}

/// GG4-5, GG4-8: feedback is scoped. The shared model never moves, cards of other kinds and
/// senders keep their rank exactly, and a scoped reset restores a card to its no-feedback rank.
#[tokio::test]
async fn feedback_is_scoped_and_resettable() {
    let more = "\
m-boxy-receipt|Box <billing@boxy.io>||Your receipt|30||||Thanks for your purchase: $10.00.
m-renew-2|Box <billing@boxy.io>||Renewal notice|1||||Your plan will renew on November 4, 2026 for $10.00.";
    let new_renewal = |p: &Proactive| {
        p.all_cards()
            .unwrap()
            .into_iter()
            .find(|c| c.evidence[0].item.key.as_str() == "m-renew-2")
            .unwrap()
    };
    // Control: the same mail, no feedback.
    let (cf, control) = refreshed().await;
    put_mails(&cf.store, more);
    control.refresh(now()).await.unwrap();
    let control_rank = new_renewal(&control).rank;

    let (f, p) = refreshed().await;
    let ana_rank = card_of(&p, CandidateKind::UnansweredThread).rank;
    for c in p.all_cards().unwrap() {
        if c.kind == CandidateKind::Renewal {
            p.dismiss(&c.id, DismissReason::NotImportant, now())
                .unwrap();
        }
    }
    assert_eq!(p.model().unwrap(), ScoringModel::default());
    assert_eq!(card_of(&p, CandidateKind::UnansweredThread).rank, ana_rank);
    put_mails(&f.store, more);
    p.refresh(now()).await.unwrap();
    let lowered = new_renewal(&p);
    assert!(
        lowered.rank < control_rank,
        "{} vs {control_rank}",
        lowered.rank
    );
    assert!(lowered.learned.iter().any(|e| e.scope == Scope::Kind));
    p.reset_learning(&ResetScope::One(Scope::Kind, "renewal".into()), now())
        .unwrap();
    let restored = new_renewal(&p);
    assert!(restored.learned.is_empty());
    assert_eq!(restored.rank, control_rank);
}

#[tokio::test]
async fn metrics_track_precision_and_time_to_action() {
    let (_f, p) = refreshed().await;
    let ids: Vec<String> = p.inbox().unwrap().iter().map(|c| c.id.clone()).collect();
    p.mark_seen(&ids, now()).unwrap();
    p.accept(&ids[0], now() + Duration::minutes(10)).unwrap();
    p.dismiss(
        &ids[1],
        DismissReason::WrongInfo,
        now() + Duration::minutes(30),
    )
    .unwrap();
    let other = DismissReason::Other("meh".into());
    p.dismiss(&ids[2], other, now() + Duration::minutes(50))
        .unwrap();
    let m = p.metrics().unwrap();
    assert_eq!((m.shown, m.accepted, m.dismissed), (7, 1, 2));
    assert!((m.precision.unwrap() - 1.0 / 7.0).abs() < 1e-9);
    assert_eq!(m.median_time_to_action_secs, Some(1800.0));
    assert_eq!(m.dismiss_reasons["wrong_info"], 1);
    assert_eq!(m.dismiss_reasons["other"], 1);
    assert_eq!(m.model_updates, 3);
}

// ---- actions ----

fn runner(policy: Arc<RecordingPolicy>, exec: Arc<RecordingExec>) -> ActionRunner {
    ActionRunner::new(
        policy,
        Executors {
            drafter: Some(exec.clone()),
            mail: Some(exec.clone()),
            calendar: Some(exec.clone()),
            reminders: Some(exec.clone()),
            notes: Some(exec),
        },
    )
}

fn calls(exec: &RecordingExec) -> Vec<String> {
    exec.calls.lock().unwrap().clone()
}

#[tokio::test]
async fn dry_run_diffs() {
    let (_f, p) = refreshed().await;
    let exec = Arc::new(RecordingExec::default());
    let r = runner(Arc::new(RecordingPolicy::default()), exec.clone());

    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let draft = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    assert_eq!(draft.preview.risk, RiskClass::Edit);
    let text = draft.preview.render();
    for line in [
        "+ To: ana@acme.io",
        "+ Subject: Re: Pricing for the pilot",
        "+ Here it is.",
        "Nothing is sent",
    ] {
        assert!(text.contains(line), "{line}: {text}");
    }
    // Preparing only drafted: nothing executed.
    assert_eq!(calls(&exec), ["draft:Re: Pricing for the pilot"]);

    let archive = p.prepare_action(&r, &ana.id, 1).await.unwrap();
    assert_eq!(archive.preview.risk, RiskClass::Destroy);
    let removes =
        |l: &nugget_proactive::DiffLine| l.op == DiffOp::Remove && l.text.starts_with("Inbox:");
    assert!(archive.preview.diff.iter().any(removes));

    let meeting = card_of(&p, CandidateKind::MeetingPrep);
    let note = p.prepare_action(&r, &meeting.id, 0).await.unwrap();
    assert!(
        note.preview
            .render()
            .contains("+ New note: Prep: Acme pilot kickoff")
    );

    let soc2 = p
        .all_cards()
        .unwrap()
        .into_iter()
        .find(|c| c.title.contains("SOC 2"))
        .unwrap();
    let ev = p.prepare_action(&r, &soc2.id, 1).await.unwrap();
    assert!(matches!(ev.action, Action::CreateEvent(_)));
    assert_eq!(
        ev.preview.risk,
        RiskClass::Edit,
        "no attendees, no invitations"
    );
    let rem = p.prepare_action(&r, &soc2.id, 0).await.unwrap();
    assert!(rem.preview.render().contains("+ Reminder at"));
    // An event with attendees sends invitations: Send risk.
    let a = Action::CreateEvent(CreateEvent {
        title: "Sync".into(),
        start: now(),
        end: now() + Duration::hours(1),
        attendees: vec!["x@y.z".into()],
        description: None,
    });
    assert_eq!(a.risk(), RiskClass::Send);
    assert!(a.preview().render().contains("Invitations will be sent"));
}

#[tokio::test]
async fn execution_only_after_approval_and_policy() {
    let (f, p) = refreshed().await;
    let policy = Arc::new(RecordingPolicy::default());
    let exec = Arc::new(RecordingExec::default());
    let r = runner(policy.clone(), exec.clone()).with_audit(f.store.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let prepared = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    exec.calls.lock().unwrap().clear();
    let set = |d: Option<Decision>| *policy.decision.lock().unwrap() = d;

    // An approval for something else (tampered action), or a wrong digest, is refused.
    let mut tampered = prepared.clone();
    if let Action::DraftReply(d) = &mut tampered.action {
        d.to = vec!["attacker@evil.example".into()];
    }
    let approval = prepared.approve(false);
    assert!(p.run_action(&r, &tampered, &approval, now()).await.is_err());
    let wrong = nugget_proactive::Approval {
        digest: "00".into(),
        confirmed: true,
    };
    assert!(p.run_action(&r, &prepared, &wrong, now()).await.is_err());
    assert!(calls(&exec).is_empty());
    assert!(
        policy.seen.lock().unwrap().is_empty(),
        "policy not even asked"
    );

    // Kill switch, denial, confirmation: nothing executes.
    *policy.halted.lock().unwrap() = true;
    let run = || p.run_action(&r, &prepared, &approval, now());
    assert_eq!(run().await.unwrap(), ExecOutcome::Halted);
    *policy.halted.lock().unwrap() = false;
    set(Some(Decision::Deny("mail blocked".into())));
    assert_eq!(
        run().await.unwrap(),
        ExecOutcome::Denied("mail blocked".into())
    );
    set(Some(Decision::Confirm("external recipient".into())));
    let confirm = ExecOutcome::NeedsConfirmation("external recipient".into());
    assert_eq!(run().await.unwrap(), confirm);
    assert!(calls(&exec).is_empty());
    assert_ne!(p.card(&ana.id).unwrap().state, CardState::Done);

    let confirmed = prepared.approve(true);
    let out = p
        .run_action(&r, &prepared, &confirmed, now())
        .await
        .unwrap();
    assert_eq!(out, ExecOutcome::Executed("draft-1".into()));
    assert_eq!(calls(&exec), ["save_draft:Re: Pricing for the pilot"]);
    let seen = policy.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        (seen[0].verb.as_str(), seen[0].declared_risk),
        ("save_draft", RiskClass::Edit)
    );
    assert_eq!(seen[0].task_id, format!("proactive:{}", ana.id));

    // Executing completes the card (accepted, then done) and is audited.
    let c = p.card(&ana.id).unwrap();
    assert_eq!(c.state, CardState::Done);
    assert!(c.was_accepted);
    let audit = f.store.recent_audit(10).unwrap();
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    assert!(
        actions.contains(&"action.draft_reply.executed"),
        "{actions:?}"
    );
    assert!(actions.contains(&"action.draft_reply.denied"));
    f.store.verify_audit_chain().unwrap();
    // A closed card can't run actions again.
    assert!(
        p.run_action(&r, &prepared, &confirmed, now())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn archiving_always_needs_confirmation() {
    let (f, p) = refreshed().await;
    let policy = Arc::new(RecordingPolicy::default());
    let exec = Arc::new(RecordingExec::default());
    let r = runner(policy.clone(), exec.clone()).with_audit(f.store.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let archive = p.prepare_action(&r, &ana.id, 1).await.unwrap();
    assert!(archive.action.requires_confirmation().is_some());
    exec.calls.lock().unwrap().clear();

    // The policy allows it, but the action itself asks for confirmation.
    let out = p
        .run_action(&r, &archive, &archive.approve(false), now())
        .await;
    let Ok(ExecOutcome::NeedsConfirmation(reason)) = out else {
        panic!("expected a confirmation, got {out:?}");
    };
    assert!(reason.contains("inbox"), "{reason}");
    assert!(calls(&exec).is_empty());
    let audit = f.store.recent_audit(10).unwrap();
    let needs = "action.archive_thread.needs_confirmation";
    assert!(audit.iter().any(|a| a.action == needs));

    // A policy denial still wins over the user's confirmation.
    *policy.decision.lock().unwrap() = Some(Decision::Deny("no mailbox edits".into()));
    let yes = archive.approve(true);
    let run = || p.run_action(&r, &archive, &yes, now());
    assert_eq!(
        run().await.unwrap(),
        ExecOutcome::Denied("no mailbox edits".into())
    );
    *policy.decision.lock().unwrap() = None;
    assert_eq!(
        run().await.unwrap(),
        ExecOutcome::Executed("archived".into())
    );
    assert_eq!(calls(&exec), ["archive:Pricing for the pilot"]);
    // Other actions need no extra confirmation.
    let reminder = Action::Reminder(Reminder {
        text: "x".into(),
        at: now(),
        about: None,
    });
    assert!(reminder.requires_confirmation().is_none());
}

#[tokio::test]
async fn missing_executor_is_unsupported() {
    let (_f, p) = refreshed().await;
    let r = ActionRunner::new(Arc::new(RecordingPolicy::default()), Executors::default());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    // No drafter: cannot even prepare a reply.
    assert!(p.prepare_action(&r, &ana.id, 0).await.is_err());
    let archive = p.prepare_action(&r, &ana.id, 1).await.unwrap();
    let err = p
        .run_action(&r, &archive, &archive.approve(false), now())
        .await;
    assert!(matches!(err, Err(nugget_core::Error::Unsupported(_))));
}

// ---- approval binding (security review PA-4) ----

#[tokio::test]
async fn pa_run_action_executes_only_the_cards_own_action() {
    let (_f, p) = refreshed().await;
    let exec = Arc::new(RecordingExec::default());
    let r = runner(Arc::new(RecordingPolicy::default()), exec.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let evil = Action::CreateEvent(CreateEvent {
        title: "x".into(),
        start: now(),
        end: now() + Duration::hours(1),
        attendees: vec!["attacker@evil.example".into()],
        description: None,
    });
    assert!(!ana.actions.contains(&evil));
    // The runner signs it, but the card does not offer it at that position.
    let forged = r.prepare(&ana.id, 0, &evil, &[]).await.unwrap();
    let out = p
        .run_action(&r, &forged, &forged.approve(true), now())
        .await;
    assert!(out.is_err());
    // Moving a real action to another position breaks its digest.
    let mut moved = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    moved.action_index = 1;
    assert!(
        p.run_action(&r, &moved, &moved.approve(true), now())
            .await
            .is_err()
    );
    assert!(!calls(&exec).iter().any(|c| c.starts_with("event")));
    assert_ne!(p.card(&ana.id).unwrap().state, CardState::Done);
}

#[tokio::test]
async fn approvals_are_keyed_by_the_runner_and_execute_once() {
    let (_f, p) = refreshed().await;
    let exec = Arc::new(RecordingExec::default());
    let policy = || Arc::new(RecordingPolicy::default());
    let c = card_of(&p, CandidateKind::Commitment);
    // Minted by another runner (another key): refused, even though it is the card's action.
    let foreign = p
        .prepare_action(&runner(policy(), exec.clone()), &c.id, 0)
        .await
        .unwrap();
    let ours = runner(policy(), exec.clone());
    let out = p
        .run_action(&ours, &foreign, &foreign.approve(true), now())
        .await;
    assert!(out.is_err());
    // An approval executes once on the runner directly.
    let prepared = p.prepare_action(&ours, &c.id, 0).await.unwrap();
    let approval = prepared.approve(true);
    let out = ours.execute(&prepared, &approval).await.unwrap();
    assert!(matches!(out, ExecOutcome::Executed(_)));
    let err = ours.execute(&prepared, &approval).await.unwrap_err();
    assert!(err.to_string().contains("already used"), "{err}");
    assert_eq!(calls(&exec).len(), 1);
    // The same key (e.g. the install's, after a restart) accepts it.
    let key = nugget_proactive::new_approval_key().unwrap();
    let a = runner(policy(), exec.clone()).with_key(key);
    let b = runner(policy(), exec.clone()).with_key(key);
    let prepared = p.prepare_action(&a, &c.id, 0).await.unwrap();
    let out = p
        .run_action(&b, &prepared, &prepared.approve(true), now())
        .await;
    assert!(matches!(out, Ok(ExecOutcome::Executed(_))));
}

/// Reminders that wait for a signal, so two runs overlap.
struct GatedReminders {
    gate: tokio::sync::Notify,
    calls: std::sync::Mutex<u32>,
}

#[async_trait::async_trait]
impl nugget_proactive::ReminderExecutor for GatedReminders {
    async fn create_reminder(&self, _: &Reminder) -> nugget_core::Result<String> {
        self.gate.notified().await;
        *self.calls.lock().unwrap() += 1;
        Ok("rem-1".into())
    }
}

#[tokio::test]
async fn concurrent_runs_on_a_card_execute_once() {
    let (_f, p) = refreshed().await;
    let gated = Arc::new(GatedReminders {
        gate: tokio::sync::Notify::new(),
        calls: std::sync::Mutex::new(0),
    });
    let reminders = Executors {
        reminders: Some(gated.clone()),
        ..Default::default()
    };
    let r = ActionRunner::new(Arc::new(RecordingPolicy::default()), reminders);
    let c = card_of(&p, CandidateKind::Commitment);
    let prepared = p.prepare_action(&r, &c.id, 0).await.unwrap();
    assert!(matches!(prepared.action, Action::Reminder(_)));
    let approval = prepared.approve(true);
    let (first, second) = tokio::join!(p.run_action(&r, &prepared, &approval, now()), async {
        // Runs while the first is parked in the executor.
        let out = p.run_action(&r, &prepared, &approval, now()).await;
        gated.gate.notify_one();
        out
    });
    assert_eq!(first.unwrap(), ExecOutcome::Executed("rem-1".into()));
    assert!(second.is_err());
    assert_eq!(*gated.calls.lock().unwrap(), 1);
    // And afterwards the card is closed: no replay.
    assert!(p.run_action(&r, &prepared, &approval, now()).await.is_err());
}

/// Asks for confirmation and refuses to count any approval (the rate limit is reached).
struct ExhaustedPolicy;

impl nugget_policy::PolicyGate for ExhaustedPolicy {
    fn evaluate(&self, _: &nugget_policy::ActionDescriptor) -> Decision {
        Decision::Confirm("check this".into())
    }
    fn halted(&self) -> bool {
        false
    }
    fn record_approved(&self, _: &nugget_policy::ActionDescriptor, _: RiskClass) -> bool {
        false
    }
}

#[tokio::test]
async fn a_confirmed_action_over_the_rate_limit_is_denied() {
    let (_f, p) = refreshed().await;
    let exec = Arc::new(RecordingExec::default());
    let reminders = Executors {
        reminders: Some(exec.clone()),
        ..Default::default()
    };
    let r = ActionRunner::new(Arc::new(ExhaustedPolicy), reminders);
    let c = card_of(&p, CandidateKind::Commitment);
    let prepared = p.prepare_action(&r, &c.id, 0).await.unwrap();
    let out = p
        .run_action(&r, &prepared, &prepared.approve(false), now())
        .await;
    assert!(matches!(out, Ok(ExecOutcome::NeedsConfirmation(_))));
    let out = p
        .run_action(&r, &prepared, &prepared.approve(true), now())
        .await;
    assert!(matches!(out, Ok(ExecOutcome::Denied(_))));
    assert!(calls(&exec).is_empty());
}

// ---- judge ----

/// A judge reply enriching every key, plus one invented candidate.
fn judge_reply(f: &Fixture) -> String {
    let p = Proactive::new(f.store.clone(), cfg());
    let items: Vec<_> = p
        .candidates(now())
        .unwrap()
        .into_iter()
        .map(|c| serde_json::json!({"key": c.dedup_key, "summary": "Worth doing today.", "suggested_action": "archive_thread", "confidence": 0.9}))
        .chain([serde_json::json!({"key": "invented:1", "summary": "made up", "confidence": 1.0})])
        .collect();
    format!("```json\n{}\n```", serde_json::json!({"items": items}))
}

fn judged(f: &Fixture, cfg: ProactiveConfig, provider: &Arc<FakeProvider>) -> Proactive {
    Proactive::new(f.store.clone(), cfg)
        .with_card_store(Arc::new(MemCardStore::default()))
        .with_judge(Arc::new(LlmJudge::new(provider.clone())))
}

#[tokio::test]
async fn local_judge_enriches_but_never_invents() {
    let f = fixture();
    let provider = FakeProvider::new(Locality::OnDevice, &judge_reply(&f));
    let p = judged(&f, cfg(), &provider);
    let r = p.refresh(now()).await.unwrap();
    assert_eq!((r.judged, r.created, r.judge_withheld), (7, 7, 0), "{r:?}");
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    assert_eq!(
        ana.enrichment.as_ref().unwrap().summary,
        "Worth doing today."
    );
    // Suggested action moved first; the rank blends model and judge.
    assert_eq!(ana.actions[0].name(), "archive_thread");
    assert!((ana.rank - (0.8 * ana.score + 0.2 * 0.9)).abs() < 1e-9);
    assert_eq!(
        provider.requests.lock().unwrap()[0].feature,
        "proactive_judge"
    );
    // GG4-10: the judge knows today's date and who owes whom.
    let sent = format!("{:?}", provider.requests.lock().unwrap()[0].messages);
    assert!(sent.contains("2026-10-28") && sent.contains("has not replied yet"));
    // Already enriched cards are not re-sent...
    assert_eq!(p.refresh(now()).await.unwrap().judged, 0);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    // ...until their due date passes (the renewal, 5 days out).
    let later = now() + Duration::days(6);
    assert!(p.refresh(later).await.unwrap().judged > 0);
    let sent = format!("{:?}", provider.requests.lock().unwrap()[1].messages);
    assert!(sent.contains("(overdue)"), "{sent}");

    // A failing judge falls back to the model.
    let provider = FakeProvider::new(Locality::OnDevice, "not json at all");
    let r = judged(&f, cfg(), &provider).refresh(now()).await.unwrap();
    assert!(r.judge_error.is_some());
    assert_eq!(r.created, 7);
}

#[tokio::test]
async fn cloud_judge_is_gated_and_lists_evidence_items() {
    let f = fixture();
    let reply = judge_reply(&f);
    // Cloud not allowed: nothing is sent.
    let provider = FakeProvider::new(Locality::Cloud, &reply);
    let r = judged(&f, cfg(), &provider).refresh(now()).await.unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!((r.judge_withheld, r.created), (7, 7));

    // Cloud allowed: the egress ledger gets the items whose content was sent (GG8-6), as
    // provenance only.
    let allow = ProactiveConfig {
        judge_allow_cloud: true,
        ..cfg()
    };
    let provider = FakeProvider::new(Locality::Cloud, &reply);
    judged(&f, allow.clone(), &provider)
        .refresh(now())
        .await
        .unwrap();
    {
        let reqs = provider.requests.lock().unwrap();
        let cands = Proactive::new(f.store.clone(), cfg())
            .candidates(now())
            .unwrap();
        for e in cands.iter().flat_map(|c| &c.evidence) {
            assert!(reqs[0].items.contains(&e.item), "{:?} missing", e.item);
        }
        assert!(
            !serde_json::to_string(&reqs[0].messages)
                .unwrap()
                .contains("\"items\"")
        );
    }

    // Sensitive candidates are never sent.
    let ana = ANA.replace("|3||||", "|3|||sensitive|");
    put_mails(&f.store, &ana);
    let provider = FakeProvider::new(Locality::Cloud, &reply);
    let p = judged(&f, allow, &provider);
    let r = p.refresh(now()).await.unwrap();
    let sent = serde_json::to_string(&provider.requests.lock().unwrap()[0].messages).unwrap();
    assert!(
        !sent.contains("Pricing for the pilot"),
        "sensitive content left the device"
    );
    assert!(r.judge_withheld >= 1, "{r:?}");
    assert!(
        card_of(&p, CandidateKind::UnansweredThread)
            .enrichment
            .is_none()
    );

    assert!(judge_allowed(Locality::OnDevice, false, true));
    assert!(judge_allowed(Locality::LocalNetwork, false, true));
    assert!(!judge_allowed(Locality::Cloud, true, true));
    assert!(!judge_allowed(Locality::Cloud, false, false));
}

// ---- brief and stores ----

#[tokio::test]
async fn morning_brief_structure_and_utc_offset() {
    let f = fixture();
    // Something today with an external attendee, plus the internal standup.
    let at = now() + Duration::hours(5);
    put_event(
        &f.store,
        "ev-today",
        "Acme pilot call",
        at,
        &[ME, "ana@acme.io"],
    );
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let b = p.brief(now()).unwrap();
    assert_eq!(b.date, time::macros::date!(2026 - 10 - 28));
    let titles: Vec<&str> = b.meetings.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, ["Standup", "Acme pilot call"]);
    let call = &b.meetings[1];
    assert_eq!(call.external, ["ana@acme.io"]);
    assert!(call.prep_card.is_some() && b.meetings[0].prep_card.is_none());
    assert!(call.prep.iter().any(|e| e.item == f.unanswered));
    // Overdue: Ana's agreement.
    assert_eq!(b.overdue.len(), 1);
    assert!(b.overdue[0].title.contains("Signed pilot"));
    // Top: no meeting-prep or overdue cards, at most 5, ranked.
    assert!(!b.top.is_empty() && b.top.len() <= 5);
    assert!(
        b.top
            .iter()
            .all(|c| c.kind != CandidateKind::MeetingPrep && !c.factors.is_empty())
    );
    assert!(b.top.windows(2).all(|w| w[0].rank >= w[1].rank));
    assert_eq!(b.renewals.len(), 2);
    assert!(b.open_cards >= 7);
    let md = b.to_markdown();
    assert!(
        md.contains("## Today's meetings") && md.contains("## Overdue"),
        "{md}"
    );

    // 09:00 UTC is already the 29th at UTC+16; the initial config is kept.
    p.set_utc_offset(time::macros::offset!(+16));
    assert_eq!(p.utc_offset(), time::macros::offset!(+16));
    assert_eq!(
        p.brief(now()).unwrap().date,
        time::macros::date!(2026 - 10 - 29)
    );
    assert_eq!(p.config().utc_offset, time::UtcOffset::UTC);
}

#[test]
fn card_stores_round_trip_and_refuse_newer_versions() {
    let store = Store::open_in_memory().unwrap();
    for cs in [
        Box::new(StoreCardStore::new(store.clone())) as Box<dyn CardStore>,
        Box::new(MemCardStore::default()),
    ] {
        let mut s = cs.load().unwrap();
        assert!(s.cards.is_empty());
        s.model.updates = 5;
        s.last_refresh = Some(now());
        cs.save(&s).unwrap();
        let back: ProactiveState = cs.load().unwrap();
        assert_eq!(back, s);
    }
    let cursor = nugget_core::ids::SourceId::new("proactive:state");
    let newer = serde_json::json!({"version": 99, "cards": {}, "model": {}});
    store.set_cursor(&cursor, newer).unwrap();
    assert!(StoreCardStore::new(store).load().is_err());
}
