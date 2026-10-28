mod common;

use std::sync::Arc;

use common::*;
use nugget_core::capability::Locality;
use nugget_policy::{Decision, RiskClass};
use nugget_proactive::cards::CardStore;
use nugget_proactive::{
    Action, ActionRunner, CandidateKind, CardState, DiffOp, DismissReason, ExecOutcome, Executors,
    LlmJudge, MemCardStore, Proactive, ProactiveState, StoreCardStore, judge_allowed,
};
use nugget_store::Store;
use time::Duration;

fn card_of(p: &Proactive, kind: CandidateKind) -> nugget_proactive::Card {
    p.all_cards()
        .unwrap()
        .into_iter()
        .find(|c| c.kind == kind)
        .unwrap()
}

// ---- persistence, dedup, state machine ----

#[tokio::test]
async fn refresh_persists_cards_in_the_encrypted_store() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let r = p.refresh(now()).await.unwrap();
    assert_eq!(r.generated, 7, "{r:?}");
    assert_eq!(r.created, 7);

    // A second engine over the same store sees the same cards (cursor-backed state).
    let p2 = Proactive::new(f.store.clone(), cfg());
    assert_eq!(p2.all_cards().unwrap().len(), 7);
    let raw = f
        .store
        .get_cursor(&nugget_core::ids::SourceId::new("proactive:state"))
        .unwrap()
        .unwrap();
    assert_eq!(raw["version"], 1);
}

#[tokio::test]
async fn dedup_across_runs() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let r = p.refresh(now() + Duration::hours(1)).await.unwrap();
    assert_eq!(r.created, 0, "{r:?}");
    assert_eq!(r.updated, 7);
    assert_eq!(p.all_cards().unwrap().len(), 7);

    // A dismissed card never comes back.
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    p.dismiss(
        &ana.id,
        DismissReason::AlreadyHandled,
        now() + Duration::hours(1),
    )
    .unwrap();
    let r = p.refresh(now() + Duration::hours(2)).await.unwrap();
    assert_eq!(r.suppressed, 1);
    assert_eq!(r.created, 0);
    assert_eq!(p.card(&ana.id).unwrap().state, CardState::Dismissed);
    assert!(p.inbox().unwrap().iter().all(|c| c.id != ana.id));
}

#[tokio::test]
async fn answered_thread_resolves_its_card() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    put_mail(
        &f.store,
        Mail {
            key: "m-ana-reply",
            from: ME,
            to: &["ana@acme.io"],
            subject: "Re: Pricing for the pilot",
            body: "Attached.",
            days_ago: 0.5,
            message_id: "<me-ana@nugget.dev>",
            in_reply_to: Some("<ana-1@acme.io>"),
            labels: &["SENT"],
            ..Default::default()
        },
    );
    let r = p.refresh(now()).await.unwrap();
    assert!(r.resolved >= 1, "{r:?}");
    assert_eq!(p.card(&ana.id).unwrap().state, CardState::Expired);
}

#[tokio::test]
async fn deadlines_expire_cards() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let meeting = card_of(&p, CandidateKind::MeetingPrep);
    // After the meeting starts its card expires.
    let r = p.refresh(now() + Duration::hours(27)).await.unwrap();
    assert!(r.expired >= 1, "{r:?}");
    assert_eq!(p.card(&meeting.id).unwrap().state, CardState::Expired);
}

#[tokio::test]
async fn state_machine() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
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
    let r = p.refresh(now() + Duration::hours(4)).await.unwrap();
    assert!(r.woken >= 1);
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
    assert!(
        p.complete(&card_of(&p, CandidateKind::FollowUp).id, now())
            .is_err()
    );
    let states: Vec<CardState> = c.history.iter().map(|t| t.to).collect();
    assert_eq!(
        states,
        vec![
            CardState::Seen,
            CardState::Snoozed,
            CardState::New,
            CardState::Accepted,
            CardState::Done
        ]
    );
    // Snoozing into the past is rejected.
    let fu = card_of(&p, CandidateKind::FollowUp);
    assert!(p.snooze(&fu.id, now() - Duration::hours(1), now()).is_err());
}

#[tokio::test]
async fn terminal_cards_are_pruned_after_tombstone_days() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    p.dismiss(&ana.id, DismissReason::NotRelevant, now())
        .unwrap();
    p.refresh(now() + Duration::days(15)).await.unwrap();
    let c = p.card(&ana.id).unwrap();
    assert!(c.evidence.is_empty(), "compacted after 14 days");
    p.refresh(now() + Duration::days(91)).await.unwrap();
    assert!(p.card(&ana.id).is_err(), "dropped after tombstone_days");
}

// ---- scoring ----

#[tokio::test]
async fn scoring_order_and_explanations() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let inbox = p.inbox().unwrap();
    assert!(inbox.windows(2).all(|w| w[0].rank >= w[1].rank));
    // The overdue commitment (urgency 1.2) outranks the 5-day renewal.
    let pos = |k: &str| inbox.iter().position(|c| c.title.contains(k)).unwrap();
    assert!(pos("Signed pilot") < pos("Cloudy"));
    for c in &inbox {
        assert!(!c.factors.is_empty() && c.factors.len() <= 3, "{c:?}");
        assert!(c.score > 0.0 && c.score < 1.0);
    }
}

#[tokio::test]
async fn feedback_moves_weights_and_order() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let before = p.model().unwrap();
    let renewal_rank = card_of(&p, CandidateKind::Renewal).rank;

    // Dismissing renewals teaches the model the user doesn't want them.
    let renewals: Vec<String> = p
        .all_cards()
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CandidateKind::Renewal)
        .map(|c| c.id)
        .collect();
    for id in &renewals {
        p.dismiss(id, DismissReason::NotImportant, now()).unwrap();
    }
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    p.accept(&ana.id, now()).unwrap();
    // A repeated accept is not double-counted.
    p.accept(&ana.id, now()).unwrap();

    let after = p.model().unwrap();
    assert_eq!(after.updates, 3);
    assert!(after.weights["kind_renewal"] < before.weights["kind_renewal"]);
    assert!(after.weights["kind_unanswered_thread"] > before.weights["kind_unanswered_thread"]);

    // A new renewal (from a vendor the user pays) now scores lower than the first one did.
    put_mail(
        &f.store,
        Mail {
            key: "m-boxy-receipt",
            from: "Box <billing@boxy.io>",
            subject: "Your receipt",
            body: "Thanks for your purchase: $10.00.",
            days_ago: 30.0,
            message_id: "<receipt@boxy.io>",
            ..Default::default()
        },
    );
    put_mail(
        &f.store,
        Mail {
            key: "m-renew-2",
            from: "Box <billing@boxy.io>",
            subject: "Renewal notice",
            body: "Your plan will renew on November 4, 2026 for $10.00.",
            days_ago: 1.0,
            message_id: "<renew2@boxy.io>",
            ..Default::default()
        },
    );
    p.refresh(now()).await.unwrap();
    let new_renewal = p
        .all_cards()
        .unwrap()
        .into_iter()
        .find(|c| c.kind == CandidateKind::Renewal && c.state == CardState::New)
        .unwrap();
    assert!(
        new_renewal.rank < renewal_rank,
        "{} vs {renewal_rank}",
        new_renewal.rank
    );
    // Persisted with the cards.
    assert_eq!(
        Proactive::new(f.store.clone(), cfg()).model().unwrap(),
        p.model().unwrap()
    );
}

#[tokio::test]
async fn cards_below_min_score_are_kept_but_not_offered() {
    let f = fixture();
    let strict = nugget_proactive::ProactiveConfig {
        min_score: 0.99,
        ..cfg()
    };
    let p = Proactive::new(f.store.clone(), strict.clone());
    p.refresh(now()).await.unwrap();
    assert!(!p.all_cards().unwrap().is_empty());
    assert!(p.inbox().unwrap().is_empty());
    assert!(p.brief(now()).unwrap().top.is_empty());
    // The same cards, with the default threshold, are offered.
    let p = Proactive::new(f.store.clone(), cfg());
    assert!(!p.inbox().unwrap().is_empty());
    assert!(p.inbox().unwrap().iter().all(|c| c.rank >= cfg().min_score));
}

// ---- metrics ----

#[tokio::test]
async fn metrics_track_precision_and_time_to_action() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let inbox = p.inbox().unwrap();
    let ids: Vec<String> = inbox.iter().map(|c| c.id.clone()).collect();
    p.mark_seen(&ids, now()).unwrap();
    p.accept(&ids[0], now() + Duration::minutes(10)).unwrap();
    p.dismiss(
        &ids[1],
        DismissReason::WrongInfo,
        now() + Duration::minutes(30),
    )
    .unwrap();
    p.dismiss(
        &ids[2],
        DismissReason::Other("meh".into()),
        now() + Duration::minutes(50),
    )
    .unwrap();
    let m = p.metrics().unwrap();
    assert_eq!(m.shown, 7);
    assert_eq!(m.accepted, 1);
    assert_eq!(m.dismissed, 2);
    assert!((m.precision.unwrap() - 1.0 / 7.0).abs() < 1e-9);
    assert_eq!(m.median_time_to_action_secs, Some(1800.0));
    assert_eq!(m.dismiss_reasons["wrong_info"], 1);
    assert_eq!(m.dismiss_reasons["other"], 1);
    assert_eq!(m.model_updates, 3);
    let json = serde_json::to_value(&m).unwrap();
    assert!(json["by_kind"].is_object());
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

#[tokio::test]
async fn dry_run_diffs() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let exec = Arc::new(RecordingExec::default());
    let r = runner(Arc::new(RecordingPolicy::default()), exec.clone());

    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let draft = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    assert_eq!(draft.preview.risk, RiskClass::Edit);
    let text = draft.preview.render();
    assert!(text.contains("+ To: ana@acme.io"), "{text}");
    assert!(
        text.contains("+ Subject: Re: Pricing for the pilot"),
        "{text}"
    );
    assert!(
        text.contains("+ Here it is."),
        "drafted body is previewed: {text}"
    );
    assert!(text.contains("Nothing is sent"), "{text}");
    // Preparing only drafted: nothing executed.
    assert_eq!(
        *exec.calls.lock().unwrap(),
        vec!["draft:Re: Pricing for the pilot".to_string()]
    );

    let archive = p.prepare_action(&r, &ana.id, 1).await.unwrap();
    assert_eq!(archive.preview.risk, RiskClass::Destroy);
    assert!(
        archive
            .preview
            .diff
            .iter()
            .any(|l| l.op == DiffOp::Remove && l.text.starts_with("Inbox:"))
    );

    let meeting = card_of(&p, CandidateKind::MeetingPrep);
    let note = p.prepare_action(&r, &meeting.id, 0).await.unwrap();
    assert!(
        note.preview
            .render()
            .contains("+ New note: Prep: Acme pilot kickoff")
    );

    let commitment = p
        .all_cards()
        .unwrap()
        .into_iter()
        .find(|c| c.title.contains("SOC 2"))
        .unwrap();
    let ev = p.prepare_action(&r, &commitment.id, 1).await.unwrap();
    assert!(matches!(ev.action, Action::CreateEvent(_)));
    assert_eq!(
        ev.preview.risk,
        RiskClass::Edit,
        "no attendees, no invitations"
    );
    let rem = p.prepare_action(&r, &commitment.id, 0).await.unwrap();
    assert!(rem.preview.render().contains("+ Reminder at"));

    // An event with attendees sends invitations: Send risk.
    let a = Action::CreateEvent(nugget_proactive::CreateEvent {
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
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let policy = Arc::new(RecordingPolicy::default());
    let exec = Arc::new(RecordingExec::default());
    let r = runner(policy.clone(), exec.clone()).with_audit(f.store.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let prepared = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    exec.calls.lock().unwrap().clear();

    // An approval for something else (tampered action) is refused.
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
    assert!(exec.calls.lock().unwrap().is_empty());
    assert!(
        policy.seen.lock().unwrap().is_empty(),
        "policy not even asked"
    );

    // Kill switch.
    *policy.halted.lock().unwrap() = true;
    assert_eq!(
        p.run_action(&r, &prepared, &approval, now()).await.unwrap(),
        ExecOutcome::Halted
    );
    *policy.halted.lock().unwrap() = false;

    // Policy denies.
    *policy.decision.lock().unwrap() = Some(Decision::Deny("mail blocked".into()));
    assert_eq!(
        p.run_action(&r, &prepared, &approval, now()).await.unwrap(),
        ExecOutcome::Denied("mail blocked".into())
    );
    // Policy wants confirmation: not executed until confirmed.
    *policy.decision.lock().unwrap() = Some(Decision::Confirm("external recipient".into()));
    assert_eq!(
        p.run_action(&r, &prepared, &approval, now()).await.unwrap(),
        ExecOutcome::NeedsConfirmation("external recipient".into())
    );
    assert!(exec.calls.lock().unwrap().is_empty());
    assert_ne!(p.card(&ana.id).unwrap().state, CardState::Done);

    let confirmed = prepared.approve(true);
    assert_eq!(
        p.run_action(&r, &prepared, &confirmed, now())
            .await
            .unwrap(),
        ExecOutcome::Executed("draft-1".into())
    );
    assert_eq!(
        *exec.calls.lock().unwrap(),
        vec!["save_draft:Re: Pricing for the pilot".to_string()]
    );
    let seen = policy.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].verb, "save_draft");
    assert_eq!(seen[0].declared_risk, RiskClass::Edit);
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
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
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
        .await
        .unwrap();
    let ExecOutcome::NeedsConfirmation(reason) = out else {
        panic!("expected a confirmation, got {out:?}");
    };
    assert!(reason.contains("inbox"), "{reason}");
    assert!(exec.calls.lock().unwrap().is_empty());
    let audit = f.store.recent_audit(10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "action.archive_thread.needs_confirmation")
    );

    // A policy denial still wins over the user's confirmation.
    *policy.decision.lock().unwrap() = Some(Decision::Deny("no mailbox edits".into()));
    assert_eq!(
        p.run_action(&r, &archive, &archive.approve(true), now())
            .await
            .unwrap(),
        ExecOutcome::Denied("no mailbox edits".into())
    );
    *policy.decision.lock().unwrap() = None;

    assert_eq!(
        p.run_action(&r, &archive, &archive.approve(true), now())
            .await
            .unwrap(),
        ExecOutcome::Executed("archived".into())
    );
    assert_eq!(
        *exec.calls.lock().unwrap(),
        vec!["archive:Pricing for the pilot".to_string()]
    );
    // Other actions need no extra confirmation.
    let draft = p.prepare_action(&r, &ana.id, 0).await;
    assert!(draft.is_err(), "the card is done");
    let reminder = Action::Reminder(nugget_proactive::Reminder {
        text: "x".into(),
        at: now(),
        about: None,
    });
    assert!(reminder.requires_confirmation().is_none());
}

#[tokio::test]
async fn missing_executor_is_unsupported() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let r = ActionRunner::new(Arc::new(RecordingPolicy::default()), Executors::default());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    // No drafter: cannot even prepare a reply.
    assert!(p.prepare_action(&r, &ana.id, 0).await.is_err());
    let archive = p.prepare_action(&r, &ana.id, 1).await.unwrap();
    let err = p
        .run_action(&r, &archive, &archive.approve(false), now())
        .await
        .unwrap_err();
    assert!(matches!(err, nugget_core::Error::Unsupported(_)));
}

// ---- judge ----

fn judge_reply(keys: &[String]) -> String {
    let items: Vec<_> = keys
        .iter()
        .map(|k| serde_json::json!({"key": k, "summary": "Worth doing today.", "suggested_action": "archive_thread", "confidence": 0.9}))
        .chain(std::iter::once(serde_json::json!({"key": "invented:1", "summary": "made up", "confidence": 1.0})))
        .collect();
    format!("```json\n{}\n```", serde_json::json!({"items": items}))
}

#[tokio::test]
async fn local_judge_enriches_but_never_invents() {
    let f = fixture();
    let keys: Vec<String> = Proactive::new(f.store.clone(), cfg())
        .candidates(now())
        .unwrap()
        .into_iter()
        .map(|c| c.dedup_key)
        .collect();
    let provider = FakeProvider::new(Locality::OnDevice, &judge_reply(&keys));
    let p = Proactive::new(f.store.clone(), cfg())
        .with_judge(Arc::new(LlmJudge::new(provider.clone())));
    let r = p.refresh(now()).await.unwrap();
    assert_eq!(r.judged, 7, "{r:?}");
    assert_eq!(r.created, 7, "the invented candidate was dropped");
    assert_eq!(r.judge_withheld, 0);
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let e = ana.enrichment.as_ref().unwrap();
    assert_eq!(e.summary, "Worth doing today.");
    // Suggested action moved first; the rank blends model and judge.
    assert_eq!(ana.actions[0].name(), "archive_thread");
    assert!((ana.rank - (0.8 * ana.score + 0.2 * 0.9)).abs() < 1e-9);
    {
        let req = provider.requests.lock().unwrap();
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].feature, "proactive_judge");
    }
    // Already enriched cards are not re-sent.
    let r = p.refresh(now()).await.unwrap();
    assert_eq!(r.judged, 0);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cloud_judge_is_gated() {
    let f = fixture();
    // Mark Ana's mail sensitive.
    put_mail(
        &f.store,
        Mail {
            key: "m-ana-1",
            from: "Ana Ruiz <ana@acme.io>",
            subject: "Pricing for the pilot",
            body: "Hi,\n\nCould you send over the pricing for the pilot? We'd like to decide this week.\n\nThanks, Ana",
            days_ago: 3.0,
            message_id: "<ana-1@acme.io>",
            sensitive: true,
            ..Default::default()
        },
    );
    let keys: Vec<String> = Proactive::new(f.store.clone(), cfg())
        .candidates(now())
        .unwrap()
        .into_iter()
        .map(|c| c.dedup_key)
        .collect();

    // Cloud not allowed: nothing is sent.
    let provider = FakeProvider::new(Locality::Cloud, &judge_reply(&keys));
    let p = Proactive::new(f.store.clone(), cfg())
        .with_card_store(Arc::new(MemCardStore::default()))
        .with_judge(Arc::new(LlmJudge::new(provider.clone())));
    let r = p.refresh(now()).await.unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(r.judge_withheld, 7);
    assert_eq!(r.created, 7, "cards are still made without the judge");

    // Cloud allowed: only non-sensitive candidates are sent.
    let mut c = cfg();
    c.judge_allow_cloud = true;
    let provider = FakeProvider::new(Locality::Cloud, &judge_reply(&keys));
    let p = Proactive::new(f.store.clone(), c)
        .with_card_store(Arc::new(MemCardStore::default()))
        .with_judge(Arc::new(LlmJudge::new(provider.clone())));
    let r = p.refresh(now()).await.unwrap();
    let reqs = provider.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let sent = serde_json::to_string(&reqs[0].messages).unwrap();
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

#[tokio::test]
async fn judge_failure_falls_back_to_the_model() {
    let f = fixture();
    let provider = FakeProvider::new(Locality::OnDevice, "not json at all");
    let p = Proactive::new(f.store.clone(), cfg()).with_judge(Arc::new(LlmJudge::new(provider)));
    let r = p.refresh(now()).await.unwrap();
    assert!(r.judge_error.is_some());
    assert_eq!(r.created, 7);
}

// ---- brief ----

#[tokio::test]
async fn morning_brief_structure() {
    let f = fixture();
    // Something today with an external attendee, plus the internal standup.
    put_event(
        &f.store,
        "ev-today",
        "Acme pilot call",
        now() + Duration::hours(5),
        &[ME, "ana@acme.io"],
    );
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let b = p.brief(now()).unwrap();
    assert_eq!(b.date, time::macros::date!(2026 - 10 - 28));
    let titles: Vec<&str> = b.meetings.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Standup", "Acme pilot call"]);
    let call = &b.meetings[1];
    assert_eq!(call.external, vec!["ana@acme.io".to_string()]);
    assert!(call.prep_card.is_some());
    assert!(call.prep.iter().any(|e| e.item == f.unanswered));
    assert!(b.meetings[0].prep_card.is_none());
    // Overdue: Ana's agreement.
    assert_eq!(b.overdue.len(), 1);
    assert!(b.overdue[0].title.contains("Signed pilot"));
    // Top: no meeting-prep or overdue cards, at most 5, ranked.
    assert!(!b.top.is_empty() && b.top.len() <= 5);
    assert!(b.top.iter().all(|c| c.kind != CandidateKind::MeetingPrep));
    assert!(b.top.windows(2).all(|w| w[0].rank >= w[1].rank));
    assert!(b.top.iter().all(|c| !c.factors.is_empty()));
    assert_eq!(b.renewals.len(), 2);
    assert!(b.open_cards >= 7);
    let md = b.to_markdown();
    assert!(
        md.contains("## Today's meetings") && md.contains("## Overdue"),
        "{md}"
    );
    serde_json::to_value(&b).unwrap();
}

// ---- stores ----

#[test]
fn card_stores_round_trip() {
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
    // A newer state version is refused rather than misread.
    store
        .set_cursor(
            &nugget_core::ids::SourceId::new("proactive:state"),
            serde_json::json!({"version": 99, "cards": {}, "model": {}}),
        )
        .unwrap();
    assert!(StoreCardStore::new(store).load().is_err());
}

// ---- approval binding (security review PA-4) ----

#[tokio::test]
async fn pa_run_action_executes_action_not_on_the_card() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let exec = Arc::new(RecordingExec::default());
    let r = runner(Arc::new(RecordingPolicy::default()), exec.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    let evil = Action::CreateEvent(nugget_proactive::CreateEvent {
        title: "x".into(),
        start: now(),
        end: now() + Duration::hours(1),
        attendees: vec!["attacker@evil.example".into()],
        description: None,
    });
    assert!(!ana.actions.contains(&evil));
    // The runner signs it, but the card does not offer it at that position.
    let forged = r.prepare(&ana.id, 0, &evil, &[]).await.unwrap();
    assert!(
        p.run_action(&r, &forged, &forged.approve(true), now())
            .await
            .is_err()
    );
    // Moving a real action to another position breaks its digest.
    let mut moved = p.prepare_action(&r, &ana.id, 0).await.unwrap();
    moved.action_index = 1;
    assert!(
        p.run_action(&r, &moved, &moved.approve(true), now())
            .await
            .is_err()
    );
    assert!(
        !exec
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("event"))
    );
    assert_ne!(p.card(&ana.id).unwrap().state, CardState::Done);
}

#[tokio::test]
async fn approvals_are_keyed_by_the_runner() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let exec = Arc::new(RecordingExec::default());
    let ours = runner(Arc::new(RecordingPolicy::default()), exec.clone());
    let theirs = runner(Arc::new(RecordingPolicy::default()), exec.clone());
    let c = card_of(&p, CandidateKind::Commitment);
    // Minted by another runner (another key): refused, even though it is the card's action.
    let foreign = p.prepare_action(&theirs, &c.id, 0).await.unwrap();
    assert!(
        p.run_action(&ours, &foreign, &foreign.approve(true), now())
            .await
            .is_err()
    );
    // The same key (e.g. the install's, after a restart) accepts it.
    let key = nugget_proactive::new_approval_key().unwrap();
    let a = runner(Arc::new(RecordingPolicy::default()), exec.clone()).with_key(key);
    let b = runner(Arc::new(RecordingPolicy::default()), exec.clone()).with_key(key);
    let prepared = p.prepare_action(&a, &c.id, 0).await.unwrap();
    assert!(matches!(
        p.run_action(&b, &prepared, &prepared.approve(true), now())
            .await
            .unwrap(),
        ExecOutcome::Executed(_)
    ));
}

#[tokio::test]
async fn an_approval_executes_once() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let exec = Arc::new(RecordingExec::default());
    let r = runner(Arc::new(RecordingPolicy::default()), exec.clone());
    let c = card_of(&p, CandidateKind::Commitment);
    let prepared = p.prepare_action(&r, &c.id, 0).await.unwrap();
    let approval = prepared.approve(true);
    assert!(matches!(
        r.execute(&prepared, &approval).await.unwrap(),
        ExecOutcome::Executed(_)
    ));
    let err = r.execute(&prepared, &approval).await.unwrap_err();
    assert!(err.to_string().contains("already used"), "{err}");
    assert_eq!(exec.calls.lock().unwrap().len(), 1);
}

/// Reminders that wait for a signal, so two runs overlap.
struct GatedReminders {
    gate: tokio::sync::Notify,
    calls: std::sync::Mutex<u32>,
}

#[async_trait::async_trait]
impl nugget_proactive::ReminderExecutor for GatedReminders {
    async fn create_reminder(&self, _: &nugget_proactive::Reminder) -> nugget_core::Result<String> {
        self.gate.notified().await;
        *self.calls.lock().unwrap() += 1;
        Ok("rem-1".into())
    }
}

#[tokio::test]
async fn concurrent_runs_on_a_card_execute_once() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let gated = Arc::new(GatedReminders {
        gate: tokio::sync::Notify::new(),
        calls: std::sync::Mutex::new(0),
    });
    let r = ActionRunner::new(
        Arc::new(RecordingPolicy::default()),
        Executors {
            reminders: Some(gated.clone()),
            ..Default::default()
        },
    );
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
