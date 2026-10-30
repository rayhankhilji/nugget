//! Time-aware claims: validity intervals, supersession of single-valued predicates (past-tense
//! and start cues in quotes, deterministic ties), order independence, and the one-time backfill
//! of graphs written before validity intervals.

mod common;

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_graph::{Claim, ClaimObject, ClaimPatch, ClaimStatus, EntityKind, NewClaim};
use nugget_knowledge::{ItemFeed, Knowledge, KnowledgeConfig, LlmConfig, MeConfig};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            emails: vec!["robin@glimmerfen.test".into()],
            ..Default::default()
        },
        extra_freemail_domains: vec!["mail.test".into()],
        llm: LlmConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

const M1: &str = "Hello Robin,\n\nPriya Castellane, our Senior Data Scientist at Cindergate Analytics, \
will join the kickoff.\n\nMargaret";
const M2: &str = "Hi Robin,\n\nFriday is my last day at Cindergate Analytics.\n\nPriya Castellane";
const M3: &str =
    "Hi Robin,\n\nI've joined Quillfeather Systems as Head of Data.\n\nPriya Castellane";
const NOTE: &str = "# Priya Castellane\n\nMoved from [[Cindergate Analytics]] to [[Quillfeather Systems]] in October \
2026. Now Head of Data at Quillfeather.";

fn ent(name: &str, kind: &str) -> Value {
    json!({"name": name, "kind": kind})
}

fn claim(s: &str, p: &str, o: &str, quote: &str) -> Value {
    json!({"subject": s, "predicate": p, "object": o, "quote": quote})
}

/// The scripted model: what a local model says about each item (roles under the synonyms a
/// model uses: `role`, `position`, `job_title`).
fn reply(prompt: &str) -> String {
    const P: &str = "Priya Castellane";
    let (c, q) = ("Cindergate Analytics", "Quillfeather Systems");
    let (entities, claims) = if prompt.contains("our Senior Data Scientist") {
        let quote = "Priya Castellane, our Senior Data Scientist at Cindergate Analytics";
        (
            vec![ent(P, "person"), ent(c, "organization")],
            vec![
                claim(P, "works_at", c, quote),
                claim(P, "role", "Senior Data Scientist", quote),
            ],
        )
    } else if prompt.contains("my last day at") {
        (
            vec![ent(P, "person"), ent(c, "organization")],
            vec![claim(
                P,
                "works_at",
                c,
                "Friday is my last day at Cindergate Analytics",
            )],
        )
    } else if prompt.contains("joined Quillfeather") {
        let quote = "I've joined Quillfeather Systems as Head of Data";
        (
            vec![ent(P, "person"), ent(q, "organization")],
            vec![
                claim(P, "works_at", q, quote),
                claim(P, "position", "Head of Data", quote),
            ],
        )
    } else if prompt.contains("Moved from") {
        let moved = "Moved from [[Cindergate Analytics]] to [[Quillfeather Systems]]";
        (
            vec![
                ent(P, "person"),
                ent(c, "organization"),
                ent(q, "organization"),
            ],
            vec![
                claim(P, "works_at", c, moved),
                claim(P, "works_at", q, moved),
                claim(
                    P,
                    "job_title",
                    "Head of Data",
                    "Now Head of Data at Quillfeather",
                ),
            ],
        )
    } else {
        (Vec::new(), Vec::new())
    };
    json!({"entities": entities, "claims": claims}).to_string()
}

fn knowledge(env: &Env, script: fn(&str) -> String) -> Knowledge {
    let p = FakeProvider::new("local", Locality::OnDevice, script);
    env.knowledge(cfg(), Some(router(Tier::Synthesis, vec![p])))
}

fn mail<'a>(key: &'a str, from: &'a str, date: &'a str, body: &'a str) -> Mail<'a> {
    Mail {
        key,
        from,
        to: &["Robin Ashgrove <robin@glimmerfen.test>"],
        date,
        subject: "News",
        body,
        ..Default::default()
    }
}

/// The job-change items, by name.
fn put(env: &Env, which: &str) {
    match which {
        "m1" => mail(
            "m1",
            "Margaret <margaret@mail.test>",
            "2026-09-03T08:05:00Z",
            M1,
        )
        .put(env),
        "m2" => mail(
            "m2",
            "Priya Castellane <priya@mail.test>",
            "2026-10-01T17:45:00Z",
            M2,
        )
        .put(env),
        "m3" => mail(
            "m3",
            "Priya Castellane <priya@mail.test>",
            "2026-10-05T09:30:00Z",
            M3,
        )
        .put(env),
        "note" => env.put(
            note(
                "files:notes",
                "people/Priya Castellane.md",
                "Priya Castellane",
                Value::Null,
                &[],
                &[],
                NOTE,
            )
            .with_modified_at(at("2026-10-01T12:00:00Z")),
            &[NOTE],
        ),
        _ => unreachable!(),
    }
}

type Row = (String, String, ClaimStatus, Option<String>, Option<String>);

fn object_name(env: &Env, c: &Claim) -> String {
    match &c.object {
        ClaimObject::Entity(e) => env.g().get_entity(e).unwrap().unwrap().name,
        ClaimObject::Literal(l) => l.clone(),
    }
}

fn fmt(t: Option<OffsetDateTime>) -> Option<String> {
    t.map(|t| t.format(&Rfc3339).unwrap())
}

/// Priya's employer and title claims: predicate, object, status, interval.
fn snapshot(env: &Env) -> Vec<Row> {
    let priya = env.entity(EntityKind::Person, "Priya Castellane");
    let mut rows: Vec<Row> = Vec::new();
    for pred in ["works_at", "has_title"] {
        for c in env.claims(&priya, pred) {
            rows.push((
                pred.into(),
                object_name(env, &c),
                c.status,
                fmt(c.valid_from),
                fmt(c.valid_to),
            ));
        }
    }
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}

fn row(p: &str, o: &str, s: ClaimStatus, from: &str, to: Option<&str>) -> Row {
    (
        p.into(),
        o.into(),
        s,
        Some(from.into()),
        to.map(str::to_owned),
    )
}

fn expected() -> Vec<Row> {
    use ClaimStatus::*;
    let mut rows = vec![
        // Joined in October (the note is the earliest evidence), current.
        row(
            "works_at",
            "Quillfeather Systems",
            Active,
            "2026-10-01T12:00:00Z",
            None,
        ),
        // Last seen in September; the mail of 1 October says it ended.
        row(
            "works_at",
            "Cindergate Analytics",
            Superseded,
            "2026-09-03T08:05:00Z",
            Some("2026-10-01T17:45:00Z"),
        ),
        row(
            "has_title",
            "Head of Data",
            Active,
            "2026-10-01T12:00:00Z",
            None,
        ),
        // Valid until the next title is first seen.
        row(
            "has_title",
            "Senior Data Scientist",
            Superseded,
            "2026-09-03T08:05:00Z",
            Some("2026-10-01T12:00:00Z"),
        ),
    ];
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}

#[tokio::test]
async fn a_job_change_ends_up_the_same_whatever_the_processing_order() {
    let orders: [&[&str]; 4] = [
        &["m1", "m2", "note", "m3"],
        &["m3", "note", "m2", "m1"],
        &["note", "m3", "m1", "m2"],
        &["m2", "m1", "m3", "note"],
    ];
    let mut first: Option<Vec<Row>> = None;
    for order in orders {
        let env = Env::new();
        let k = knowledge(&env, reply);
        for which in order {
            put(&env, which);
            run(&k).await;
        }
        let snap = snapshot(&env);
        assert_eq!(snap, expected(), "order {order:?}");
        match &first {
            Some(f) => assert_eq!(&snap, f, "order {order:?}"),
            None => first = Some(snap),
        }
    }
    // All at once (one run, the plan's order).
    let env = Env::new();
    let k = knowledge(&env, reply);
    for which in ["m1", "m2", "m3", "note"] {
        put(&env, which);
    }
    run(&k).await;
    assert_eq!(snapshot(&env), expected());
}

#[tokio::test]
async fn a_quote_saying_the_fact_ended_never_makes_it_current() {
    let env = Env::new();
    let k = knowledge(&env, reply);
    put(&env, "m2");
    run(&k).await;
    let priya = env.entity(EntityKind::Person, "Priya Castellane");
    assert!(env.active(&priya, "works_at").is_empty());
    let all = env.claims(&priya, "works_at");
    assert_eq!(all.len(), 1);
    let c = &all[0];
    assert_eq!(c.status, ClaimStatus::Superseded);
    assert_eq!(c.valid_to, Some(at("2026-10-01T17:45:00Z")));
    assert_eq!(c.valid_from, Some(at("2026-10-01T17:45:00Z")));
    // The evidence stays attached and cited.
    assert_eq!(c.evidence.len(), 1);
    assert_eq!(
        c.evidence[0].quote.as_deref(),
        Some("Friday is my last day at Cindergate Analytics")
    );

    // Earlier evidence of the job does not bring it back either.
    put(&env, "m1");
    run(&k).await;
    assert!(env.active(&priya, "works_at").is_empty());
    let c = &env.claims(&priya, "works_at")[0];
    assert_eq!(c.evidence.len(), 2);
    assert_eq!(c.valid_from, Some(at("2026-09-03T08:05:00Z")));
    assert_eq!(c.valid_to, Some(at("2026-10-01T17:45:00Z")));

    // Deleting the mail that ended it: the job is current again.
    env.store
        .delete_item(&item_ref("imap:me@corp.example", "m2"))
        .unwrap();
    run(&k).await;
    let active = env.active(&priya, "works_at");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].valid_to, None);
}

const TIE: &str = "Alice Smith consulted for Acme Corp in spring. Alice Smith works at Globex Inc.";

fn tie_forward(_: &str) -> String {
    json!({
        "entities": [ent("Alice Smith", "person"), ent("Acme Corp", "organization"),
                     ent("Globex Inc", "organization")],
        "claims": [
            claim("Alice Smith", "works_at", "Acme Corp", "Alice Smith consulted for Acme Corp"),
            claim("Alice Smith", "works_at", "Globex Inc", "Alice Smith works at Globex Inc"),
        ]
    })
    .to_string()
}

fn tie_reverse(_: &str) -> String {
    json!({
        "entities": [ent("Globex Inc", "organization"), ent("Alice Smith", "person"),
                     ent("Acme Corp", "organization")],
        "claims": [
            claim("Alice Smith", "works_at", "Globex Inc", "Alice Smith works at Globex Inc"),
            claim("Alice Smith", "works_at", "Acme Corp", "Alice Smith consulted for Acme Corp"),
        ]
    })
    .to_string()
}

#[tokio::test]
async fn two_values_from_one_item_tie_by_their_place_in_the_text() {
    for script in [tie_forward as fn(&str) -> String, tie_reverse] {
        let env = Env::new();
        let k = knowledge(&env, script);
        env.put(
            note(
                "files:notes",
                "alice.md",
                "Alice",
                Value::Null,
                &[],
                &[],
                TIE,
            )
            .with_modified_at(at("2026-05-01T00:00:00Z")),
            &[TIE],
        );
        run(&k).await;
        let alice = env.entity(EntityKind::Person, "Alice Smith");
        let active = env.active(&alice, "works_at");
        assert_eq!(active.len(), 1);
        assert_eq!(object_name(&env, &active[0]), "Globex Inc");
        let old: Vec<Claim> = env
            .claims(&alice, "works_at")
            .into_iter()
            .filter(|c| c.status == ClaimStatus::Superseded)
            .collect();
        assert_eq!(old.len(), 1);
        assert_eq!(object_name(&env, &old[0]), "Acme Corp");
        assert_eq!(old[0].valid_to, Some(at("2026-05-01T00:00:00Z")));
    }
}

#[tokio::test]
async fn every_claim_starts_at_its_earliest_evidence() {
    let env = Env::new();
    let k = knowledge(&env, |_| "{}".into());
    // The newer mail is processed first (keys sort before).
    mail(
        "a",
        "Alice Smith <alice@mail.test>",
        "2026-06-01T10:00:00Z",
        "Lunch?",
    )
    .put(&env);
    run(&k).await;
    mail(
        "b",
        "Alice Smith <alice@mail.test>",
        "2026-01-01T10:00:00Z",
        "Coffee?",
    )
    .put(&env);
    run(&k).await;
    let me = env.me();
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 1);
    assert_eq!(corr[0].evidence.len(), 2);
    assert_eq!(corr[0].valid_from, Some(at("2026-01-01T10:00:00Z")));
    for c in env.all_claims() {
        assert!(c.valid_from.is_some(), "{c:?}");
    }
}

fn card(key: &str, org: &str, modified: &str) -> nugget_store::NewItem {
    nugget_store::NewItem::new(
        "contacts:phone",
        key,
        nugget_store::content_hash(format!("{key}{org}{modified}")),
    )
    .with_mime("application/json")
    .with_version(format!("{key}{org}{modified}"))
    .with_modified_at(at(modified))
    .with_metadata(json!({
        "source": {"contact": {"name": "Alice Smith", "emails": ["alice@example.org"],
                                "organization": org}},
        "sensitive": false
    }))
}

fn plain_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            emails: vec!["robin@glimmerfen.test".into()],
            ..Default::default()
        },
        extra_freemail_domains: vec!["mail.test".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn the_users_assertion_wins_over_newer_evidence() {
    let env = Env::new();
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(plain_cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let acme = env.entity(EntityKind::Organization, "Acme");
    let mine = env
        .g()
        .add_claim(&NewClaim {
            subject: alice.id.clone(),
            predicate: "works_at".into(),
            object: ClaimObject::Entity(acme.id.clone()),
            text: "Alice Smith works at Acme.".into(),
            confidence: 1.0,
            status: ClaimStatus::UserAsserted,
            evidence: Vec::new(),
            valid_from: None,
            valid_to: None,
        })
        .unwrap();
    env.put(
        card("new", "Globex", "2026-02-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    run(&k).await;
    let active = env.active(&alice, "works_at");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, mine);
    assert_eq!(active[0].status, ClaimStatus::UserAsserted);
    let globex = env.entity(EntityKind::Organization, "Globex");
    let g = env
        .claims(&alice, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(globex.id.clone()))
        .unwrap();
    assert_eq!(g.status, ClaimStatus::Superseded);
}

/// A graph as written before validity intervals: no `valid_from` anywhere, two active values of
/// a single-valued predicate, a superseded value without `valid_to`, and pipeline state without
/// the temporal key.
#[tokio::test]
async fn the_backfill_migrates_an_old_graph_once() {
    let env = Env::new();
    env.put(
        card("older", "Initech", "2024-01-01T00:00:00Z"),
        &["Alice Smith, Initech"],
    );
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    env.put(
        card("new", "Globex", "2026-02-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    mail(
        "m",
        "Bob Jones <bob@mail.test>",
        "2026-03-01T10:00:00Z",
        "Hello",
    )
    .put(&env);
    let k = env.knowledge(plain_cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let name_of = |c: &Claim| object_name(&env, c);

    // Make it old.
    for c in env.g().claims_for(&alice.id, true).unwrap() {
        env.g()
            .update_claim(
                c.id,
                &ClaimPatch {
                    valid_from: Some(None),
                    valid_to: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        if c.predicate == "works_at" && name_of(&c) != "Initech" {
            env.g().set_claim_status(c.id, ClaimStatus::Active).unwrap();
        }
    }
    assert_eq!(env.active(&alice, "works_at").len(), 2);
    let mut state = env.store.get_state("knowledge").unwrap().unwrap();
    state.as_object_mut().unwrap().remove("temporal");
    ItemFeed::set_state(&env.store, "knowledge", state).unwrap();

    // The next run has nothing to extract, but migrates the graph.
    let k = env.knowledge(plain_cfg(), None);
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 0);
    assert!(stats.changed_entities > 0);
    let by_org = |org: &str| {
        env.claims(&alice, "works_at")
            .into_iter()
            .find(|c| name_of(c) == org)
            .unwrap()
    };
    let globex = by_org("Globex");
    assert_eq!(globex.status, ClaimStatus::Active);
    assert_eq!(globex.valid_from, Some(at("2026-02-01T00:00:00Z")));
    assert_eq!(globex.valid_to, None);
    let acme = by_org("Acme");
    assert_eq!(acme.status, ClaimStatus::Superseded);
    assert_eq!(acme.valid_from, Some(at("2025-01-01T00:00:00Z")));
    assert_eq!(acme.valid_to, Some(at("2026-02-01T00:00:00Z")));
    let initech = by_org("Initech");
    assert_eq!(initech.status, ClaimStatus::Superseded);
    assert_eq!(initech.valid_to, Some(at("2025-01-01T00:00:00Z")));
    // Every claim with evidence has a start now.
    for c in env.all_claims() {
        assert!(c.valid_from.is_some(), "{c:?}");
    }
    let me = env.me();
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr[0].valid_from, Some(at("2026-03-01T10:00:00Z")));

    // Once only: the next run changes nothing.
    let before: Vec<Claim> = env.g().claims_for(&alice.id, true).unwrap();
    let again = run(&k).await;
    assert_eq!(again.changed_entities, 0);
    assert_eq!(env.g().claims_for(&alice.id, true).unwrap(), before);
    let fresh = env.knowledge(plain_cfg(), None);
    assert_eq!(run(&fresh).await.changed_entities, 0);
}

#[tokio::test]
async fn a_new_vault_records_the_temporal_version() {
    let env = Env::new();
    let k = env.knowledge(plain_cfg(), None);
    run(&k).await;
    let state = env.store.get_state("knowledge").unwrap().unwrap();
    assert_eq!(state["temporal"], json!(1));
}
