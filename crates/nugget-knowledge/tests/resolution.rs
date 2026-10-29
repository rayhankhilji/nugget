//! Entity resolution across sources: a person's own note, job changes, organisations known by
//! name and by mail domain.

mod common;

use common::*;
use nugget_graph::{ClaimObject, ClaimStatus, EntityKind, Identifier};
use nugget_knowledge::KnowledgeConfig;
use serde_json::json;

/// A recipient at a webmail domain (no organisation).
const TO: &[&str] = &["Robin <robin@gmail.com>"];

#[tokio::test]
async fn a_persons_own_note_joins_their_addresses_and_the_new_job_wins() {
    let env = Env::new();
    Mail {
        key: "1",
        to: TO,
        from: "Priya Castellane <priya@oldco.test>",
        date: "2026-09-01T10:00:00Z",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "2",
        to: TO,
        from: "Priya Castellane <priya@newco.test>",
        date: "2026-10-05T10:00:00Z",
        body: "I've joined Newco. Please use this address from now on.",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let priyas = |env: &Env| {
        env.entities(EntityKind::Person)
            .into_iter()
            .filter(|e| e.name == "Priya Castellane")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        priyas(&env).len(),
        2,
        "different addresses: not merged on name"
    );

    // Her note lists both addresses: they are one person.
    env.put(
        note(
            "files:notes",
            "people/Priya Castellane.md",
            "Priya Castellane",
            json!({"type": "person",
                   "email": ["priya@newco.test", "priya@oldco.test"]}),
            &[],
            &[],
            "Moved to Newco in October.",
        ),
        &["Moved to Newco in October."],
    );
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    let all = priyas(&env);
    assert_eq!(all.len(), 1);
    let priya = &all[0];
    assert!(
        priya
            .identifiers
            .contains(&Identifier::new("email", "priya@oldco.test"))
    );
    assert!(
        priya
            .identifiers
            .contains(&Identifier::new("email", "priya@newco.test"))
    );

    // Two employers on one person now: the newer one is current, the older one history.
    let oldco = env.entity(EntityKind::Organization, "Oldco");
    let newco = env.entity(EntityKind::Organization, "Newco");
    let active = env.active(priya, "works_at");
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(active[0].object, ClaimObject::Entity(newco.id.clone()));
    let old = env
        .claims(priya, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(oldco.id.clone()))
        .expect("the old employer is kept as history");
    assert_eq!(old.status, ClaimStatus::Superseded);
    assert!(old.valid_to.is_some());
}

#[tokio::test]
async fn an_organisation_by_domain_is_the_one_by_name() {
    // The domain first, the name later.
    let env = Env::new();
    Mail {
        key: "1",
        to: TO,
        from: "Bart Fenwick <bart@cindergate.test>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    assert_eq!(
        env.entity(EntityKind::Organization, "Cindergate").name,
        "Cindergate"
    );
    env.put(
        note(
            "files:notes",
            "orgs/Cindergate Analytics.md",
            "Cindergate Analytics",
            json!({"type": "organization"}),
            &[],
            &[],
            "A customer.",
        ),
        &["A customer."],
    );
    run(&k).await;
    let orgs = env.entities(EntityKind::Organization);
    assert_eq!(orgs.len(), 1, "{orgs:?}");
    assert_eq!(orgs[0].name, "Cindergate Analytics", "the real name wins");
    assert!(
        orgs[0]
            .identifiers
            .contains(&Identifier::new("domain", "cindergate.test"))
    );
    let bart = env.entity(EntityKind::Person, "Bart Fenwick");
    assert_eq!(
        env.active(&bart, "works_at")[0].object,
        ClaimObject::Entity(orgs[0].id.clone())
    );

    // The name first (the note sorts before the mail), the domain later.
    let env = Env::new();
    env.put(
        note(
            "files:notes",
            "orgs/Saltmarsh Family Clinic.md",
            "Saltmarsh Family Clinic",
            json!({"type": "organization"}),
            &[],
            &[],
            "Our GP.",
        ),
        &["Our GP."],
    );
    Mail {
        key: "1",
        to: TO,
        from: "Nadia Kerrigan <n.kerrigan@saltmarsh-clinic.test>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let orgs = env.entities(EntityKind::Organization);
    assert_eq!(orgs.len(), 1, "{orgs:?}");
    assert_eq!(orgs[0].name, "Saltmarsh Family Clinic");
    assert!(
        orgs[0]
            .identifiers
            .contains(&Identifier::new("domain", "saltmarsh-clinic.test"))
    );
}

#[tokio::test]
async fn a_domain_spelling_two_names_links_neither() {
    let env = Env::new();
    for name in ["Acme Bank", "Acme Builders"] {
        env.put(
            note(
                "files:notes",
                &format!("orgs/{name}.md"),
                name,
                json!({"type": "organization"}),
                &[],
                &[],
                "An organisation.",
            ),
            &["An organisation."],
        );
    }
    Mail {
        key: "1",
        to: TO,
        from: "Alice Smith <alice@ab.test>",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "2",
        to: TO,
        from: "Bob Stone <bob@acmeb.test>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    // `acmeb` spells both names: ambiguous, so a third organisation; `ab` spells neither.
    assert_eq!(env.entities(EntityKind::Organization).len(), 4);
}
