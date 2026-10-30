//! Learning from feedback through the engine: per sender, kind and thread, explained on cards,
//! and forgotten on reset.

mod common;

use std::sync::Arc;

use common::*;
use nugget_proactive::cards::CardStore;
use nugget_proactive::{
    CandidateKind, Card, CardState, Direction, DismissReason, MemCardStore, Proactive,
    ProactiveState, ResetScope, Scope, ScoringModel,
};
use time::Duration;

fn find(p: &Proactive, pred: impl Fn(&Card) -> bool) -> Card {
    p.all_cards()
        .unwrap()
        .into_iter()
        .find(|c| pred(c))
        .expect("card")
}

fn in_inbox(p: &Proactive, id: &str) -> bool {
    p.inbox().unwrap().iter().any(|c| c.id == id)
}

/// A second, unrelated question from Ana (another thread).
fn ana_again(f: &Fixture) {
    put_mail(
        &f.store,
        Mail {
            key: "m-ana-contract",
            from: "Ana Ruiz <ana@acme.io>",
            subject: "Contract draft",
            body: "Could you review the contract draft before Friday?",
            days_ago: 1.5,
            message_id: "<ana-contract@acme.io>",
            ..Default::default()
        },
    );
}

fn is_contract(c: &Card) -> bool {
    c.title.contains("Contract draft")
}

#[tokio::test]
async fn a_sender_dismissed_as_not_important_ranks_lower_and_reset_brings_it_back() {
    // Control: the same world, no feedback.
    let control = fixture();
    ana_again(&control);
    let pc = Proactive::new(control.store.clone(), cfg());
    let later = now() + Duration::hours(1);
    pc.refresh(later).await.unwrap();
    let base = find(&pc, is_contract);
    assert!(base.learned.is_empty());

    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let ana = find(&p, |c| c.title.contains("Pricing for the pilot"));
    p.dismiss(&ana.id, DismissReason::NotImportant, now())
        .unwrap();

    ana_again(&f);
    p.refresh(later).await.unwrap();
    let card = find(&p, is_contract);
    assert!(
        card.rank < base.rank - 0.1,
        "{} vs {}",
        card.rank,
        base.rank
    );
    // Explained: the sender is the strongest effect, with the count and reason behind it.
    let top = &card.learned[0];
    assert_eq!(top.scope, Scope::Sender);
    assert_eq!(top.key, "ana@acme.io");
    assert_eq!(top.direction, Direction::Less);
    assert_eq!((top.accepted, top.dismissed), (0, 1));
    assert_eq!(top.reason.as_deref(), Some("not_important"));
    assert!(
        card.learned
            .iter()
            .any(|e| e.scope == Scope::Domain && e.key == "acme.io")
    );
    assert!(card.factors.iter().any(|f| f.name == "learned"));
    let total: f64 = card.learned.iter().map(|e| e.contribution).sum();
    assert!(total < -1.0);
    // Listed for the settings page.
    let listed = p.learned(later).unwrap();
    assert_eq!(listed[0].scope, Scope::Sender);
    assert_eq!(listed[0].key, "ana@acme.io");
    assert!((listed[0].dismissed - 1.0).abs() < 1e-3);
    // The reason is in the feedback log.
    let state = nugget_proactive::StoreCardStore::new(f.store.clone())
        .load()
        .unwrap();
    assert_eq!(
        state.feedback_log.last().unwrap().reason.as_deref(),
        Some("not_important")
    );

    // Forgetting the sender lifts the card; forgetting everything restores the prior exactly.
    let n = p
        .reset_learning(&ResetScope::One(Scope::Sender, "Ana@Acme.io".into()), later)
        .unwrap();
    assert_eq!(n, 1);
    let card2 = find(&p, is_contract);
    assert!(card2.rank > card.rank);
    assert!(card2.learned.iter().all(|e| e.scope != Scope::Sender));
    let n = p.reset_learning(&ResetScope::All, later).unwrap();
    assert!(n >= 2, "{n}");
    assert!(p.learned(later).unwrap().is_empty());
    assert_eq!(p.model().unwrap(), ScoringModel::default());
    let card3 = find(&p, is_contract);
    assert!(card3.learned.is_empty());
    assert!((card3.rank - base.rank).abs() < 1e-9);
    // The audit of what the user did is kept.
    assert_eq!(p.metrics().unwrap().dismissed, 1);
}

/// The user promised Hana the deck and asked her something in the same mail, four days ago, with
/// no answer: one thread, two cards (the promise and the question awaiting an answer).
fn deck_mail(f: &Fixture) {
    put_mail(
        &f.store,
        Mail {
            key: "m-deck",
            from: ME,
            to: &["Hana Ito <hana@studio.test>"],
            subject: "Deck and address",
            body: "Hi Hana,\n\nI'll send the deck by Friday. Could you confirm the shipping address?\n\nBest",
            days_ago: 4.0,
            message_id: "<me-deck@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
    );
}

fn deck_cards(p: &Proactive) -> (Card, Card) {
    let promise = find(p, |c| {
        c.kind == CandidateKind::FollowUp && c.title.contains("I'll send the deck")
    });
    let question = find(p, |c| {
        c.kind == CandidateKind::FollowUp && c.title.contains("No answer from")
    });
    (promise, question)
}

#[tokio::test]
async fn already_handled_stops_the_thread_and_reset_brings_it_back() {
    let f = fixture();
    deck_mail(&f);
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let (promise, question) = deck_cards(&p);
    assert!(promise.features.thread.is_some());
    assert_eq!(promise.features.thread, question.features.thread);
    assert!(in_inbox(&p, &question.id));

    // "I sent it": the thread is handled, and the question in that same mail goes with it.
    p.dismiss(&promise.id, DismissReason::AlreadyHandled, now())
        .unwrap();
    let q = p.card(&question.id).unwrap();
    // Hidden, explained, kept in the card set; the person is not penalised.
    assert_eq!(q.state, CardState::New);
    assert!(!in_inbox(&p, &q.id));
    assert_eq!(q.learned[0].direction, Direction::Hidden);
    assert_eq!(q.learned[0].scope, Scope::Thread);
    assert_eq!(q.learned[0].dismissed, 1);
    assert_eq!(q.learned[0].reason.as_deref(), Some("already_handled"));
    assert!(q.learned.iter().all(|e| e.scope != Scope::Sender));
    assert!(
        !p.brief(now())
            .unwrap()
            .top
            .iter()
            .any(|c| c.card_id == q.id)
    );
    // Still hidden after the next refresh.
    p.refresh(now() + Duration::hours(2)).await.unwrap();
    assert!(!in_inbox(&p, &q.id));
    let listed = p.learned(now()).unwrap();
    assert!(listed[0].suppresses);
    assert_eq!(listed[0].scope, Scope::Thread);
    assert_eq!(listed[0].label.as_deref(), Some(promise.title.as_str()));

    // Forgetting the thread brings the card back.
    let key = q.features.thread.clone().unwrap();
    assert_eq!(
        p.reset_learning(&ResetScope::One(Scope::Thread, key), now())
            .unwrap(),
        1
    );
    assert!(in_inbox(&p, &q.id));
    assert!(p.card(&q.id).unwrap().learned.is_empty());
}

#[tokio::test]
async fn a_handled_question_keeps_the_promise_and_a_new_message_surfaces() {
    let f = fixture();
    deck_mail(&f);
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let (promise, question) = deck_cards(&p);
    p.dismiss(&question.id, DismissReason::AlreadyHandled, now())
        .unwrap();
    // The user's own promise in the thread is a commitment, not part of the conversation.
    let pr = p.card(&promise.id).unwrap();
    assert!(pr.learned.iter().all(|e| e.direction != Direction::Hidden));
    assert!(in_inbox(&p, &pr.id));
    // Hana writes after the dismissal: her question surfaces.
    put_mail(
        &f.store,
        Mail {
            key: "m-hana-2",
            from: "Hana Ito <hana@studio.test>",
            subject: "Re: Deck and address",
            body: "It's 4 Elm St. Could you also send the budget?",
            days_ago: -0.5,
            message_id: "<hana-2@studio.test>",
            in_reply_to: Some("<me-deck@nugget.dev>"),
            ..Default::default()
        },
    );
    let later = now() + Duration::days(2);
    p.refresh(later).await.unwrap();
    let new = find(&p, |c| {
        c.kind == CandidateKind::UnansweredThread
            && c.state.is_open()
            && c.evidence.iter().any(|e| e.item.key.as_str() == "m-hana-2")
    });
    assert_eq!(new.features.thread, question.features.thread);
    assert!(new.learned.iter().all(|e| e.direction != Direction::Hidden));
    assert!(in_inbox(&p, &new.id), "{new:?}");
}

#[tokio::test]
async fn kind_reset_restores_the_model_weight() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let renewal = find(&p, |c| c.kind == CandidateKind::Renewal);
    p.dismiss(&renewal.id, DismissReason::NotImportant, now())
        .unwrap();
    let prior = ScoringModel::prior()["kind_renewal"];
    assert!(p.model().unwrap().weights["kind_renewal"] < prior);
    assert!(
        p.learned(now())
            .unwrap()
            .iter()
            .any(|e| e.scope == Scope::Kind && e.key == "renewal")
    );
    let n = p
        .reset_learning(&ResetScope::One(Scope::Kind, "renewal".into()), now())
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(p.model().unwrap().weights["kind_renewal"], prior);
    assert!(
        p.learned(now())
            .unwrap()
            .iter()
            .all(|e| e.scope != Scope::Kind)
    );
}

#[tokio::test]
async fn snoozes_and_repeated_accepts_teach_the_layer_nothing_extra() {
    let f = fixture();
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let ana = find(&p, |c| c.title.contains("Pricing for the pilot"));
    p.snooze(&ana.id, now() + Duration::days(1), now()).unwrap();
    assert!(p.learned(now()).unwrap().is_empty());
    p.accept(&ana.id, now()).unwrap();
    p.accept(&ana.id, now()).unwrap();
    let listed = p.learned(now()).unwrap();
    let sender = listed
        .iter()
        .find(|e| e.scope == Scope::Sender && e.key == "ana@acme.io")
        .unwrap();
    assert!((sender.accepted - 1.0).abs() < 1e-9);
    assert!(sender.adjustment > 0.0);
}

#[tokio::test]
async fn legacy_per_sender_weights_move_into_the_learned_layer() {
    let f = fixture();
    let cards = Arc::new(MemCardStore::default());
    let mut model = ScoringModel::default();
    model.weights.insert("sender:ana@acme.io".into(), -2.0);
    cards
        .save(&ProactiveState {
            version: nugget_proactive::cards::STATE_VERSION,
            model,
            ..Default::default()
        })
        .unwrap();
    let p = Proactive::new(f.store.clone(), cfg()).with_card_store(cards);
    p.refresh(now()).await.unwrap();
    assert!(
        !p.model()
            .unwrap()
            .weights
            .keys()
            .any(|k| k.starts_with("sender:"))
    );
    let ana = find(&p, |c| c.title.contains("Pricing for the pilot"));
    let e = &ana.learned[0];
    assert_eq!((e.scope, e.key.as_str()), (Scope::Sender, "ana@acme.io"));
    assert!((e.contribution + 2.0).abs() < 1e-6, "{e:?}");
}

/// PL-2: a sensitive card's title (its mail subject) is not kept in the learned layer, which
/// outlives the card and is listed by `proactive.learning`.
#[tokio::test]
async fn a_sensitive_cards_subject_is_not_kept_as_a_learned_label() {
    let f = fixture();
    put_mail(
        &f.store,
        Mail {
            key: "m-lab",
            from: "Dana Lee <dana@clinic.test>",
            subject: "Your biopsy results 884213",
            body: "Could you call me about the biopsy results before Friday?",
            days_ago: 1.5,
            message_id: "<lab-1@clinic.test>",
            sensitive: true,
            ..Default::default()
        },
    );
    let p = Proactive::new(f.store.clone(), cfg());
    p.refresh(now()).await.unwrap();
    let card = find(&p, |c| c.title.contains("biopsy"));
    assert!(card.sensitive);
    p.dismiss(&card.id, DismissReason::NotImportant, now())
        .unwrap();
    let listed = p.learned(now()).unwrap();
    let thread = listed
        .iter()
        .find(|e| e.scope == Scope::Thread)
        .expect("the thread is learned");
    assert!(thread.label.is_none(), "{:?}", thread.label);
    assert!(
        listed
            .iter()
            .all(|e| !e.key.contains("884213")
                && !e.label.as_deref().unwrap_or("").contains("884213"))
    );
}
