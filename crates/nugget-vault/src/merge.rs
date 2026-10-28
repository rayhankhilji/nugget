//! Three-way merge of the last render (base), the user's file and the new render.
//!
//! The merge is structural, by block:
//! - **User text** (everything outside generated regions, including `## Notes`) is always taken
//!   from the user's file, in the user's order. Links to renamed pages are rewritten in it.
//! - **Frontmatter** merges per key. `nugget_id`, `kind`, `updated` and stub keys always come from
//!   the new render; other generated keys (`aliases`, `identifiers`) take the side that changed,
//!   and keys the user added are kept.
//! - **Generated regions** take the new render if the user left them alone, the user's version if
//!   only the user changed them, and otherwise keep the user's version and report a conflict (the
//!   caller writes the new render to a sidecar).
//! - **The facts region** never conflicts: user edits to fact lines were already applied to the
//!   graph (so the new render contains them), and any other text the user wrote inside it is
//!   re-anchored after the fact line (or group) it followed.
//! - Regions the new render lacks are dropped unless the user changed them, in which case their
//!   text is kept as user text. Regions the user's file lacks are re-inserted in template order.

use std::collections::{BTreeMap, HashSet};

use nugget_graph::ClaimId;

use crate::doc::*;

pub(crate) struct MergeOpts<'a> {
    /// Regions that were in conflict after the previous merge.
    pub prev_conflicts: &'a [String],
    /// `(old link target, new link target)` pairs to rewrite in user text.
    pub renames: &'a [(String, String)],
    /// Lines appended as user text at the end (notes moved here from a merged page).
    pub append: Vec<String>,
}

pub(crate) struct Merged {
    pub doc: Doc,
    pub conflicts: Vec<String>,
}

/// Keys that always come from the new render.
const FORCED: [&str; 5] = ["nugget_id", "kind", "updated", "merged_into", "status"];
/// Keys the renderer owns.
const OURS: [&str; 7] = [
    "nugget_id",
    "kind",
    "updated",
    "merged_into",
    "status",
    "aliases",
    "identifiers",
];

pub(crate) fn merge(base: Option<&Doc>, user: &Doc, new: &Doc, opts: &MergeOpts) -> Merged {
    let prev = |r: &str| opts.prev_conflicts.iter().any(|c| c == r);
    let mut conflicts = Vec::new();
    let (fm, fm_conflict) = merge_fm(
        base.and_then(|b| b.fm.as_deref()),
        user.fm.as_deref(),
        new.fm.as_deref(),
        prev(R_FRONTMATTER),
    );
    if fm_conflict {
        conflicts.push(R_FRONTMATTER.to_owned());
    }

    let mut out: Vec<Segment> = Vec::new();
    for seg in &user.segs {
        match seg {
            Segment::User(lines) => out.push(Segment::User(apply_renames(lines, opts.renames))),
            Segment::Region { name, body } => match new.region(name) {
                Some(nb) => {
                    let b = base.and_then(|b| b.region(name));
                    let (merged, c) = merge_region(name, b, body, nb, prev(name));
                    if c {
                        conflicts.push(name.clone());
                    }
                    out.push(Segment::Region {
                        name: name.clone(),
                        body: merged,
                    });
                }
                None => {
                    let unchanged = base.and_then(|b| b.region(name)) == Some(body.as_slice());
                    if !unchanged && body.iter().any(|l| !l.trim().is_empty()) {
                        out.push(Segment::User(body.clone()));
                    }
                }
            },
        }
    }

    let names = new.region_names();
    for (idx, name) in names.iter().enumerate() {
        if user.region(name).is_some() {
            continue;
        }
        let body = new.region(name).unwrap_or_default().to_vec();
        let pos = names[..idx]
            .iter()
            .rev()
            .find_map(|p| {
                out.iter()
                    .position(|s| matches!(s, Segment::Region { name: n, .. } if n == p))
            })
            .map_or(0, |p| p + 1);
        out.insert(
            pos,
            Segment::Region {
                name: (*name).to_owned(),
                body,
            },
        );
        if pos > 0 {
            out.insert(pos, Segment::User(vec![String::new()]));
        }
    }

    if !opts.append.is_empty() {
        out.push(Segment::User(opts.append.clone()));
    }
    Merged {
        doc: Doc { fm, segs: out },
        conflicts,
    }
}

fn merge_region(
    name: &str,
    base: Option<&[String]>,
    user: &[String],
    new: &[String],
    prev_conflict: bool,
) -> (Vec<String>, bool) {
    if name == R_FACTS
        && let Some(b) = base
    {
        return (reanchor(b, user, new), false);
    }
    if user == new {
        return (new.to_vec(), false);
    }
    match base {
        Some(b) if user == b => (new.to_vec(), false),
        Some(b) if new == b => (user.to_vec(), prev_conflict),
        _ => (user.to_vec(), true),
    }
}

type Entries = Vec<(String, Vec<String>)>;

fn get<'a>(v: &'a Entries, k: &str) -> Option<&'a Vec<String>> {
    v.iter().find(|(kk, _)| kk == k).map(|(_, l)| l)
}

fn merge_fm(
    base: Option<&[String]>,
    user: Option<&[String]>,
    new: Option<&[String]>,
    prev_conflict: bool,
) -> (Option<Vec<String>>, bool) {
    let Some(new) = new else {
        return (user.map(<[String]>::to_vec), false);
    };
    let Some(user) = user else {
        return (Some(new.to_vec()), false);
    };
    let be = base.map(fm_entries).unwrap_or_default();
    let ue = fm_entries(user);
    let ne = fm_entries(new);
    let mut out = Vec::new();
    let mut conflict = false;
    for (k, nl) in &ne {
        if FORCED.contains(&k.as_str()) {
            out.extend(nl.iter().cloned());
            continue;
        }
        let ul = get(&ue, k);
        let bl = get(&be, k);
        let pick = if ul == bl || ul == Some(nl) {
            Some(nl)
        } else if bl == Some(nl) {
            conflict |= prev_conflict && ul.is_some();
            ul
        } else {
            conflict = true;
            ul.or(Some(nl))
        };
        if let Some(p) = pick {
            out.extend(p.iter().cloned());
        }
    }
    for (k, ul) in &ue {
        if get(&ne, k).is_some() {
            continue;
        }
        if OURS.contains(&k.as_str()) && get(&be, k) == Some(ul) {
            continue;
        }
        out.extend(ul.iter().cloned());
    }
    (Some(out), conflict)
}

/// Rewrite `[[old` link targets (followed by `]`, `|`, `#` or `^`) to `[[new`.
pub(crate) fn apply_renames(lines: &[String], renames: &[(String, String)]) -> Vec<String> {
    if renames.is_empty() {
        return lines.to_vec();
    }
    lines
        .iter()
        .map(|l| {
            let mut l = l.clone();
            for (old, new) in renames {
                let pat = format!("[[{old}");
                let mut from = 0;
                while let Some(i) = l[from..].find(&pat) {
                    let at = from + i;
                    let end = at + pat.len();
                    if l[end..].starts_with([']', '|', '#', '^']) {
                        let rep = format!("[[{new}");
                        l.replace_range(at..end, &rep);
                        from = at + rep.len();
                    } else {
                        from = end;
                    }
                }
            }
            l
        })
        .collect()
}

/// One line of a facts region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FLine {
    Blank,
    /// A `### ` group heading with its predicate (from the marker, or derived from the title when
    /// fact bullets follow it).
    Heading(String),
    Fact {
        claim: Option<ClaimId>,
        content: String,
    },
    Other,
}

pub(crate) fn classify_facts(lines: &[String]) -> Vec<FLine> {
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            out.push(FLine::Blank);
        } else if let Some((title, pred)) = parse_group_heading(l) {
            match pred {
                Some(p) => out.push(FLine::Heading(p)),
                None => {
                    let has_facts = lines[i + 1..]
                        .iter()
                        .take_while(|n| !n.starts_with("#"))
                        .any(|n| parse_fact(n).is_some_and(|(_, c)| !c.is_empty()));
                    out.push(if has_facts {
                        FLine::Heading(snake(&title))
                    } else {
                        FLine::Other
                    });
                }
            }
        } else if let Some((claim, content)) = parse_fact(l) {
            out.push(FLine::Fact { claim, content });
        } else {
            out.push(FLine::Other);
        }
    }
    out
}

#[derive(Debug, Clone)]
enum Anchor {
    Start,
    Heading(String),
    Claim(ClaimId, Option<String>),
    Line(String, Option<String>),
    GroupEnd(Option<String>),
}

/// The new facts region with the user's free text (anything that is not a fact line, a group
/// heading or a generated line) re-inserted after whatever it followed in the user's version.
fn reanchor(base: &[String], user: &[String], new: &[String]) -> Vec<String> {
    let generated: HashSet<&str> = base.iter().map(String::as_str).collect();
    let ucls = classify_facts(user);
    let is_prose =
        |i: usize| matches!(ucls[i], FLine::Other) && !generated.contains(user[i].as_str());

    let mut blocks: Vec<(Anchor, Vec<String>)> = Vec::new();
    let mut anchor = Anchor::Start;
    let mut group: Option<String> = None;
    let mut in_prose = false;
    for (i, line) in user.iter().enumerate() {
        match &ucls[i] {
            FLine::Heading(p) => {
                group = Some(p.clone());
                anchor = Anchor::Heading(p.clone());
                in_prose = false;
            }
            FLine::Fact {
                claim: Some(id), ..
            } => {
                anchor = Anchor::Claim(*id, group.clone());
                in_prose = false;
            }
            FLine::Fact { claim: None, .. } => {
                anchor = Anchor::GroupEnd(group.clone());
                in_prose = false;
            }
            FLine::Blank => {
                let next_prose = (i + 1..user.len())
                    .find(|&j| ucls[j] != FLine::Blank)
                    .is_some_and(is_prose);
                if in_prose && next_prose {
                    if let Some((_, b)) = blocks.last_mut() {
                        b.push(line.clone());
                    }
                } else {
                    in_prose = false;
                }
            }
            FLine::Other if !is_prose(i) => {
                anchor = Anchor::Line(line.clone(), group.clone());
                in_prose = false;
            }
            FLine::Other => {
                if in_prose && let Some((_, b)) = blocks.last_mut() {
                    b.push(line.clone());
                } else {
                    blocks.push((anchor.clone(), vec![line.clone()]));
                }
                in_prose = true;
            }
        }
    }
    if blocks.is_empty() {
        return new.to_vec();
    }

    let ncls = classify_facts(new);
    let heading_at =
        |g: &str| (0..new.len()).find(|&i| matches!(&ncls[i], FLine::Heading(p) if p == g));
    let group_end = |g: &Option<String>| -> usize {
        let Some(h) = g.as_deref().and_then(heading_at) else {
            return new.len();
        };
        let mut j = h + 1;
        while j < new.len() && matches!(ncls[j], FLine::Fact { .. }) {
            j += 1;
        }
        j
    };
    let mut inserts: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (a, lines) in blocks {
        let pos = match &a {
            Anchor::Start => 0,
            Anchor::Heading(g) => heading_at(g).map_or(new.len(), |i| i + 1),
            Anchor::Claim(id, g) => (0..new.len())
                .find(|&i| matches!(&ncls[i], FLine::Fact { claim: Some(c), .. } if c == id))
                .map_or_else(|| group_end(g), |i| i + 1),
            Anchor::Line(text, g) => new
                .iter()
                .position(|l| l == text)
                .map_or_else(|| group_end(g), |i| i + 1),
            Anchor::GroupEnd(g) => group_end(g),
        };
        inserts.entry(pos).or_default().extend(lines);
    }
    let mut out = Vec::with_capacity(new.len() + inserts.len());
    for i in 0..=new.len() {
        if let Some(lines) = inserts.remove(&i) {
            out.extend(lines);
        }
        if let Some(l) = new.get(i) {
            out.push(l.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    #[test]
    fn region_rules() {
        let b = v("a");
        assert_eq!(
            merge_region("x", Some(&b), &v("a"), &v("n"), false),
            (v("n"), false)
        );
        assert_eq!(
            merge_region("x", Some(&b), &v("u"), &v("a"), false),
            (v("u"), false)
        );
        assert_eq!(
            merge_region("x", Some(&b), &v("u"), &v("a"), true),
            (v("u"), true)
        );
        assert_eq!(
            merge_region("x", Some(&b), &v("u"), &v("n"), false),
            (v("u"), true)
        );
        assert_eq!(
            merge_region("x", Some(&b), &v("n"), &v("n"), true),
            (v("n"), false)
        );
        assert_eq!(
            merge_region("x", None, &v("u"), &v("n"), false),
            (v("u"), true)
        );
    }

    #[test]
    fn prose_is_reanchored() {
        let base =
            v("## Facts\n\n### A <!-- p:a -->\n- one [^1] <!-- c:1 -->\n- two [^2] <!-- c:2 -->");
        let user = v(
            "## Facts\nintro\n\n### A <!-- p:a -->\n- one [^1] <!-- c:1 -->\n  - sub note\n\npara 1\n\npara 2\n- two [^2] <!-- c:2 -->",
        );
        let new =
            v("## Facts\n\n### A <!-- p:a -->\n- two [^1] <!-- c:2 -->\n- three [^2] <!-- c:3 -->");
        let out = reanchor(&base, &user, &new);
        assert_eq!(
            out,
            v(
                "## Facts\nintro\n\n### A <!-- p:a -->\n- two [^1] <!-- c:2 -->\n- three [^2] <!-- c:3 -->\n  - sub note\n\npara 1\n\npara 2"
            )
        );
    }

    #[test]
    fn renames_rewrite_links() {
        let lines = v("see [[P/Old (e1)|x]] and [[P/Old (e1)]] not [[P/Old (e10)]]");
        let out = apply_renames(&lines, &[("P/Old (e1)".into(), "P/New (e1)".into())]);
        assert_eq!(
            out,
            v("see [[P/New (e1)|x]] and [[P/New (e1)]] not [[P/Old (e10)]]")
        );
    }

    #[test]
    fn frontmatter_keys() {
        let b = v("nugget_id: \"e\"\naliases: []\nupdated: \"1\"");
        let u = v("nugget_id: \"e\"\naliases: [\"Sam\"]\nupdated: \"1\"\ntags: [x]");
        let n = v("nugget_id: \"e\"\naliases: []\nupdated: \"2\"");
        let (fm, c) = merge_fm(Some(&b), Some(&u), Some(&n), false);
        assert!(!c);
        assert_eq!(
            fm.unwrap(),
            v("nugget_id: \"e\"\naliases: [\"Sam\"]\nupdated: \"2\"\ntags: [x]")
        );
        let n2 = v("nugget_id: \"e\"\naliases: [\"Samantha\"]\nupdated: \"2\"");
        let (fm, c) = merge_fm(Some(&b), Some(&u), Some(&n2), false);
        assert!(c);
        assert!(fm.unwrap().contains(&"aliases: [\"Sam\"]".to_string()));
    }
}
