//! Vault validators (also used by `bench knowledge`).
//!
//! - every fact line in a facts block cites at least one footnote that is defined on the page,
//!   unless it is a user-asserted fact marked `(you)`;
//! - so does every history line (`<!-- h:ID -->`, a superseded claim): history is never `(you)`;
//! - claim and history markers in the facts block parse (`<!-- c:ID -->`, `<!-- h:ID -->` on a
//!   bullet);
//! - footnotes referenced in the merge section (`merged` block) are defined;
//! - every link in a generated block resolves to a page in the vault;
//! - entity pages have valid frontmatter (`nugget_id`, `kind`, `aliases`, `identifiers`,
//!   `updated`; stubs carry `merged_into` or `status`).

use std::collections::HashSet;

use nugget_graph::{ClaimId, ClaimStatus, EntityKind};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::doc::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    MissingCitation,
    BrokenLink,
    BadFrontmatter,
    MissingPage,
    UnreadablePage,
    /// A claim or history marker that does not parse.
    BadMarker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    /// Vault-relative page path.
    pub page: String,
    /// 1-based line number, when the issue is on one line.
    pub line: Option<usize>,
    pub kind: IssueKind,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ValidationReport {
    pub pages: u64,
    /// Fact lines seen (history lines included).
    pub facts: u64,
    /// History lines (superseded claims) among `facts`.
    pub history_facts: u64,
    /// Fact lines with at least one resolvable citation (user facts excluded).
    pub cited_facts: u64,
    /// Fact lines marked `(you)`.
    pub user_facts: u64,
    /// Links checked in generated blocks.
    pub links: u64,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    /// Share of non-user facts that carry a citation (1.0 when there are none).
    pub fn citation_coverage(&self) -> f64 {
        let need = self.facts.saturating_sub(self.user_facts);
        if need == 0 {
            1.0
        } else {
            self.cited_facts as f64 / need as f64
        }
    }

    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }

    /// Distinct pages with at least one issue, sorted.
    pub fn invalid_pages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.issues.iter().map(|i| i.page.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn merge(&mut self, other: ValidationReport) {
        self.pages += other.pages;
        self.facts += other.facts;
        self.history_facts += other.history_facts;
        self.cited_facts += other.cited_facts;
        self.user_facts += other.user_facts;
        self.links += other.links;
        self.issues.extend(other.issues);
    }
}

type StatusFn<'a> = &'a dyn Fn(ClaimId) -> Option<ClaimStatus>;

/// Validate one page's text. `link_exists` gets a link target without anchor (e.g.
/// `Person/Sam (ent_1)`) and says whether that page exists.
pub fn validate_page(
    rel: &str,
    text: &str,
    link_exists: &dyn Fn(&str) -> bool,
) -> ValidationReport {
    validate_page_with(rel, text, link_exists, None)
}

pub(crate) fn validate_page_with(
    rel: &str,
    text: &str,
    link_exists: &dyn Fn(&str) -> bool,
    claim_status: Option<StatusFn<'_>>,
) -> ValidationReport {
    let mut r = ValidationReport {
        pages: 1,
        ..Default::default()
    };
    let issue = |r: &mut ValidationReport, line: Option<usize>, kind, message: String| {
        r.issues.push(ValidationIssue {
            page: rel.to_owned(),
            line,
            kind,
            message,
        })
    };
    let doc = Doc::parse(text);
    // Line numbers: map each region line back to the file.
    let lines: Vec<&str> = text.lines().collect();
    let region_start = |name: &str| {
        let b = begin_line(name);
        lines.iter().position(|l| l.trim() == b).map(|i| i + 2)
    };

    if rel != "README.md" {
        check_frontmatter(&doc, &mut |m| {
            issue(&mut r, Some(1), IssueKind::BadFrontmatter, m)
        });
    }

    let defined: HashSet<&str> = lines
        .iter()
        .filter_map(|l| {
            let rest = l.strip_prefix("[^")?;
            let end = rest.find("]:")?;
            Some(&rest[..end])
        })
        .collect();

    if let Some(facts) = doc.region(R_FACTS) {
        let start = region_start(R_FACTS);
        for (i, line) in facts.iter().enumerate() {
            let at = start.map(|s| s + i);
            if has_bad_marker(line) {
                issue(
                    &mut r,
                    at,
                    IssueKind::BadMarker,
                    "claim marker does not parse".into(),
                );
                continue;
            }
            if parse_history(line).is_some() {
                r.facts += 1;
                r.history_facts += 1;
                let refs = footnote_refs(line);
                match refs.iter().find(|x| !defined.contains(x.as_str())) {
                    _ if refs.is_empty() => issue(
                        &mut r,
                        at,
                        IssueKind::MissingCitation,
                        "history line has no citation".into(),
                    ),
                    Some(x) => issue(
                        &mut r,
                        at,
                        IssueKind::MissingCitation,
                        format!("footnote [^{x}] is not defined"),
                    ),
                    None => r.cited_facts += 1,
                }
                continue;
            }
            let Some((claim, _)) = parse_fact(line) else {
                continue;
            };
            r.facts += 1;
            let refs = footnote_refs(line);
            let mine = line
                .rsplit_once(CLAIM_MARK)
                .map_or(line.as_str(), |(a, _)| a)
                .trim_end()
                .ends_with("(you)");
            let undefined: Vec<&String> = refs
                .iter()
                .filter(|x| !defined.contains(x.as_str()))
                .collect();
            if mine {
                r.user_facts += 1;
                if let (Some(f), Some(id)) = (claim_status, claim)
                    && refs.is_empty()
                    && f(id) != Some(ClaimStatus::UserAsserted)
                {
                    issue(
                        &mut r,
                        at,
                        IssueKind::MissingCitation,
                        format!("fact marked (you) but claim {id} is not user-asserted"),
                    );
                }
            } else if refs.is_empty() {
                issue(
                    &mut r,
                    at,
                    IssueKind::MissingCitation,
                    "fact has no citation".into(),
                );
            } else if !undefined.is_empty() {
                issue(
                    &mut r,
                    at,
                    IssueKind::MissingCitation,
                    format!("footnote [^{}] is not defined", undefined[0]),
                );
            } else {
                r.cited_facts += 1;
            }
        }
    }

    if let Some(merged) = doc.region(R_MERGED) {
        let start = region_start(R_MERGED);
        for (i, line) in merged.iter().enumerate() {
            if let Some(x) = footnote_refs(line)
                .iter()
                .find(|x| !defined.contains(x.as_str()))
            {
                issue(
                    &mut r,
                    start.map(|s| s + i),
                    IssueKind::MissingCitation,
                    format!("footnote [^{x}] is not defined"),
                );
            }
        }
    }

    for name in doc.region_names() {
        let start = region_start(name);
        for (i, line) in doc.region(name).unwrap_or_default().iter().enumerate() {
            for (target, _) in wikilinks(line) {
                r.links += 1;
                let page = link_page(&target);
                if page.is_empty() || !link_exists(page) {
                    issue(
                        &mut r,
                        start.map(|s| s + i),
                        IssueKind::BrokenLink,
                        format!("link [[{target}]] does not resolve"),
                    );
                }
            }
        }
    }
    r
}

fn check_frontmatter(doc: &Doc, bad: &mut dyn FnMut(String)) {
    let Some(fm) = &doc.fm else {
        bad("missing frontmatter".into());
        return;
    };
    match fm_get(fm, "nugget_id").and_then(|v| fm_scalar(&v)) {
        Some(id) if !id.is_empty() => {}
        _ => bad("nugget_id is missing or not a string".into()),
    }
    match fm_get(fm, "kind").and_then(|v| fm_scalar(&v)) {
        Some(k) if EntityKind::ALL.iter().any(|x| x.as_str() == k) => {}
        other => bad(format!("kind {other:?} is not an entity kind")),
    }
    if let Some(v) = fm_get(fm, "aliases")
        && fm_list(&v).is_none()
    {
        bad("aliases is not a list of strings".into());
    }
    if let Some(v) = fm_get(fm, "identifiers")
        && serde_json::from_str::<std::collections::BTreeMap<String, Vec<String>>>(&v).is_err()
    {
        bad("identifiers is not a map of string lists".into());
    }
    let merged = fm_get(fm, "merged_into").and_then(|v| fm_scalar(&v));
    let status = fm_get(fm, "status").and_then(|v| fm_scalar(&v));
    match (merged, status) {
        (Some(m), _) if m.is_empty() => bad("merged_into is empty".into()),
        (Some(_), _) => {}
        (None, Some(s)) if s == "retracted" => {}
        (None, Some(s)) => bad(format!("unknown status {s:?}")),
        (None, None) => match fm_get(fm, "updated").and_then(|v| fm_scalar(&v)) {
            Some(u) if OffsetDateTime::parse(&u, &Rfc3339).is_ok() => {}
            other => bad(format!("updated {other:?} is not an RFC 3339 time")),
        },
    }
}
