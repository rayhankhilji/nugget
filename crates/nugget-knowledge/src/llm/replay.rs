//! Replay of QA GG5's real-model claims through the gate (GG5-1, GG5-15).
//!
//! The GG5 runs did not record the model's raw answers, but every claim the model made that
//! the gate let through is in the runs' graph snapshots (`<run>/<snapshot>/entities.json`), with
//! its subject, predicate, object and quote. This test rebuilds, per source file, the answer
//! those claims came from (union over all snapshots of a run), feeds it through
//! [`validate_capped`] against the file's text, and counts how many claims carrying a sensitive
//! value or a junk `has_title` survive. Run it with the GG5 directory (the one holding `runs/`
//! and `corpus/`):
//!
//! ```text
//! NUGGET_GG5_DIR=…/qa/GG5 cargo test -p nugget-knowledge gg5_replay -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use nugget_core::ids::{ItemKey, ItemRef, SourceId};

use super::*;

/// `sensitive_claims.py`'s strings that are values (card, account, amounts, diagnosis,
/// prescription), not names: the gold `sensitive_strings` plus the finance figures.
const VALUES: &[&str] = &[
    "NCU-",
    "4000 0566 5566 5556",
    "7,450",
    "ferrous",
    "anaemia",
    "iron-deficiency",
    "ferritin",
    "heavy periods",
    "sleep diary",
    "210,437.55",
    "214,880.12",
    "4,836.97",
    "5,312",
];

/// `llm_claim_quality.py`'s notion of a job title.
fn title_like(s: &str) -> bool {
    const W: &[&str] = &[
        "engineer",
        "lead",
        "manager",
        "head",
        "director",
        "vp",
        "partner",
        "nurse",
        "officer",
        "counsellor",
        "designer",
        "analyst",
        "chief",
        "principal",
        "staff",
        "president",
        "gp",
        "doctor",
    ];
    let l = s.to_lowercase();
    W.iter().any(|w| l.contains(w)) && s.split_whitespace().count() <= 5
}

#[derive(Default)]
struct FileAnswer {
    entities: BTreeMap<String, String>,
    claims: BTreeSet<(String, String, String, String)>,
}

fn file_item(key: &str, text: &str) -> (Item, Vec<Chunk>) {
    let r = ItemRef {
        source: SourceId::new("files-1"),
        key: ItemKey::new(key),
    };
    let mail = key.ends_with(".eml");
    let header = |name: &str| {
        text.lines()
            .take_while(|l| !l.is_empty())
            .find_map(|l| l.strip_prefix(name).map(|v| v.trim().to_owned()))
    };
    let mut metadata = json!({ "sensitive": false });
    if mail {
        metadata["extracted"] = json!({
            "from": header("From:").unwrap_or_default(),
            "to": header("To:").into_iter().collect::<Vec<_>>(),
        });
    }
    let item = Item {
        item: r.clone(),
        title: header("Subject:"),
        content_hash: "h".into(),
        mime: mail.then(|| "message/rfc822".to_owned()),
        text: Some(text.into()),
        metadata,
        source_modified_at: None,
        version: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let chunks = vec![Chunk {
        id: 1,
        item: r,
        ord: 0,
        text: text.into(),
        title: None,
    }];
    (item, chunks)
}

#[derive(Debug, Default)]
struct Counts {
    claims: usize,
    kept: usize,
    sensitive_in: usize,
    sensitive_kept: usize,
    titles_in: usize,
    titles_kept: usize,
    junk_titles_in: usize,
    junk_titles_kept: usize,
    reasons: BTreeMap<String, usize>,
}

fn replay_run(gg5: &Path, run: &Path) -> Counts {
    let mut files: BTreeMap<String, FileAnswer> = BTreeMap::new();
    let mut snaps: Vec<_> = std::fs::read_dir(run)
        .unwrap()
        .flatten()
        .map(|e| e.path().join("entities.json"))
        .filter(|p| p.exists())
        .collect();
    snaps.sort();
    for snap in snaps {
        let d: Value = serde_json::from_str(&std::fs::read_to_string(&snap).unwrap()).unwrap();
        let details = d["details"].as_object().unwrap();
        let kind_of = |id: &Value| {
            id.as_str()
                .and_then(|id| details.get(id))
                .and_then(|e| e["kind"].as_str())
                .unwrap_or("person")
                .to_owned()
        };
        for e in details.values() {
            for c in e["claims"].as_array().into_iter().flatten() {
                let subject = c["subject"]["name"].as_str().unwrap_or_default().to_owned();
                let (object, okind) = match c["object"]["entity"].as_object() {
                    Some(o) => (
                        o["name"].as_str().unwrap_or_default().to_owned(),
                        Some(kind_of(&o["id"])),
                    ),
                    None => (value_str(&c["object"]["literal"]), None),
                };
                for ev in c["evidence"].as_array().into_iter().flatten() {
                    let (Some(key), Some(quote)) =
                        (ev["item"]["key"].as_str(), ev["quote"].as_str())
                    else {
                        continue;
                    };
                    let f = files.entry(key.to_owned()).or_default();
                    f.entities
                        .insert(subject.clone(), kind_of(&c["subject"]["id"]));
                    if let Some(k) = &okind {
                        f.entities.insert(object.clone(), k.clone());
                    }
                    f.claims.insert((
                        subject.clone(),
                        c["predicate"].as_str().unwrap_or_default().to_owned(),
                        object.clone(),
                        quote.to_owned(),
                    ));
                }
            }
        }
    }
    let mut n = Counts::default();
    let sensitive = |s: &str| VALUES.iter().any(|v| s.contains(v));
    for (key, f) in files {
        let rel = key.split_once(':').map_or(key.as_str(), |(_, r)| r);
        let text = ["phase1", "phase2"]
            .iter()
            .find_map(|p| std::fs::read_to_string(gg5.join("corpus").join(p).join(rel)).ok());
        let Some(text) = text else { continue };
        let (it, chunks) = file_item(&key, &text);
        let sender_is_me = text
            .lines()
            .take_while(|l| !l.is_empty())
            .any(|l| l.starts_with("From:") && l.contains("mara.okafor@"));
        let answer = json!({
            "entities": f.entities.iter()
                .filter(|(_, k)| k.as_str() != "topic")
                .map(|(n, k)| json!({"name": n, "kind": k})).collect::<Vec<_>>(),
            "claims": f.claims.iter().map(|(s, p, o, q)| json!({
                "subject": s, "predicate": p, "object": o, "quote": q
            })).collect::<Vec<_>>(),
        });
        if std::env::var("NUGGET_GG5_KEY").is_ok_and(|k| key.ends_with(&k)) {
            eprintln!("{answer:#}");
        }
        let v = validate_capped(
            &answer,
            &it,
            &chunks,
            &LlmConfig::default(),
            Some("Mara Okafor"),
            sender_is_me,
            (MAX_ENTITIES, MAX_CLAIMS),
        );
        for vd in v.verdicts.iter().filter(|v| v.what == "claim" && !v.kept) {
            *n.reasons.entry(vd.reason.clone()).or_default() += 1;
            if std::env::var_os("NUGGET_GG5_VERBOSE").is_some() {
                eprintln!("  dropped: {key}: {} ({})", vd.proposed, vd.reason);
            }
        }
        let ex = &v.extraction;
        let obj = |c: &ClaimSpec| match &c.object {
            OSpec::Literal(l) => l.clone(),
            OSpec::Entity(r) => ex.name_of(*r, "Mara Okafor"),
        };
        let mut kept: BTreeSet<(String, String, String)> = BTreeSet::new();
        for c in &ex.claims {
            kept.insert((
                ex.name_of(c.subject, "Mara Okafor"),
                c.predicate.clone(),
                obj(c),
            ));
        }
        let proposed: BTreeSet<(&String, &String, &String)> =
            f.claims.iter().map(|(s, p, o, _)| (s, p, o)).collect();
        for (s, p, o) in proposed {
            n.claims += 1;
            let is_sens = sensitive(s) || sensitive(o);
            let is_title = p == "has_title";
            let junk = is_title && !title_like(o);
            n.sensitive_in += usize::from(is_sens);
            n.titles_in += usize::from(is_title);
            n.junk_titles_in += usize::from(junk);
        }
        for (s, p, o) in &kept {
            n.kept += 1;
            let is_sens = sensitive(s) || sensitive(o);
            let is_title = p == "has_title";
            if is_sens {
                eprintln!("  sensitive kept: {key}: {s} | {p} | {o}");
            }
            if is_title && !title_like(o) {
                eprintln!("  junk title kept: {key}: {s} | {o}");
            }
            n.sensitive_kept += usize::from(is_sens);
            n.titles_kept += usize::from(is_title);
            n.junk_titles_kept += usize::from(is_title && !title_like(o));
        }
    }
    n
}

#[test]
#[ignore = "needs NUGGET_GG5_DIR (QA GG5 runs and corpus)"]
fn gg5_replay() {
    let Ok(dir) = std::env::var("NUGGET_GG5_DIR") else {
        eprintln!("NUGGET_GG5_DIR not set");
        return;
    };
    let gg5 = Path::new(&dir);
    let mut runs: Vec<_> = std::fs::read_dir(gg5.join("runs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    runs.sort();
    for run in runs {
        eprintln!("{}", run.file_name().unwrap().to_string_lossy());
        let n = replay_run(gg5, &run);
        eprintln!(
            "REPLAY {}: claims {} -> kept {}; sensitive {} -> {}; has_title {} -> {} (junk {} -> {})",
            run.file_name().unwrap().to_string_lossy(),
            n.claims,
            n.kept,
            n.sensitive_in,
            n.sensitive_kept,
            n.titles_in,
            n.titles_kept,
            n.junk_titles_in,
            n.junk_titles_kept
        );
        eprintln!("  drop reasons: {:?}", n.reasons);
    }
}
