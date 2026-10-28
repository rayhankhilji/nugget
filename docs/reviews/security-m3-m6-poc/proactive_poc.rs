
// ---- PA PoC (temporary) ----
#[tokio::test]
async fn pa_run_action_executes_action_not_on_the_card() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let policy = Arc::new(RecordingPolicy::default());
    let exec = Arc::new(RecordingExec::default());
    let r = runner(policy.clone(), exec.clone());
    let ana = card_of(&p, CandidateKind::UnansweredThread);
    // Not one of the card's actions: an invite with an attacker attendee.
    let evil = Action::CreateEvent(nugget_proactive::CreateEvent {
        title: "x".into(),
        start: now(),
        end: now() + Duration::hours(1),
        attendees: vec!["attacker@evil.example".into()],
        description: None,
    });
    assert!(!ana.actions.contains(&evil));
    let forged = r.prepare(&ana.id, &evil, &[]).await.unwrap();
    let out = p.run_action(&r, &forged, &forged.approve(true), now()).await;
    assert!(out.is_err(), "executed an action the card never offered: {out:?} calls={:?}", exec.calls.lock().unwrap());
}
