//! People's short names across items: nicknames, first names and initials join a full-name
//! person only with corroboration, whatever order the items arrive in, and every join leaves a
//! merge record.

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
    key: &'static str,
    item: NewItem,
    body: &'static str,
    /// People the model names (as written).
    llm_people: &'static [&'static str],
}

fn mail(
    key: &'static str,
    from: &'static str,
    cc: &'static [&'static str],
    body: &'static str,
    llm_people: &'static [&'static str],
) -> Case {
    let m = Mail {
        source: "imap:robin",
        key,
        from,
        to: ME,
        cc,
        date: "2026-10-01T10:00:00Z",
        subject: "Hello",
        body,
        ..Default::default()
    };
    Case {
        key,
        item: m.item(),
        body,
        llm_people,
    }
}

fn journal(key: &'static str, body: &'static str, llm_people: &'static [&'static str]) -> Case {
    Case {
        key,
        item: note("files:notes", key, key, Value::Null, &[], &[], body),
        body,
        llm_people,
    }
}

fn scenario() -> Vec<Case> {
    vec![
        mail(
            "a",
            "Margaret Thistlewood <margaret.thistlewood@cindergate.test>",
            &[],
            "Thanks for the update on the survey. Margaret",
            &[],
        ),
        // A colleague of hers calls her by her nickname (same organisation).
        mail(
            "b",
            "Bart Fenwick <bart.fenwick@cindergate.test>",
            &[],
            "Maggie tells me Lodestar is on track.",
            &["Maggie"],
        ),
        // A first name next to a club-mate (same organisation, known only from other items).
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

/// Process the scenario's items in `order` (one run per item) and return the partition of
/// person mentions: for each person, the (item, surface) pairs that resolved to them.
async fn partition(order: &[usize]) -> (Vec<Vec<(String, String)>>, Env, Knowledge) {
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
    for &i in order {
        let c = &cases[i];
        env.put(c.item.clone(), &[c.body]);
        let stats = run(&k).await;
        assert_eq!(stats.items_failed, 0, "{}", c.key);
    }
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
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

fn group_of<'a>(
    groups: &'a [Vec<(String, String)>],
    key: &str,
    surface: &str,
) -> &'a [(String, String)] {
    groups
        .iter()
        .find(|g| g.iter().any(|(k, s)| k == key && s == surface))
        .unwrap_or_else(|| panic!("no mention {key}/{surface} in {groups:#?}"))
}

#[tokio::test]
async fn the_people_do_not_depend_on_item_order() {
    let n = scenario().len();
    let forward: Vec<usize> = (0..n).collect();
    let reverse: Vec<usize> = (0..n).rev().collect();
    let interleaved: Vec<usize> = (0..n).step_by(2).chain((1..n).step_by(2)).collect();
    let shuffled: Vec<usize> = vec![6, 2, 1, 7, 4, 0, 5, 3];
    assert_eq!(shuffled.len(), n);

    let (want, env, knowledge) = partition(&forward).await;
    // The expected people.
    let k = |s: &str| s.to_string();
    assert_eq!(
        group_of(&want, "b", "Maggie"),
        [
            (k("a"), k("Margaret Thistlewood")),
            (k("b"), k("Maggie")),
            (k("h"), k("M. Thistlewood")),
            (k("h"), k("Margaret Thistlewood")),
        ]
    );
    assert_eq!(
        group_of(&want, "rowing.md", "Sam"),
        [(k("d"), k("Sam Whitlock")), (k("rowing.md"), k("Sam")),]
    );
    assert_eq!(
        group_of(&want, "f", "Sam Okafor"),
        [(k("f"), k("Sam Okafor"))]
    );
    // Never merged on a first name alone.
    assert_eq!(
        group_of(&want, "coffee.md", "Sam"),
        [(k("coffee.md"), k("Sam"))]
    );
    assert_eq!(
        group_of(&want, "coffee.md", "Theo"),
        [(k("coffee.md"), k("Theo"))]
    );

    // Why: every join is on record.
    let maggie = env.entity(EntityKind::Person, "Margaret Thistlewood");
    let log = merge_log(&maggie);
    assert!(maggie.attributes.get(MERGE_LOG_ATTR).is_some());
    let by_name: BTreeMap<&str, &Vec<String>> =
        log.iter().map(|r| (r.name.as_str(), &r.reasons)).collect();
    assert!(
        by_name["Maggie"].contains(&"nickname".to_string()),
        "{log:#?}"
    );
    assert!(
        by_name["Maggie"].contains(&"same_org".to_string()),
        "{log:#?}"
    );
    assert!(
        by_name["M. Thistlewood"].contains(&"initial".to_string()),
        "{log:#?}"
    );
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
            && r.from.is_some()
            && r.reasons == ["first_name", "email_local_part", "same_org"]),
        "{log:#?}"
    );
    // Details name nobody and quote nothing from the source.
    for e in [&maggie, &sam] {
        for r in merge_log(e) {
            assert!(r.detail.len() <= 80, "{}", r.detail);
            assert!(!r.detail.contains('@') && !r.detail.contains("Thistlewood"));
        }
    }

    for order in [reverse, interleaved, shuffled] {
        let (got, _, _) = partition(&order).await;
        assert_eq!(got, want, "order {order:?}");
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
    let h = &cases[7];
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
