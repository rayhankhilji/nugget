//! People's short names across items: nicknames, first names and initials join a full-name
//! person only with corroboration, whatever order the items arrive in (one per run or several
//! per run), and every join leaves a merge record.

mod common;

use std::collections::BTreeMap;

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_graph::{EntityKind, MERGE_LOG_ATTR, merge_log};
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
    let shuffled = singles([6, 2, 13, 1, 10, 7, 4, 11, 0, 5, 12, 3, 9, 8]);
    let all_at_once = vec![(0..n).collect::<Vec<_>>()];
    let reverse_batches: Vec<Vec<usize>> = (0..n)
        .rev()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(<[usize]>::to_vec)
        .collect();
    // The competing Sam and the Rob/Bob/Delphine mails first, the full names last.
    let competitor_first = vec![
        vec![5, 8, 2, 6, 9, 10],
        vec![11, 12, 1, 7],
        vec![0, 3, 4, 13],
    ];
    for runs in [
        reverse,
        shuffled,
        all_at_once,
        reverse_batches,
        competitor_first,
    ] {
        assert_eq!(
            runs.iter().map(Vec::len).sum::<usize>(),
            n,
            "every item once"
        );
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

/// 2,000 mails among 200 people sharing 20 first names, a quarter of them copying someone
/// known only by a first name at an unknown address: short-name resolution stays bounded.
#[tokio::test]
async fn a_few_thousand_items_with_short_names_stay_fast() {
    const FIRST: [&str; 20] = [
        "Sam", "Alex", "Kim", "Noor", "Jo", "Ari", "Lee", "Mika", "Dana", "Chris", "Maggie", "Bob",
        "Liz", "Theo", "Bea", "Rob", "Pat", "Nick", "Tom", "Ed",
    ];
    const LAST: [&str; 10] = [
        "Smith", "Okafor", "Whitlock", "Chen", "Patel", "Novak", "Garcia", "Kowalski", "Hughes",
        "Larsen",
    ];
    let dir = tempfile::tempdir().unwrap();
    let store = nugget_store::Store::open(
        dir.path().join("nugget.db"),
        &nugget_store::StoreKey::from_bytes([7; 32]),
    )
    .unwrap();
    let graph = std::sync::Arc::new(store.graph());
    let cfg = KnowledgeConfig {
        me: MeConfig {
            name: Some("Robin Ashgrove".into()),
            emails: vec!["robin@ashgrove.test".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let k = Knowledge::new(store.clone(), graph.clone(), None, cfg);
    let n = 2_000;
    for i in 0..n {
        let (f, l) = (FIRST[i % 20], LAST[(i / 20) % 10]);
        let from = format!(
            "{f} {l} <{}.{}@org{}.test>",
            f.to_lowercase(),
            l.to_lowercase(),
            (i / 20) % 10
        );
        let short = format!("{f} <{}{i}@club{}.test>", f.to_lowercase(), i % 5);
        let cc: Vec<&str> = if i % 4 == 0 {
            vec![short.as_str()]
        } else {
            vec![]
        };
        let key = format!("m{i}");
        let body = format!("Note number {i}.");
        let m = Mail {
            source: "imap:robin",
            key: &key,
            from: &from,
            to: ME,
            cc: &cc,
            date: "2026-10-01T10:00:00Z",
            subject: "Hello",
            body: &body,
            ..Default::default()
        };
        store
            .upsert_item_with_chunks(&m.item(), &[nugget_store::NewChunk::text(&body)])
            .unwrap();
    }
    let started = std::time::Instant::now();
    let stats = run(&k).await;
    let took = started.elapsed();
    println!("{n} items: {took:?}");
    assert_eq!(stats.items_failed, 0);
    assert_eq!(stats.items_processed as usize, n);
    assert!(
        took < std::time::Duration::from_secs(120),
        "{n} items took {took:?}"
    );
    // Copied first names at unknown clubs corroborate nobody: no person absorbed them.
    use nugget_graph::GraphStore;
    for s in graph
        .list_entities(&nugget_graph::EntityQuery {
            kind: Some(EntityKind::Person),
            ..Default::default()
        })
        .unwrap()
    {
        let e = graph.get_entity(&s.id).unwrap().unwrap();
        assert!(merge_log(&e).is_empty(), "no joins: {}", e.name);
    }
}
