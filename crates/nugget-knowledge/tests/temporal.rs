//! Time-aware claims: validity intervals, supersession of single-valued predicates (past-tense
//! and start cues in quotes, deterministic ties), order independence, and the one-time backfill
//! of graphs written before validity intervals; the user's vault edits against re-extraction
//! (GG5-4) and newer sources (GG5-13); facts as of when their sources are from (QA GG5).

mod common;

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_graph::{Claim, ClaimObject, ClaimPatch, ClaimStatus, EntityKind, NewClaim};
use nugget_knowledge::{ItemFeed, Knowledge, KnowledgeConfig, LlmConfig, MeConfig};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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
        // Last seen in September; the mail of 1 October 17:45 says it ended, but the note says
        // Quillfeather from 12:00 that day: valid until the earlier of the two.
        row(
            "works_at",
            "Cindergate Analytics",
            Superseded,
            "2026-09-03T08:05:00Z",
            Some("2026-10-01T12:00:00Z"),
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
    let orders: [&[&str]; 2] = [&["m1", "m2", "note", "m3"], &["m3", "note", "m2", "m1"]];
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
async fn a_forged_future_date_is_clamped() {
    let env = Env::new();
    let k = env.knowledge(plain_cfg(), None);
    mail(
        "far",
        "Alice Smith <alice@mail.test>",
        "2099-01-01T00:00:00Z",
        "From the future",
    )
    .put(&env);
    run(&k).await;
    let me = env.me();
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 1);
    let limit = OffsetDateTime::now_utc() + time::Duration::days(2);
    assert!(
        corr[0].valid_from.unwrap() < limit,
        "{:?}",
        corr[0].valid_from
    );
}

fn user_claim(subject: &nugget_core::ids::EntityId, object: ClaimObject) -> NewClaim {
    NewClaim {
        subject: subject.clone(),
        predicate: "works_at".into(),
        object,
        text: "my words".into(),
        confidence: 1.0,
        status: ClaimStatus::UserAsserted,
        evidence: Vec::new(),
        valid_from: None,
        valid_to: None,
    }
}

#[tokio::test]
async fn the_users_edit_wins_over_re_extraction_until_a_newer_source() {
    let env = Env::new();
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(plain_cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let acme = env.active(&alice, "works_at");
    assert_eq!(acme.len(), 1);
    let acme = acme[0].clone();

    // The user rewrites the line in the vault (no link: a literal value).
    let mine = env
        .g()
        .supersede_by_user(
            acme.id,
            &user_claim(
                &alice.id,
                ClaimObject::Literal("Alice Smith is CTO at Acme (fixed by me)".into()),
            ),
        )
        .unwrap();

    let current = |env: &Env| -> Vec<i64> {
        let mut v: Vec<i64> = env
            .active(&alice, "works_at")
            .iter()
            .map(|c| c.id)
            .collect();
        v.sort_unstable();
        v
    };

    // Everything is extracted again: the edit stands.
    k.reset().await.unwrap();
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);
    let old = env.g().get_claim(acme.id).unwrap().unwrap();
    assert_eq!(old.status, ClaimStatus::Superseded);
    assert!(!old.evidence.is_empty(), "the evidence is kept");

    // The same source is saved again, now dated after the edit, still saying the same: the edit
    // still stands (the source did not change that fact).
    env.put(
        card("old", "Acme", "2099-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);
    assert_eq!(
        env.g().get_claim(acme.id).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );

    // An older source with another value: history, the user's value stays current.
    env.put(
        card("older", "Initech", "2024-01-01T00:00:00Z"),
        &["Alice Smith, Initech"],
    );
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);

    // A strictly newer source (dated after the edit) with another value supersedes the user's.
    env.put(
        card("new", "Globex", "2099-06-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    run(&k).await;
    let globex = env.entity(EntityKind::Organization, "Globex");
    let active = env.active(&alice, "works_at");
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(active[0].object, ClaimObject::Entity(globex.id.clone()));
    assert_eq!(
        env.g().get_claim(mine).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );
}

/// Second review, finding 2: the user edits A into B (a link to another organisation), then B
/// into C. An older source then says B: the store revives B (it was replaced, and the
/// evidence is new to it); the session must expect that and keep it as history, so C stays the
/// only current value.
#[tokio::test]
async fn an_older_source_for_an_edited_away_user_value_stays_history() {
    let env = Env::new();
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    // Another contact makes the organisation Globex known.
    env.put(
        nugget_store::NewItem::new("contacts:phone", "bob", nugget_store::content_hash("bob"))
            .with_mime("application/json")
            .with_version("bob")
            .with_modified_at(
                time::OffsetDateTime::parse(
                    "2025-01-01T00:00:00Z",
                    &time::format_description::well_known::Rfc3339,
                )
                .unwrap(),
            )
            .with_metadata(json!({
                "source": {"contact": {"name": "Bob Jones", "emails": ["bob@example.net"],
                                        "organization": "Globex"}},
                "sensitive": false
            })),
        &["Bob Jones, Globex"],
    );
    let k = env.knowledge(plain_cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let globex = env.entity(EntityKind::Organization, "Globex");
    let a = env.active(&alice, "works_at")[0].clone();
    let b = env
        .g()
        .supersede_by_user(
            a.id,
            &user_claim(&alice.id, ClaimObject::Entity(globex.id.clone())),
        )
        .unwrap();
    let c = env
        .g()
        .supersede_by_user(
            b,
            &user_claim(
                &alice.id,
                ClaimObject::Literal("Alice Smith is a consultant".into()),
            ),
        )
        .unwrap();
    assert!(env.g().has_user_replacement(b).unwrap());
    env.put(
        card("older", "Globex", "2024-01-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    run(&k).await;
    let active: Vec<i64> = env
        .active(&alice, "works_at")
        .iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(active, vec![c], "{:?}", env.claims(&alice, "works_at"));
}

fn mara_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Mara Okafor".into()),
            emails: vec!["mara.okafor@brightwell.example".into()],
            infer_from_sent: false,
        },
        ..Default::default()
    }
}

/// Dana's person note (both her addresses), as of `date`, saying she works at `org`.
fn dana_note(env: &Env, date: &str, org: &str) {
    let body = format!("# Dana Whitfield\n\nWorks at {org}.");
    env.put(
        note(
            "files:notes",
            "Notes/People/Dana-Whitfield.md",
            "Dana Whitfield",
            json!({"type": "person", "date": date,
                   "email": ["dana@corvidlabs.example", "dana.whitfield@brightwell.example"],
                   "org": org}),
            &[],
            &[],
            &body,
        ),
        &[&body],
    );
}

/// Dana's mails: at Brightwell in March, at Corvid Labs from August (and her person note).
fn dana_mails(env: &Env) {
    dana_note(env, "2026-08-17", "Corvid Labs");
    Mail {
        source: "imap:mara",
        key: "INBOX/1",
        from: "Dana Whitfield <dana.whitfield@brightwell.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        date: "2026-03-04T09:00:00Z",
        subject: "Dashboards",
        body: "The Harbor dashboards are ready for review.",
        ..Default::default()
    }
    .put(env);
    Mail {
        source: "imap:mara",
        key: "INBOX/2",
        from: "Dana Whitfield <dana@corvidlabs.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        date: "2026-08-14T09:00:00Z",
        subject: "New job",
        body: "I've joined Corvid Labs as Head of Product.",
        ..Default::default()
    }
    .put(env);
}

/// GG5-6: an old person note copied in today (mtime = now) is dated by its frontmatter
/// `date:` (March), so it does not make Brightwell Dana's current employer again over her
/// August mail.
#[tokio::test]
async fn a_copied_old_note_keeps_its_own_date() {
    let env = Env::new();
    dana_mails(&env);
    let now = OffsetDateTime::now_utc();
    env.put(
        note(
            "files:notes",
            "Notes/People/Dana (Brightwell).md",
            "Dana Whitfield",
            json!({"type": "person", "date": "2026-03-01",
                   "email": "dana.whitfield@brightwell.example",
                   "org": "Brightwell Analytics"}),
            &[],
            &[],
            "# Dana Whitfield\n\nDesign Lead at Brightwell Analytics.",
        )
        .with_modified_at(now),
        &["# Dana Whitfield\n\nDesign Lead at Brightwell Analytics."],
    );
    let k = env.knowledge(mara_cfg(), None);
    run(&k).await;

    let dana = env.entity(EntityKind::Person, "Dana Whitfield");
    let current = env.active(&dana, "works_at");
    assert_eq!(current.len(), 1, "{current:#?}");
    assert!(object_name(&env, &current[0]).starts_with("Corvid"));
    // Every Brightwell claim is history, dated by the note's own date, not the copy time.
    let old: Vec<_> = env
        .claims(&dana, "works_at")
        .into_iter()
        .filter(|c| object_name(&env, c).starts_with("Brightwell"))
        .collect();
    assert!(!old.is_empty());
    for c in &old {
        assert_eq!(c.status, ClaimStatus::Superseded, "{c:#?}");
        assert!(c.valid_from.is_some_and(|f| f < at("2026-04-01T00:00:00Z")));
    }
}

/// GG5-16: a person only copied on a mail did not correspond with the user.
#[tokio::test]
async fn cc_alone_is_not_correspondence() {
    let env = Env::new();
    Mail {
        source: "imap:mara",
        key: "INBOX/9",
        from: "Robert Chen <robert.chen@brightwell.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        cc: &["Dana Whitfield <dana@corvidlabs.example>"],
        date: "2026-09-01T09:00:00Z",
        subject: "Harbor",
        body: "Launch plan attached.",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(mara_cfg(), None);
    run(&k).await;
    let me = env.me();
    let bob = env.entity(EntityKind::Person, "Robert Chen");
    let dana = env.entity(EntityKind::Person, "Dana Whitfield");
    let corr = env.active(&me, "corresponded_with");
    assert!(
        corr.iter()
            .any(|c| c.object == ClaimObject::Entity(bob.id.clone()))
    );
    assert!(
        corr.iter()
            .all(|c| c.object != ClaimObject::Entity(dana.id.clone())),
        "{corr:#?}"
    );
}
