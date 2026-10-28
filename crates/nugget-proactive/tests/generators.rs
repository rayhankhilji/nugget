mod common;

use common::*;
use nugget_proactive::{Action, CandidateKind, Proactive};

fn by_kind(p: &Proactive, kind: CandidateKind) -> Vec<nugget_proactive::Candidate> {
    p.candidates(now())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == kind)
        .collect()
}

#[test]
fn unanswered_threads_are_precise() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::UnansweredThread);
    // Only Ana: Bob was answered, newsletters/automated/bulk excluded, too old and too fresh out.
    assert_eq!(
        c.len(),
        1,
        "{:#?}",
        c.iter().map(|c| &c.title).collect::<Vec<_>>()
    );
    let ana = &c[0];
    assert!(ana.title.contains("Ana Ruiz"), "{}", ana.title);
    assert_eq!(ana.evidence[0].item, f.unanswered);
    assert!(
        ana.evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .contains("pricing")
    );
    assert_eq!(ana.features.question, 1.0);
    assert_eq!(ana.features.direct, 1.0);
    assert!(
        ana.features.importance > 0.3,
        "graph relationship: {}",
        ana.features.importance
    );
    assert!(matches!(ana.actions[0], Action::DraftReply(_)));
    assert!(matches!(ana.actions[1], Action::ArchiveThread(_)));
    // None of the excluded items appear anywhere as unanswered evidence.
    assert!(c.iter().all(|x| x.evidence[0].item != f.answered_inbound));
}

#[test]
fn newsletters_never_appear_as_people() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let all = p.candidates(now()).unwrap();
    for c in &all {
        if matches!(
            c.kind,
            CandidateKind::UnansweredThread | CandidateKind::FollowUp
        ) {
            for e in &c.evidence {
                let k = e.item.key.as_str();
                assert!(
                    !["m-news-1", "m-news-2", "m-noreply", "m-bulk"].contains(&k),
                    "{k} leaked into {c:?}"
                );
            }
        }
    }
}

#[test]
fn user_addresses_are_inferred_from_sent_mail() {
    let f = fixture();
    let p = Proactive::new(
        f.store.clone(),
        nugget_proactive::ProactiveConfig::default(),
    );
    let corpus = p.corpus().unwrap();
    assert_eq!(corpus.user_addresses, vec![ME.to_string()]);
    assert_eq!(by_kind(&p, CandidateKind::UnansweredThread).len(), 1);
}

#[test]
fn commitments_from_graph() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::Commitment);
    let titles: Vec<&str> = c.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(c.len(), 2, "{titles:#?}");
    let mine = c.iter().find(|c| c.title.contains("SOC 2")).unwrap();
    assert!(mine.title.starts_with("Due 2026-10-30"), "{}", mine.title);
    assert_eq!(mine.features.direct, 1.0);
    assert!(
        mine.evidence
            .iter()
            .any(|e| e.quote.as_deref() == Some("send the SOC 2 report"))
    );
    assert!(matches!(mine.actions[0], Action::Reminder(_)));
    assert!(matches!(mine.actions[1], Action::CreateEvent(_)));
    let theirs = c.iter().find(|c| c.title.contains("Signed pilot")).unwrap();
    assert!(
        theirs.title.starts_with("Waiting on Ana Ruiz"),
        "{}",
        theirs.title
    );
    assert!(theirs.features.urgency > 1.0, "overdue");
    // Far-future and done commitments are out.
    assert!(
        !titles
            .iter()
            .any(|t| t.contains("Annual plan") || t.contains("Old task"))
    );
}

#[test]
fn meetings_with_external_attendees() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::MeetingPrep);
    // Standup is internal; the quarterly review is 5 days out.
    assert_eq!(c.len(), 1, "{c:#?}");
    let m = &c[0];
    assert!(m.title.contains("Acme pilot kickoff"));
    assert_eq!(m.people, vec!["ana@acme.io".to_string()]);
    assert_eq!(m.evidence[0].item, f.meeting);
    // Related thread with Ana and the related note.
    assert!(m.evidence.iter().any(|e| e.item == f.unanswered));
    assert!(
        m.evidence.iter().any(|e| e.item == f.note),
        "{:#?}",
        m.evidence
    );
    let Action::ResearchNote(n) = &m.actions[0] else {
        panic!()
    };
    assert!(n.body.contains("Pricing for the pilot"));
    assert!(n.body.contains("ana"));
}

#[test]
fn renewals_and_trials() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::Renewal);
    assert_eq!(c.len(), 2, "{c:#?}");
    let renew = c.iter().find(|c| c.evidence[0].item == f.renewal).unwrap();
    assert!(renew.title.contains("renews 2026-11-02"), "{}", renew.title);
    assert!(renew.title.contains("$49.00"), "{}", renew.title);
    let trial = c.iter().find(|c| c.evidence[0].item == f.trial).unwrap();
    assert!(trial.title.contains("trial ends"), "{}", trial.title);
    assert!(trial.title.contains("€9.99"), "{}", trial.title);
    // 0.5 days ago + 3 days.
    assert_eq!(
        trial.due.unwrap().date(),
        time::macros::date!(2026 - 10 - 30)
    );
}

#[test]
fn follow_ups_from_sent_promises() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::FollowUp);
    // The kept promise (later sent message in the thread) is out.
    assert_eq!(c.len(), 1, "{c:#?}");
    let fu = &c[0];
    assert_eq!(fu.evidence[0].item, f.promise);
    assert!(fu.title.contains("Frank Ode"), "{}", fu.title);
    assert!(fu.title.contains("I'll send the deck by Friday"));
    // Written Monday 2026-10-26, "by Friday" → 2026-10-30.
    assert_eq!(fu.due.unwrap().date(), time::macros::date!(2026 - 10 - 30));
    let Action::DraftReply(d) = &fu.actions[0] else {
        panic!()
    };
    assert_eq!(d.to, vec!["frank@partner.org".to_string()]);
}

#[test]
fn generators_are_deterministic() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let a = p.candidates(now()).unwrap();
    let b = p.candidates(now()).unwrap();
    assert_eq!(a, b);
    let keys: std::collections::HashSet<_> = a.iter().map(|c| &c.dedup_key).collect();
    assert_eq!(keys.len(), a.len());
}
