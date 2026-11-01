//! Turning user edits of generated fact lines into graph changes.

use std::collections::{BTreeMap, HashMap, HashSet};

use nugget_graph::ClaimId;

use crate::doc::*;
use crate::merge::{FLine, classify_facts, classify_facts_with, history_ids};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewFact {
    pub predicate: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EditPlan {
    pub retract: Vec<ClaimId>,
    pub supersede: Vec<(ClaimId, NewFact)>,
    pub add: Vec<NewFact>,
    pub aliases: Vec<String>,
}

impl EditPlan {
    pub fn is_empty(&self) -> bool {
        self.retract.is_empty()
            && self.supersede.is_empty()
            && self.add.is_empty()
            && self.aliases.is_empty()
    }
}

pub(crate) fn plan_edits(base: &Doc, user: &Doc) -> EditPlan {
    let mut plan = EditPlan::default();

    if let (Some(bf), Some(uf)) = (&base.fm, &user.fm) {
        let had = fm_get(bf, "aliases")
            .and_then(|v| fm_list(&v))
            .unwrap_or_default();
        for a in fm_get(uf, "aliases")
            .and_then(|v| fm_list(&v))
            .unwrap_or_default()
        {
            let a = collapse_ws(&a);
            if !a.is_empty() && !had.contains(&a) && !plan.aliases.contains(&a) {
                plan.aliases.push(a);
            }
        }
    }

    // A missing facts block (markers deleted) is not a request to retract everything.
    let (Some(b), Some(u)) = (base.region(R_FACTS), user.region(R_FACTS)) else {
        return plan;
    };
    let mut base_facts: BTreeMap<ClaimId, (String, String)> = BTreeMap::new();
    let mut group = "note".to_owned();
    for l in classify_facts(b) {
        match l {
            FLine::Heading(p) => group = p,
            FLine::Fact {
                claim: Some(id),
                content,
            } => {
                base_facts.entry(id).or_insert((content, group.clone()));
            }
            _ => {}
        }
    }
    // A base fact's text on a line without its marker (moved or retyped as it was) keeps that
    // fact, unless its marked line is on the page too.
    let mut base_contents: HashMap<&str, ClaimId> = HashMap::new();
    for (id, (c, _)) in &base_facts {
        base_contents.entry(c.as_str()).or_insert(*id);
    }
    let marked: HashSet<ClaimId> = classify_facts(u)
        .into_iter()
        .filter_map(|l| match l {
            FLine::Fact {
                claim: Some(id),
                content,
            } if !content.is_empty() && base_facts.contains_key(&id) => Some(id),
            _ => None,
        })
        .collect();

    let mut seen = HashSet::new();
    let mut group: Option<String> = None;
    for l in classify_facts_with(u, Some(&history_ids(b))) {
        match l {
            FLine::Heading(p) => group = Some(p),
            FLine::Fact { claim, content } => {
                if let Some(id) = claim
                    && let Some((old, pred)) = base_facts.get(&id)
                    && !seen.contains(&id)
                {
                    if content.is_empty() {
                        continue; // emptied line: retracted below
                    }
                    seen.insert(id);
                    if content != *old {
                        plan.supersede.push((
                            id,
                            NewFact {
                                predicate: pred.clone(),
                                content,
                            },
                        ));
                    }
                    continue;
                }
                if content.is_empty() {
                    continue;
                }
                if let Some(&id) = base_contents.get(content.as_str())
                    && !marked.contains(&id)
                {
                    seen.insert(id);
                    continue;
                }
                let nf = NewFact {
                    predicate: group.clone().unwrap_or_else(|| "note".into()),
                    content,
                };
                if !plan.add.contains(&nf) {
                    plan.add.push(nf);
                }
            }
            _ => {}
        }
    }
    plan.retract = base_facts
        .keys()
        .filter(|id| !seen.contains(*id))
        .copied()
        .collect();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(facts: &str) -> Doc {
        Doc::parse(&format!(
            "---\naliases: []\n---\n<!-- nugget:begin facts -->\n{facts}\n<!-- nugget:end facts -->\n"
        ))
    }

    #[test]
    fn edits_deletes_and_additions() {
        let base = page(
            "## Facts\n\n### Works at <!-- p:works_at -->\n- A [^1] <!-- c:1 -->\n- B [^2] <!-- c:2 -->\n- C [^3] <!-- c:3 -->",
        );
        let user = page(
            "## Facts\n\n### Works at <!-- p:works_at -->\n- A <!-- c:1 -->\n- B changed [^2] <!-- c:2 -->\n- new one\n\n### Hobbies\n- Chess\n- <!-- c:3 -->",
        );
        let p = plan_edits(&base, &user);
        assert_eq!(p.retract, vec![3]);
        assert_eq!(
            p.supersede,
            vec![(
                2,
                NewFact {
                    predicate: "works_at".into(),
                    content: "B changed".into()
                }
            )]
        );
        assert_eq!(
            p.add,
            vec![
                NewFact {
                    predicate: "works_at".into(),
                    content: "new one".into()
                },
                NewFact {
                    predicate: "hobbies".into(),
                    content: "Chess".into()
                }
            ]
        );
    }

    /// Review finding 2: a fact line moved or retyped without its marker is not a deletion.
    #[test]
    fn unmarked_copy_of_a_base_fact_keeps_it() {
        let base = page("- A [^1] <!-- c:1 -->\n- B [^2] <!-- c:2 -->");
        let user = page("- B [^2]\n- A <!-- c:1 -->");
        let p = plan_edits(&base, &user);
        assert!(p.retract.is_empty(), "{p:?}");
        assert!(p.add.is_empty());
        // With its marked line still there, an unmarked copy is a fact of the user's own.
        let user = page("- B [^2]\n- B changed <!-- c:2 -->\n- A <!-- c:1 -->");
        let p = plan_edits(&base, &user);
        assert!(p.retract.is_empty());
        assert_eq!(p.supersede.len(), 1);
        assert_eq!(
            p.add,
            vec![NewFact {
                predicate: "note".into(),
                content: "B".into()
            }]
        );
    }

    #[test]
    fn missing_region_is_not_mass_retraction() {
        let base = page("- A [^1] <!-- c:1 -->");
        let user = Doc::parse("---\naliases: [\"Al\"]\n---\njust notes\n");
        let p = plan_edits(&base, &user);
        assert!(p.retract.is_empty());
        assert_eq!(p.aliases, vec!["Al".to_string()]);
    }
}
