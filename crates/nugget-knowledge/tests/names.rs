//! People's short names across items: nicknames, first names and initials join a full-name
//! person only with corroboration, whatever order the items arrive in (one per run or several
//! per run), and every join leaves a merge record. Resolution across sources (a person's own
//! note, organisations by name and domain), and a user's split as a constraint.

mod common;

use std::collections::BTreeMap;

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::EntityId;
use nugget_graph::{
    Claim, ClaimObject, EntityKind, Identifier, MERGE_LOG_ATTR, NewEntity, merge_log,
};
use nugget_knowledge::{Knowledge, KnowledgeConfig, LlmConfig, MeConfig};
use nugget_store::NewItem;
use serde_json::{Value, json};

const ME: &[&str] = &["Robin Ashgrove <robin@ashgrove.test>"];

/// One item of the scenario: how to store it, and what the scripted model says about it.
struct Case {
    item: NewItem,
    body: &'static str,
    /// People the model names (as written).
    llm_people: &'static [&'static str],
}

fn mail_to(
    key: &'static str,
    from: &'static str,
    to: &'static [&'static str],
    cc: &'static [&'static str],
    body: &'static str,
    llm_people: &'static [&'static str],
) -> Case {
    let m = Mail {
        source: "imap:robin",
        key,
        from,
        to,
        cc,
        date: "2026-10-01T10:00:00Z",
        subject: "Hello",
        body,
        ..Default::default()
    };
    Case {
        item: m.item(),
        body,
        llm_people,
    }
}

fn mail(
    key: &'static str,
    from: &'static str,
    cc: &'static [&'static str],
    body: &'static str,
    llm_people: &'static [&'static str],
) -> Case {
    mail_to(key, from, ME, cc, body, llm_people)
}

fn journal(key: &'static str, body: &'static str, llm_people: &'static [&'static str]) -> Case {
    Case {
        item: note("files:notes", key, key, Value::Null, &[], &[], body),
        body,
        llm_people,
    }
}

const H: usize = 7;

fn scenario() -> Vec<Case> {
    vec![
        mail(
            "a",
            "Margaret Thistlewood <margaret.thistlewood@cindergate.test>",
            &[],
            "Thanks for the update on the survey. Margaret",
            &[],
        ),
        // A colleague of hers (a header at her organisation) calls her by her nickname.
        mail(
            "b",
            "Bart Fenwick <bart.fenwick@cindergate.test>",
            &[],
            "Maggie tells me Lodestar is on track.",
            &["Maggie"],
        ),
        // A first name next to a club-mate in a note: no header, nothing corroborates it.
        journal(
            "rowing.md",
            "Dawn outing with Sam and Rupert Gaskell on the river.",
            &["Sam", "Rupert Gaskell"],
        ),
        mail(
            "d",
            "Sam Whitlock <sam@marrowbank-rc.test>",
            &[],
            "See you at the boathouse on Saturday.",
            &[],
        ),
        mail(
            "e",
            "Rupert Gaskell <rupert.gaskell@marrowbank-rc.test>",
            &[],
            "The boathouse is booked for the fundraiser.",
            &[],
        ),
        // A competing Sam at another organisation.
        mail(
            "f",
            "Sam Okafor <sam.okafor@ostrander-freight.test>",
            &[],
            "The Harbourline staging feed is up.",
            &[],
        ),
        // Nothing corroborates either name: both stay on their own.
        journal(
            "coffee.md",
            "Coffee with Sam and Theo at noon.",
            &["Sam", "Theo"],
        ),
        // An initialled form on an item she is on (same item).
        mail(
            "h",
            "Theo Brannigan <theo.brannigan@glimmerfen.test>",
            &["Margaret Thistlewood <margaret.thistlewood@cindergate.test>"],
            "M. Thistlewood will present the survey results.",
            &["M. Thistlewood"],
        ),
        // A club-mate's mail (header at the club) names "Sam": the one Sam there.
        mail(
            "r1",
            "Rupert Gaskell <rupert.gaskell@marrowbank-rc.test>",
            &[],
            "Sam has the boathouse keys this week.",
            &["Sam"],
        ),
        // "Rob" signs his own mail; "Bob" is greeted in a mail to him (the user is Robin).
        mail(
            "rob1",
            "Robert Ferrand <robert.ferrand@glimmerfen.test>",
            &[],
            "I'll run the on-call rota from November. Rob",
            &["Rob"],
        ),
        mail_to(
            "bob",
            "Robin Ashgrove <robin@ashgrove.test>",
            &["Robert Ferrand <robert.ferrand@glimmerfen.test>"],
            &[],
            "Thanks, Bob, and welcome to the team.",
            &["Bob"],
        ),
        // A job change: the same full name from two addresses, and her new address's mail
        // names the old organisation as hers.
        mail(
            "d53",
            "Delphine Corrigan <delphine.corrigan@glimmerfen.test>",
            &[],
            "I've joined Glimmerfen Labs; my Vellum & Thistle address no longer works.",
            &[],
        ),
        mail(
            "d54",
            "Lucia Penhallow <lucia.penhallow@vellumthistle.test>",
            &[],
            "Friday is Delphine Corrigan's last day at Vellum & Thistle LLP.",
            &["Delphine Corrigan"],
        ),
        mail(
            "d55",
            "Delphine Corrigan <delphine.corrigan@vellumthistle.test>",
            &[],
            "Here is the engagement letter for the review.",
            &[],
        ),
    ]
}

fn reply(cases: &[(&'static str, &'static [&'static str])], prompt: &str) -> String {
    let people = cases
        .iter()
        .find(|(body, _)| prompt.contains(body))
        .map(|(_, p)| *p)
        .unwrap_or(&[]);
    let entities: Vec<Value> = people
        .iter()
        .map(|n| json!({"name": n, "kind": "person"}))
        .collect();
    json!({"entities": entities, "claims": []}).to_string()
}

type Groups = Vec<Vec<(String, String)>>;

/// Process the scenario's items in `runs` (each a batch of items stored, then one run) and
/// return the partition of person mentions: for each person, the (item, surface) pairs that
/// resolved to them.
async fn partition(runs: &[Vec<usize>]) -> (Groups, Env, Knowledge) {
    let env = Env::new();
    let cases = scenario();
    let script: Vec<(&'static str, &'static [&'static str])> =
        cases.iter().map(|c| (c.body, c.llm_people)).collect();
    let p = FakeProvider::new("local", Locality::OnDevice, move |prompt| {
        reply(&script, prompt)
    });
    let cfg = KnowledgeConfig {
        llm: LlmConfig {
            enabled: true,
            ..Default::default()
        },
        me: MeConfig {
            name: Some("Robin Ashgrove".into()),
            emails: vec!["robin@ashgrove.test".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let k = env.knowledge(cfg, Some(router(Tier::Synthesis, vec![p])));
    for batch in runs {
        for &i in batch {
            let c = &cases[i];
            env.put(c.item.clone(), &[c.body]);
        }
        let stats = run(&k).await;
        assert_eq!(stats.items_failed, 0, "{batch:?}");
    }
    let mut groups: Groups = Vec::new();
    for e in env.entities(EntityKind::Person) {
        if e.attributes.get("is_me") == Some(&Value::Bool(true)) {
            continue;
        }
        let mut g: Vec<(String, String)> = env
            .g()
            .mentions_for(&e.id, 0)
            .unwrap()
            .into_iter()
            .map(|m| (m.item.key.to_string(), m.surface))
            .collect();
        g.sort();
        g.dedup();
        if !g.is_empty() {
            groups.push(g);
        }
    }
    groups.sort();
    (groups, env, k)
}

fn group_of(groups: &[Vec<(String, String)>], key: &str, surface: &str) -> Vec<String> {
    let g = groups
        .iter()
        .find(|g| g.iter().any(|(k, s)| k == key && s == surface))
        .unwrap_or_else(|| panic!("no mention {key}/{surface} in {groups:#?}"));
    g.iter().map(|(k, s)| format!("{k}/{s}")).collect()
}

fn singles(order: impl IntoIterator<Item = usize>) -> Vec<Vec<usize>> {
    order.into_iter().map(|i| vec![i]).collect()
}

#[tokio::test]
async fn the_people_do_not_depend_on_item_order() {
    let n = scenario().len();
    let forward = singles(0..n);
    let (want, env, knowledge) = partition(&forward).await;
    // The expected people.
    assert_eq!(
        group_of(&want, "b", "Maggie"),
        [
            "a/Margaret Thistlewood",
            "b/Maggie",
            "h/M. Thistlewood",
            "h/Margaret Thistlewood"
        ]
    );
    assert_eq!(group_of(&want, "r1", "Sam"), ["d/Sam Whitlock", "r1/Sam"]);
    assert_eq!(group_of(&want, "f", "Sam Okafor"), ["f/Sam Okafor"]);
    assert_eq!(
        group_of(&want, "rob1", "Rob"),
        [
            "bob/Bob",
            "bob/Robert Ferrand",
            "rob1/Rob",
            "rob1/Robert Ferrand"
        ]
    );
    assert_eq!(
        group_of(&want, "d53", "Delphine Corrigan"),
        [
            "d53/Delphine Corrigan",
            "d54/Delphine Corrigan",
            "d55/Delphine Corrigan"
        ]
    );
    // Never merged on a first name alone: a note's "Sam" next to a club-mate, a coffee's Sam
    // and Theo.
    assert_eq!(group_of(&want, "rowing.md", "Sam"), ["rowing.md/Sam"]);
    assert_eq!(group_of(&want, "coffee.md", "Sam"), ["coffee.md/Sam"]);
    assert_eq!(group_of(&want, "coffee.md", "Theo"), ["coffee.md/Theo"]);

    // Why: every join is on record.
    let maggie = env.entity(EntityKind::Person, "Margaret Thistlewood");
    let log = merge_log(&maggie);
    assert!(maggie.attributes.get(MERGE_LOG_ATTR).is_some());
    let by_name: BTreeMap<&str, &Vec<String>> =
        log.iter().map(|r| (r.name.as_str(), &r.reasons)).collect();
    assert_eq!(by_name["Maggie"], &["nickname", "same_org"], "{log:#?}");
    assert!(
        by_name["M. Thistlewood"].contains(&"same_item".to_string()),
        "{log:#?}"
    );
    assert!(
        log.iter()
            .all(|r| r.item.is_some() && r.at.is_some() && !r.detail.is_empty())
    );
    let sam = env.entity(EntityKind::Person, "Sam Whitlock");
    let log = merge_log(&sam);
    assert!(
        log.iter().any(|r| r.name == "Sam"
            && r.reasons.contains(&"same_org".to_string())
            && r.item.as_ref().is_some_and(|i| i.key.as_str() == "r1")),
        "{log:#?}"
    );
    let delphine = env.entity(EntityKind::Person, "Delphine Corrigan");
    assert!(
        merge_log(&delphine)
            .iter()
            .any(|r| r.reasons == ["same_name", "job_change"]),
        "{:#?}",
        merge_log(&delphine)
    );
    // Details name nobody and quote nothing from the source.
    for e in [&maggie, &sam, &delphine] {
        for r in merge_log(e) {
            assert!(r.detail.len() <= 80, "{}", r.detail);
            assert!(!r.detail.contains('@') && !r.detail.contains("Thistlewood"));
        }
    }

    // Other orders, one item per run and several per run.
    let reverse = singles((0..n).rev());
    let all_at_once = vec![(0..n).collect::<Vec<_>>()];
    // The competing Sam and the Rob/Bob/Delphine mails first, the full names last.
    let competitor_first = vec![
        vec![5, 8, 2, 6, 9, 10],
        vec![11, 12, 1, 7],
        vec![0, 3, 4, 13],
    ];
    for runs in [reverse, all_at_once, competitor_first] {
        let (got, _, _) = partition(&runs).await;
        assert_eq!(got, want, "runs {runs:?}");
    }

    // No record outlives its source: deleting the item that corroborated "Maggie" drops the
    // record citing it (the merge stays); records citing other items stay.
    let cites = |key: &str| {
        let e = env.entity(EntityKind::Person, "Margaret Thistlewood");
        merge_log(&e)
            .iter()
            .any(|r| r.item.as_ref().is_some_and(|i| i.key.as_str() == key))
    };
    assert!(cites("b") && cites("h"));
    env.store.delete_item(&item_ref("imap:robin", "b")).unwrap();
    let stats = run(&knowledge).await;
    assert_eq!(stats.items_deleted, 1);
    assert!(!cites("b"), "the deleted item's record is gone");
    assert!(cites("h"));
    // Changed and extracted again: the record is dropped and, still corroborated, written
    // again (once).
    let cases = scenario();
    let h = &cases[H];
    let changed = h.item.clone().with_version("v2").with_title("Hello again");
    env.put(changed, &[h.body]);
    run(&knowledge).await;
    let e = env.entity(EntityKind::Person, "Margaret Thistlewood");
    let from_h: Vec<_> = merge_log(&e)
        .into_iter()
        .filter(|r| r.item.as_ref().is_some_and(|i| i.key.as_str() == "h"))
        .collect();
    assert_eq!(from_h.len(), 1, "{from_h:#?}");
}

/// GG5-9: a person is named by their fullest, most used name, whatever form came first.
#[tokio::test]
async fn a_person_is_named_by_their_fullest_name_in_any_order() {
    async fn names(order: &[usize]) -> Vec<String> {
        let env = Env::new();
        let cfg = KnowledgeConfig {
            me: MeConfig {
                name: Some("Mara Okafor".into()),
                emails: vec!["mara@lumenfold.example".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let k = env.knowledge(cfg, None);
        let mails = [
            ("m1", "R. Chen <robert.chen@lumenfold.example>"),
            ("m2", "Robert Chen <robert.chen@lumenfold.example>"),
            ("m3", "Sam <sam.okonkwo@lumenfold.example>"),
            ("m4", "Robert Chen <robert.chen@lumenfold.example>"),
            ("m5", "Sam Okonkwo <sam.okonkwo@lumenfold.example>"),
            ("m6", "Bob Chen <robert.chen@lumenfold.example>"),
        ];
        for &i in order {
            let (key, from) = mails[i];
            let m = Mail {
                source: "imap:mara",
                key,
                from,
                to: &["Mara Okafor <mara@lumenfold.example>"],
                date: "2026-10-01T10:00:00Z",
                subject: "Hi",
                body: "See you at the review.",
                ..Default::default()
            };
            env.put(m.item(), &[m.body]);
            assert_eq!(run(&k).await.items_failed, 0);
        }
        let mut out: Vec<String> = env
            .entities(EntityKind::Person)
            .into_iter()
            .filter(|e| e.attributes.get("is_me") != Some(&Value::Bool(true)))
            .map(|e| e.name)
            .collect();
        out.sort();
        out
    }
    let want = ["Robert Chen", "Sam Okonkwo"];
    for order in [vec![0, 1, 2, 3, 4, 5], vec![5, 4, 3, 2, 1, 0]] {
        assert_eq!(names(&order).await, want, "{order:?}");
    }
}

/// A recipient at a webmail domain (no organisation).
const TO: &[&str] = &["Robin <robin@gmail.com>"];

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
}

/// Claims referencing `e` whose evidence all comes from `key`.
fn claims_from(env: &Env, e: &EntityId, key: &str) -> Vec<Claim> {
    env.g()
        .claims_for(e, false)
        .unwrap()
        .into_iter()
        .filter(|c| {
            !c.evidence.is_empty() && c.evidence.iter().all(|ev| ev.item.key.as_str() == key)
        })
        .collect()
}

fn split(env: &Env, k: &Knowledge, from: &EntityId, claims: &[Claim], new: NewEntity) -> EntityId {
    let ids: Vec<_> = claims.iter().map(|c| c.id).collect();
    let to = env.g().split(from, &ids, &new).unwrap();
    k.record_split(from, &to).unwrap();
    to
}

fn live(env: &Env, id: &EntityId) -> EntityId {
    env.g().get_entity(id).unwrap().expect("alive").id
}

#[tokio::test]
async fn a_split_off_namesake_stays_split_with_its_facts() {
    let env = Env::new();
    for (key, title) in [
        ("projects/Fennel.md", "Fennel"),
        ("projects/Bramble.md", "Bramble"),
    ] {
        env.put(
            note(
                "files:notes",
                key,
                title,
                json!({"type": "project", "people": ["Jo Marsh"]}),
                &[],
                &[],
                "Planning.",
            ),
            &["Planning."],
        );
    }
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    // Name only: both notes' Jo Marsh are one person.
    let jo = env.entity(EntityKind::Person, "Jo Marsh");
    assert_eq!(env.active(&jo, "works_on").len(), 2);

    // They are two people: the user splits Bramble's Jo off.
    let moved = claims_from(&env, &jo.id, "projects/Bramble.md");
    assert_eq!(moved.len(), 1, "{moved:?}");
    let other = split(
        &env,
        &k,
        &jo.id,
        &moved,
        NewEntity::new(EntityKind::Person, "Jo Marsh"),
    );

    // Re-extract everything (as after a reset or an extractor upgrade).
    k.reset().await.unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    assert_eq!(stats.items_processed, 2);
    assert_eq!(stats.entities_merged, 0);
    let (a, b) = (live(&env, &jo.id), live(&env, &other));
    assert_eq!(a, jo.id);
    assert_eq!(b, other, "never merged back");
    let fennel = env.entity(EntityKind::Project, "Fennel");
    let bramble = env.entity(EntityKind::Project, "Bramble");
    let a_e = env.g().get_entity(&a).unwrap().unwrap();
    let b_e = env.g().get_entity(&b).unwrap().unwrap();
    let a_on = env.active(&a_e, "works_on");
    let b_on = env.active(&b_e, "works_on");
    assert_eq!(a_on.len(), 1, "{a_on:?}");
    assert_eq!(a_on[0].object, ClaimObject::Entity(fennel.id.clone()));
    assert_eq!(b_on.len(), 1, "{b_on:?}");
    assert_eq!(b_on[0].object, ClaimObject::Entity(bramble.id.clone()));
    assert_eq!(b_on[0].id, moved[0].id, "the moved fact keeps its id");
    // Mentions follow: Bramble's note mentions the split-off Jo only.
    let b_items: Vec<String> = env
        .g()
        .mentions_for(&b, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.item.key.as_str().to_owned())
        .collect();
    assert!(!b_items.is_empty());
    assert!(
        b_items.iter().all(|k| k == "projects/Bramble.md"),
        "{b_items:?}"
    );
    assert!(
        env.g()
            .mentions_for(&a, 0)
            .unwrap()
            .iter()
            .all(|m| m.item.key.as_str() == "projects/Fennel.md")
    );
    assert_eq!(
        env.entities(EntityKind::Person)
            .iter()
            .filter(|e| e.name == "Jo Marsh")
            .count(),
        2
    );

    // An edit of Bramble's note is still about the split-off Jo.
    env.put(
        note(
            "files:notes",
            "projects/Bramble.md",
            "Bramble",
            json!({"type": "project", "people": ["Jo Marsh"]}),
            &[],
            &[],
            "Planning, again.",
        ),
        &["Planning, again."],
    );
    run(&k).await;
    assert_eq!(live(&env, &other), other);
    let b_e = env.g().get_entity(&other).unwrap().unwrap();
    assert_eq!(env.active(&b_e, "works_on")[0].id, moved[0].id);

    // Merging by hand lifts the constraint.
    env.g().merge(&jo.id, &other).unwrap();
    k.reset().await.unwrap();
    assert_eq!(run(&k).await.items_failed, 0);
    let jo = env.entity(EntityKind::Person, "Jo Marsh");
    assert_eq!(env.active(&jo, "works_on").len(), 2);
}
