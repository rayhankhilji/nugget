//! Turning user edits of generated fact lines into graph changes.
//!
//! Each generated fact line ends with an invisible `<!-- c:ID -->` marker naming its claim. The
//! user's facts region is compared with the last render (the merge base), line by marker:
//! - a marked line whose content changed: the old claim becomes `superseded` and the new text
//!   becomes a `user_asserted` claim (same subject and predicate, no evidence);
//! - a marked line that is gone (or emptied): the claim becomes `retracted`;
//! - a new bullet without a marker: a new `user_asserted` claim, with the predicate of the group
//!   heading it sits under (`note` outside any group).
//!
//! "Content" ignores the marker, footnote references and the `(you)` tag, so removing a citation
//! is not an edit. A link in the new text (`[[Kind/Name (id)|Name]]`) makes the claim's object
//! that entity; otherwise the object is the text itself. Aliases added to the frontmatter become
//! graph aliases.

use std::collections::{BTreeMap, HashSet};

use nugget_graph::ClaimId;

use crate::doc::*;
use crate::merge::{FLine, classify_facts};

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
    let base_contents: HashSet<&str> = base_facts.values().map(|(c, _)| c.as_str()).collect();

    let mut seen = HashSet::new();
    let mut group: Option<String> = None;
    for l in classify_facts(u) {
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
                if content.is_empty() || base_contents.contains(content.as_str()) {
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

    #[test]
    fn missing_region_is_not_mass_retraction() {
        let base = page("- A [^1] <!-- c:1 -->");
        let user = Doc::parse("---\naliases: [\"Al\"]\n---\njust notes\n");
        let p = plan_edits(&base, &user);
        assert!(p.retract.is_empty());
        assert_eq!(p.aliases, vec!["Al".to_string()]);
    }
}
