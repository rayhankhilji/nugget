mod common;

use common::*;
use nugget_proactive::{Action, Candidate, CandidateKind, Proactive};
use time::Duration;

fn by_kind_at(p: &Proactive, kind: CandidateKind, at: time::OffsetDateTime) -> Vec<Candidate> {
    let all = p.candidates(at).unwrap();
    all.into_iter().filter(|c| c.kind == kind).collect()
}

fn by_kind(p: &Proactive, kind: CandidateKind) -> Vec<Candidate> {
    by_kind_at(p, kind, now())
}

/// A store with only these mails (see [`put_mails`]).
fn with_mails(table: &str) -> Proactive {
    let store = nugget_store::Store::open_in_memory().unwrap();
    put_mails(&store, table);
    Proactive::new(store, cfg())
}

fn keys(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.evidence[0].item.key.as_str()).collect()
}

#[test]
fn unanswered_threads_are_precise() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    // Only Ana: Bob was answered, newsletters/automated/bulk excluded, too old and too fresh out.
    let c = by_kind(&p, CandidateKind::UnansweredThread);
    assert_eq!(keys(&c), ["m-ana-1"]);
    let ana = &c[0];
    assert!(ana.title.contains("Ana Ruiz"), "{}", ana.title);
    assert!(
        ana.evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .contains("pricing")
    );
    assert_eq!((ana.features.question, ana.features.direct), (1.0, 1.0));
    assert!(ana.features.importance > 0.3, "graph relationship");
    assert!(matches!(ana.actions[0], Action::DraftReply(_)));
    assert!(matches!(ana.actions[1], Action::ArchiveThread(_)));
    // Newsletters never appear as people anywhere.
    for c in p.candidates(now()).unwrap() {
        for e in &c.evidence {
            let k = e.item.key.as_str();
            assert!(
                !["m-news-1", "m-news-2", "m-noreply", "m-bulk"].contains(&k),
                "{k}"
            );
        }
    }
    // The user's addresses are inferred from sent mail.
    let p = Proactive::new(f.store.clone(), Default::default());
    assert_eq!(p.corpus().unwrap().user_addresses, vec![ME.to_string()]);
    assert_eq!(by_kind(&p, CandidateKind::UnansweredThread).len(), 1);
}

#[test]
fn commitments_from_graph() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::Commitment);
    // Far-future and done commitments are out.
    assert_eq!(c.len(), 2, "{c:#?}");
    let mine = c.iter().find(|c| c.title.contains("SOC 2")).unwrap();
    assert!(mine.title.starts_with("Due 2026-10-30"), "{}", mine.title);
    assert_eq!(mine.features.direct, 1.0);
    let quote = Some("send the SOC 2 report");
    assert!(mine.evidence.iter().any(|e| e.quote.as_deref() == quote));
    assert!(matches!(mine.actions[0], Action::Reminder(_)));
    assert!(matches!(mine.actions[1], Action::CreateEvent(_)));
    let theirs = c.iter().find(|c| c.title.contains("Signed pilot")).unwrap();
    assert!(
        theirs.title.starts_with("Waiting on Ana Ruiz"),
        "{}",
        theirs.title
    );
    assert!(theirs.features.urgency > 1.0, "overdue");
}

#[test]
fn meetings_with_external_attendees() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    // Standup is internal; the quarterly review is 5 days out.
    let c = by_kind(&p, CandidateKind::MeetingPrep);
    assert_eq!(c.len(), 1, "{c:#?}");
    let m = &c[0];
    assert!(m.title.contains("Acme pilot kickoff"));
    assert_eq!(m.people, vec!["ana@acme.io".to_string()]);
    assert_eq!(m.evidence[0].item, f.meeting);
    // The related thread with Ana and the related note.
    assert!(m.evidence.iter().any(|e| e.item == f.unanswered));
    assert!(
        m.evidence.iter().any(|e| e.item == f.note),
        "{:#?}",
        m.evidence
    );
    let Action::ResearchNote(n) = &m.actions[0] else {
        panic!()
    };
    assert!(n.body.contains("Pricing for the pilot") && n.body.contains("ana"));
}

#[test]
fn renewals_trials_and_follow_ups() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    let c = by_kind(&p, CandidateKind::Renewal);
    assert_eq!(c.len(), 2, "{c:#?}");
    let renew = c.iter().find(|c| c.evidence[0].item == f.renewal).unwrap();
    assert!(renew.title.contains("renews 2026-11-02") && renew.title.contains("$49.00"));
    let trial = c.iter().find(|c| c.evidence[0].item == f.trial).unwrap();
    assert!(trial.title.contains("trial ends") && trial.title.contains("€9.99"));
    assert_eq!(
        trial.due.unwrap().date(),
        time::macros::date!(2026 - 10 - 30)
    );

    // The kept promise (a later sent message in the thread) is out.
    let c = by_kind(&p, CandidateKind::FollowUp);
    assert_eq!(keys(&c), ["m-promise"]);
    assert!(
        c[0].title.contains("Frank Ode") && c[0].title.contains("I'll send the deck by Friday")
    );
    // Written Monday 2026-10-26, "by Friday" → 2026-10-30.
    assert_eq!(
        c[0].due.unwrap().date(),
        time::macros::date!(2026 - 10 - 30)
    );
    let Action::DraftReply(d) = &c[0].actions[0] else {
        panic!()
    };
    assert_eq!(d.to, vec!["frank@partner.org".to_string()]);

    // Deterministic, with unique keys.
    let a = p.candidates(now()).unwrap();
    assert_eq!(a, p.candidates(now()).unwrap());
    let unique: std::collections::HashSet<_> = a.iter().map(|c| &c.dedup_key).collect();
    assert_eq!(unique.len(), a.len());
}

#[test]
fn acknowledgements_and_calendar_replies_need_no_reply() {
    let p = with_mails(
        "\
m-done|Lena <lena@acme.io>||Re: Pager rotation|2||||Done, you're on from the 1st.
m-fyi|Lena <lena@acme.io>||Minutes|2||||Please find attached the minutes from the vendor call.
m-cancel|Omar <omar@woodgrove.io>||Cancelled: Hardware walkthrough|2||||I'm travelling, so I've cancelled our meeting. Could we redo it next month?
m-role|Acme Events <events@acme-hub.io>||Join us next week?|2||||Will you join us for the meetup?
m-ask|Lena <lena@acme.io>||On-call swap|2||||Can you swap on-call with me on the 26th?",
    );
    let c = by_kind(&p, CandidateKind::UnansweredThread);
    assert_eq!(keys(&c), ["m-ask"]);
    assert_eq!(c[0].features.sender.as_deref(), Some("lena@acme.io"));
}

#[test]
fn unanswered_questions_of_the_user() {
    // Asked 4 days ago with no answer: a card; answered, asked yesterday, or a statement: none.
    let p = with_mails(
        "\
m-q||Omar <omar@woodgrove.io>|Hardware quote|4||||Hi Omar, could you send an updated quote for the 20 laptops? Thanks
m-q2||lena@acme.io|Pager|4||||Can you add me to the pager rotation?
m-a2|lena@acme.io||Re: Pager|3||m-q2||Done, you're on from the 1st.
m-q3||kim@northwind.io|Export|1||||Could your team send the export?
m-s||kim@northwind.io|Recap|5||||Quick recap: we agreed to ship the beta next month.",
    );
    let c = by_kind(&p, CandidateKind::FollowUp);
    assert_eq!(keys(&c), ["m-q"]);
    assert!(c[0].dedup_key.starts_with("awaiting:"));
    assert!(c[0].title.contains("Omar"), "{}", c[0].title);
    let Action::DraftReply(d) = &c[0].actions[0] else {
        panic!()
    };
    assert_eq!(d.to, vec!["omar@woodgrove.io".to_string()]);
}

#[test]
fn dated_promises_wait_for_their_deadline() {
    let p = with_mails(
        "m-far||dana@acme.io|Deck|1||||Happy to. I'll send the review deck by 2026-11-06.",
    );
    assert!(by_kind(&p, CandidateKind::FollowUp).is_empty());
    // Two days before the deadline it is a card.
    let later = now() + Duration::days(7);
    assert_eq!(by_kind_at(&p, CandidateKind::FollowUp, later).len(), 1);
}

#[test]
fn renewals_need_a_vendor_the_user_pays() {
    let p = with_mails(
        "\
m-scam|Billing <billing@secure-renewal-center.io>||Final notice: auto-renewal|0.5||||Your auto-renewal will be charged tomorrow. Confirm your billing details?
m-receipt|Receipts <receipts@cloudvault.io>||Your receipt from CloudVault|360||||Thanks for your purchase. CloudVault annual plan: $99.00. Your annual subscription renews on 2026-11-02.
m-notice|Billing <billing@mail.cloudvault.io>||Your plan renews soon|1||||Your annual plan renews on November 2, 2026 for $99.00.",
    );
    // One card for CloudVault (receipt and notice merge by vendor), none for the scam.
    let c = by_kind(&p, CandidateKind::Renewal);
    assert_eq!(keys(&c), ["m-notice"]);
    // The key names the vendor, not the date: next cycle's notice maps to the same card.
    let next = by_kind_at(&p, CandidateKind::Renewal, now() + Duration::days(1));
    assert_eq!(next[0].dedup_key, c[0].dedup_key);
}

#[test]
fn dated_todos_in_notes() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let text = "- [ ] Send the landlord the lease extension (due 2026-10-30)\n- [ ] Water the \
plants\n- [x] Submit the expenses (due 2026-10-29)\n- [ ] Book flights (due 2026-12-01)";
    let item =
        nugget_store::NewItem::new("files:notes", "admin.md", nugget_store::content_hash(text))
            .with_title("Personal admin")
            .with_mime("text/markdown")
            .with_text(text)
            .with_modified_at(now() - Duration::days(5))
            .with_metadata(serde_json::json!({"mime": "text/markdown", "sensitive": false}));
    store.upsert_item(&item).unwrap();
    let p = Proactive::new(store, cfg());
    let c = by_kind(&p, CandidateKind::Commitment);
    assert_eq!(keys(&c), ["admin.md"]);
    assert!(c[0].title.starts_with("Due 2026-10-30") && c[0].title.contains("lease extension"));
    assert!(c[0].dedup_key.starts_with("todo:"));
}

/// GG4-7: an HR letter asks nothing ("please don't share" is no request, `hr@` is a role
/// mailbox); a confidential ask from a person is a sensitive card.
#[test]
fn confidential_mail_is_no_ask_and_sensitive() {
    let p = with_mails(
        "\
m-hr|Larkspur People Team <hr@larkspur.example>||Confidential: your 2027 compensation|2||||Your salary for 2027 will be $118,500 (+6%). Please don't share this letter.
m-cfo|Rita Cole <rita@larkspur.example>||Board numbers|2||||Confidential. Can you check the 2027 salary bands before Friday?",
    );
    let c = by_kind(&p, CandidateKind::UnansweredThread);
    assert_eq!(keys(&c), ["m-cfo"]);
    assert!(c[0].sensitive);
}

/// GG4-12: a nudge keeps the card (same key) that the first ask started.
#[test]
fn a_nudge_keeps_the_card() {
    let ask = "m-ask|Priya <priya@x.example>||Budget|3||||Can you approve the budget?";
    let key = |p: &Proactive| {
        by_kind(p, CandidateKind::UnansweredThread)[0]
            .dedup_key
            .clone()
    };
    let before = key(&with_mails(ask));
    let nudged = with_mails(&format!(
        "{ask}\nm-nudge|Priya <priya@x.example>||Re: Budget|0.5||m-ask||Any update?"
    ));
    assert_eq!(key(&nudged), before);
}

/// GG4-13: on Cc only, a question for someone else is no reply card.
#[test]
fn cc_only_ask_for_someone_else() {
    let p = with_mails(
        "\
m-cc|Priya <priya@x.example>|theo@x.example|Invoice|2|||cc|Theo, can you approve the invoice by Friday?
m-sent|Maya Lin <me@nugget.dev>|priya@x.example|Hi|9|SENT|||Hello.
m-cc2|Priya <priya@x.example>|theo@x.example|Plan|2|||cc|Maya, can you check the plan?",
    );
    assert_eq!(
        keys(&by_kind(&p, CandidateKind::UnansweredThread)),
        ["m-cc2"]
    );
}

/// GG4-14: a refresh minutes later offers the same actions, so an open preview still matches.
#[test]
fn actions_are_stable_across_refreshes() {
    let p = Proactive::new(fixture().store.clone(), cfg());
    let actions = |t| -> Vec<_> {
        p.candidates(t)
            .unwrap()
            .into_iter()
            .flat_map(|c| c.actions)
            .collect()
    };
    let first = actions(now() + Duration::minutes(1));
    assert!(first.iter().any(|a| matches!(a, Action::Reminder(_))));
    assert_eq!(first, actions(now() + Duration::minutes(3)));
}
