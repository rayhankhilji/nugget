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

fn store_with(mails: Vec<Mail<'_>>) -> nugget_store::Store {
    let store = nugget_store::Store::open_in_memory().unwrap();
    for m in mails {
        put_mail(&store, m);
    }
    store
}

#[test]
fn acknowledgements_and_calendar_replies_need_no_reply() {
    let store = store_with(vec![
        Mail {
            key: "m-done",
            from: "Lena <lena@acme.io>",
            subject: "Re: Pager rotation",
            body: "Done, you're on from the 1st.",
            message_id: "<done@acme.io>",
            ..Default::default()
        },
        Mail {
            key: "m-fyi",
            from: "Lena <lena@acme.io>",
            subject: "Minutes",
            body: "Please find attached the minutes from the vendor call.",
            message_id: "<fyi@acme.io>",
            ..Default::default()
        },
        Mail {
            key: "m-cancel",
            from: "Omar <omar@woodgrove.io>",
            subject: "Cancelled: Hardware walkthrough",
            body: "I'm travelling, so I've cancelled our meeting. Could we redo it next month?",
            message_id: "<cancel@woodgrove.io>",
            ..Default::default()
        },
        Mail {
            key: "m-role",
            from: "Acme Events <events@acme-hub.io>",
            subject: "Join us next week?",
            body: "Will you join us for the meetup?",
            message_id: "<role@acme-hub.io>",
            ..Default::default()
        },
        Mail {
            key: "m-ask",
            from: "Lena <lena@acme.io>",
            subject: "On-call swap",
            body: "Can you swap on-call with me on the 26th?",
            message_id: "<ask@acme.io>",
            ..Default::default()
        },
    ]);
    let p = Proactive::new(store, cfg());
    let c = by_kind(&p, CandidateKind::UnansweredThread);
    let keys: Vec<&str> = c.iter().map(|c| c.evidence[0].item.key.as_str()).collect();
    assert_eq!(keys, vec!["m-ask"]);
    assert_eq!(c[0].features.sender.as_deref(), Some("lena@acme.io"));
}

#[test]
fn unanswered_questions_of_the_user() {
    let store = store_with(vec![
        // Asked 4 days ago, no answer: a card.
        Mail {
            key: "m-q",
            from: ME,
            to: &["Omar <omar@woodgrove.io>"],
            subject: "Hardware quote",
            body: "Hi Omar, could you send an updated quote for the 20 laptops? Thanks",
            days_ago: 4.0,
            message_id: "<q@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
        // Asked 4 days ago, answered: none.
        Mail {
            key: "m-q2",
            from: ME,
            to: &["lena@acme.io"],
            subject: "Pager",
            body: "Can you add me to the pager rotation?",
            days_ago: 4.0,
            message_id: "<q2@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
        Mail {
            key: "m-a2",
            from: "lena@acme.io",
            subject: "Re: Pager",
            body: "Done, you're on from the 1st.",
            days_ago: 3.0,
            message_id: "<a2@acme.io>",
            in_reply_to: Some("<q2@nugget.dev>"),
            ..Default::default()
        },
        // Asked yesterday: too early. A statement 5 days ago: nothing to wait for.
        Mail {
            key: "m-q3",
            from: ME,
            to: &["kim@northwind.io"],
            subject: "Export",
            body: "Could your team send the export?",
            days_ago: 1.0,
            message_id: "<q3@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
        Mail {
            key: "m-s",
            from: ME,
            to: &["kim@northwind.io"],
            subject: "Recap",
            body: "Quick recap: we agreed to ship the beta next month.",
            days_ago: 5.0,
            message_id: "<s@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
    ]);
    let p = Proactive::new(store, cfg());
    let c = by_kind(&p, CandidateKind::FollowUp);
    assert_eq!(c.len(), 1, "{c:#?}");
    assert!(c[0].dedup_key.starts_with("awaiting:"));
    assert_eq!(c[0].evidence[0].item.key.as_str(), "m-q");
    assert!(c[0].title.contains("Omar"), "{}", c[0].title);
    let Action::DraftReply(d) = &c[0].actions[0] else {
        panic!()
    };
    assert_eq!(d.to, vec!["omar@woodgrove.io".to_string()]);
}

#[test]
fn dated_promises_wait_for_their_deadline() {
    let store = store_with(vec![Mail {
        key: "m-far",
        from: ME,
        to: &["dana@acme.io"],
        subject: "Deck",
        body: "Happy to. I'll send the review deck by 2026-11-06.",
        days_ago: 1.0,
        message_id: "<far@nugget.dev>",
        labels: &["SENT"],
        ..Default::default()
    }]);
    let p = Proactive::new(store.clone(), cfg());
    assert!(by_kind(&p, CandidateKind::FollowUp).is_empty());
    // Two days before the deadline it is a card.
    let later = now() + time::Duration::days(7);
    let c: Vec<_> = p
        .candidates(later)
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CandidateKind::FollowUp)
        .collect();
    assert_eq!(c.len(), 1, "{c:#?}");
}

#[test]
fn renewals_need_a_vendor_the_user_pays() {
    let store = store_with(vec![
        Mail {
            key: "m-scam",
            from: "Billing <billing@secure-renewal-center.io>",
            subject: "Final notice: auto-renewal",
            body: "Your auto-renewal will be charged tomorrow. Confirm your billing details?",
            days_ago: 0.5,
            message_id: "<scam@x.io>",
            ..Default::default()
        },
        Mail {
            key: "m-receipt",
            from: "Receipts <receipts@cloudvault.io>",
            subject: "Your receipt from CloudVault",
            body: "Thanks for your purchase. CloudVault annual plan: $99.00. Your annual subscription renews on 2026-11-02.",
            days_ago: 360.0,
            message_id: "<r@cloudvault.io>",
            ..Default::default()
        },
        Mail {
            key: "m-notice",
            from: "Billing <billing@mail.cloudvault.io>",
            subject: "Your plan renews soon",
            body: "Your annual plan renews on November 2, 2026 for $99.00.",
            days_ago: 1.0,
            message_id: "<n@cloudvault.io>",
            ..Default::default()
        },
    ]);
    let p = Proactive::new(store, cfg());
    let c = by_kind(&p, CandidateKind::Renewal);
    // One card for CloudVault (receipt and notice merge by vendor), none for the scam.
    assert_eq!(c.len(), 1, "{c:#?}");
    assert_eq!(c[0].evidence[0].item.key.as_str(), "m-notice");
    // The key names the vendor, not the date: next cycle's notice maps to the same card.
    let next_cycle = p
        .candidates(now() + time::Duration::days(1))
        .unwrap()
        .into_iter()
        .find(|x| x.kind == CandidateKind::Renewal)
        .unwrap();
    assert_eq!(next_cycle.dedup_key, c[0].dedup_key);
}

#[test]
fn dated_todos_in_notes() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let text = "- [ ] Send the landlord the lease extension (due 2026-10-30)\n- [ ] Water the plants\n- [x] Submit the expenses (due 2026-10-29)\n- [ ] Book flights (due 2026-12-01)";
    let item =
        nugget_store::NewItem::new("files:notes", "admin.md", nugget_store::content_hash(text))
            .with_title("Personal admin")
            .with_mime("text/markdown")
            .with_text(text)
            .with_modified_at(now() - time::Duration::days(5))
            .with_metadata(serde_json::json!({"mime": "text/markdown", "sensitive": false}));
    store.upsert_item(&item).unwrap();
    let p = Proactive::new(store, cfg());
    let c = by_kind(&p, CandidateKind::Commitment);
    assert_eq!(c.len(), 1, "{c:#?}");
    assert!(c[0].title.starts_with("Due 2026-10-30"), "{}", c[0].title);
    assert!(c[0].title.contains("lease extension"));
    assert_eq!(c[0].evidence[0].item.key.as_str(), "admin.md");
    assert!(c[0].dedup_key.starts_with("todo:"));
}
