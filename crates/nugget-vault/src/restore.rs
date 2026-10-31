//! Pages that lost their invisible markers.
//!
//! Some editors, formatters, web clippers and sync tools drop every HTML comment. A page saved
//! that way has no `<!-- nugget:begin … -->` regions and no `<!-- c:N -->` fact markers left, so
//! the merge would see only user text: the user's edits would never reach the graph, and the
//! generated regions would be written again above a stale copy of the whole page.
//!
//! When the user's file has no region markers but the last render (the merge base) had them,
//! [`restore_markers`] puts the markers back. It aligns the base, with its comments removed,
//! with the user's lines (longest common subsequence):
//! - a line the user kept, or moved (the same text anywhere among the changed lines), gets its
//!   base line back, comments included;
//! - within a run of changed lines, a user line that is clearly a version of a base line (the
//!   same link target, or at least half of the words in common) keeps that base line's trailing
//!   markers, so a reworded fact line stays the same claim and becomes an edit of it; other
//!   lines get no marker (a new fact, or text);
//! - lines that were nothing but a marker are put back where they were.
//!
//! The markers are only put back when at least half of the base's fact lines were found again;
//! otherwise (an emptied or rewritten page) the page is left as it is, which never retracts
//! anything. And since a page without markers is weak evidence of deletions, only facts whose
//! lines were removed outright between two lines the user kept count as deleted
//! ([`Restored::deleted`]); the caller rejects no others.

use std::collections::{HashMap, HashSet, VecDeque};

use nugget_graph::ClaimId;

use crate::doc::{BEGIN, CLAIM_MARK, END, wikilinks};

/// Longest page (in lines, base times user) aligned; longer pages are left as they are.
const MAX_CELLS: usize = 4_000_000;

/// A user page with its markers put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Restored {
    pub text: String,
    /// Facts whose lines the user removed outright (the only ones a deletion may reject).
    pub deleted: HashSet<ClaimId>,
}

enum Step {
    Same(usize),
    Changed(Vec<usize>, Vec<usize>),
}

/// `user` with the markers of `base` put back, if `user` lost all its region markers, `base`
/// had some, and enough of the base's facts are found again; `None` otherwise.
pub(crate) fn restore_markers(base: &str, user: &str) -> Option<Restored> {
    if user.contains(BEGIN) || !base.contains(BEGIN) {
        return None;
    }
    let base = base.replace("\r\n", "\n");
    let user = user.replace("\r\n", "\n");
    let b: Vec<&str> = base.lines().collect();
    let u: Vec<&str> = user.lines().collect();
    if b.len().saturating_mul(u.len()) > MAX_CELLS {
        return None;
    }
    let bs: Vec<String> = b.iter().map(|l| strip_comments(l)).collect();
    let us: Vec<String> = u.iter().map(|l| strip_comments(l)).collect();
    let is_marker = |i: usize| bs[i].is_empty() && b[i].contains("<!--");

    // LCS table, from the end.
    let (n, m) = (b.len(), u.len());
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[at(i, j)] = if bs[i] == us[j] {
                t[at(i + 1, j + 1)] + 1
            } else {
                t[at(i + 1, j)].max(t[at(i, j + 1)])
            };
        }
    }
    let mut steps: Vec<Step> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut hb, mut hu) = (Vec::new(), Vec::new());
    while i < n || j < m {
        if i < n && j < m && bs[i] == us[j] {
            if !hb.is_empty() || !hu.is_empty() {
                steps.push(Step::Changed(
                    std::mem::take(&mut hb),
                    std::mem::take(&mut hu),
                ));
            }
            steps.push(Step::Same(i));
            i += 1;
            j += 1;
        } else if j < m && (i == n || t[at(i, j + 1)] >= t[at(i + 1, j)]) {
            hu.push(j);
            j += 1;
        } else {
            hb.push(i);
            i += 1;
        }
    }
    if !hb.is_empty() || !hu.is_empty() {
        steps.push(Step::Changed(hb, hu));
    }

    // Which base line each changed user line takes its markers from: first lines moved
    // unchanged (anywhere), then versions of a base line in the same run.
    let mut from: HashMap<usize, usize> = HashMap::new();
    let mut taken = vec![false; n];
    let mut by_text: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for s in &steps {
        if let Step::Changed(hb, _) = s {
            for &i in hb.iter().filter(|&&i| !bs[i].is_empty()) {
                by_text.entry(bs[i].as_str()).or_default().push_back(i);
            }
        }
    }
    for s in &steps {
        if let Step::Changed(_, hu) = s {
            for &j in hu {
                if let Some(i) = by_text
                    .get_mut(us[j].as_str())
                    .and_then(VecDeque::pop_front)
                {
                    from.insert(j, i);
                    taken[i] = true;
                }
            }
        }
    }
    for s in &steps {
        let Step::Changed(hb, hu) = s else { continue };
        for &j in hu {
            if from.contains_key(&j) || us[j].is_empty() {
                continue;
            }
            let best = hb
                .iter()
                .copied()
                .filter(|&i| !taken[i] && !is_marker(i) && !bs[i].is_empty())
                .map(|i| (similarity(&bs[i], &us[j]), i))
                .filter(|(s, _)| *s >= 0.5)
                .max_by(|x, y| x.0.total_cmp(&y.0).then(y.1.cmp(&x.1)));
            if let Some((_, i)) = best {
                from.insert(j, i);
                taken[i] = true;
            }
        }
    }

    // Enough of the base's facts found again?
    let facts: Vec<usize> = (0..n).filter(|&i| claim_of(b[i]).is_some()).collect();
    let mut found = vec![false; n];
    for s in &steps {
        if let Step::Same(i) = s {
            found[*i] = true;
        }
    }
    let matched = facts.iter().filter(|&&i| found[i] || taken[i]).count();
    if matched * 2 < facts.len() {
        return None;
    }

    // Facts removed outright: a run with no user lines, between two kept lines.
    let mut deleted = HashSet::new();
    for (k, s) in steps.iter().enumerate() {
        if let Step::Changed(hb, hu) = s
            && hu.is_empty()
            && k > 0
            && matches!(steps[k - 1], Step::Same(_))
            && matches!(steps.get(k + 1), Some(Step::Same(_)))
        {
            deleted.extend(
                hb.iter()
                    .filter(|&&i| !taken[i])
                    .filter_map(|&i| claim_of(b[i])),
            );
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(n.max(m));
    for s in &steps {
        match s {
            Step::Same(i) => out.push(b[*i].to_owned()),
            Step::Changed(hb, hu) => {
                let lines: Vec<String> = hu
                    .iter()
                    .map(|&j| match from.get(&j) {
                        Some(&i) if bs[i] == us[j] => b[i].to_owned(),
                        Some(&i) => {
                            let marks = trailing_comments(b[i]);
                            if marks.is_empty() {
                                u[j].to_owned()
                            } else {
                                format!("{} {marks}", u[j].trim_end())
                            }
                        }
                        None => u[j].to_owned(),
                    })
                    .collect();
                // The user's lines go before the first region end in the run (so they stay
                // inside the region they were written in), else where the first replaced line
                // was, else at the end; marker-only base lines stay in their order.
                let pos = hb
                    .iter()
                    .position(|&i| b[i].trim_start().starts_with(END))
                    .or_else(|| hb.iter().position(|&i| !is_marker(i)))
                    .unwrap_or(hb.len());
                let mut lines = Some(lines);
                for (k, &i) in hb.iter().enumerate() {
                    if k == pos {
                        out.extend(lines.take().unwrap_or_default());
                    }
                    if is_marker(i) {
                        out.push(b[i].to_owned());
                    }
                }
                out.extend(lines.take().unwrap_or_default());
            }
        }
    }
    let mut text = out.join("\n");
    if user.ends_with('\n') {
        text.push('\n');
    }
    Some(Restored { text, deleted })
}

/// The claim a line's `<!-- c:N -->` marker names.
fn claim_of(line: &str) -> Option<ClaimId> {
    let rest = &line[line.find(CLAIM_MARK)? + CLAIM_MARK.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// How clearly `user` is a version of `base`: 1 for the same first link target, else the
/// share of words in common (footnote numbers aside).
fn similarity(base: &str, user: &str) -> f64 {
    let link = |l: &str| wikilinks(l).into_iter().next().map(|(t, _)| t);
    if let Some(t) = link(base)
        && link(user).as_ref() == Some(&t)
    {
        return 1.0;
    }
    let words = |l: &str| -> HashSet<String> {
        l.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_lowercase)
            .collect()
    };
    let (a, b) = (words(base), words(user));
    let all = a.union(&b).count();
    if all == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / all as f64
}

/// `line` without its `<!-- … -->` comments (and the space before each), trailing space trimmed.
fn strip_comments(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(s) = rest.find("<!--") {
        let Some(e) = rest[s..].find("-->") else {
            break;
        };
        out.push_str(rest[..s].strip_suffix(' ').unwrap_or(&rest[..s]));
        rest = &rest[s + e + 3..];
    }
    out.push_str(rest);
    out.trim_end().to_owned()
}

/// The comments at the end of `line` (`<!-- c:3 -->`), as written, or "".
fn trailing_comments(line: &str) -> String {
    let mut t = line.trim_end();
    let mut start = t.len();
    while t.ends_with("-->") {
        let Some(s) = t.rfind("<!--") else { break };
        start = s;
        t = t[..s].trim_end();
    }
    line.trim_end()[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "---\nnugget_id: \"e1\"\naliases: []\n---\n<!-- nugget:begin header -->\n# Sam Patel\n<!-- nugget:end header -->\n\n<!-- nugget:begin facts -->\n## Facts\n\n### Works at <!-- p:works_at -->\n- Sam Patel works at [[O/Acme (e2)|Acme]]. [^1] <!-- c:18 -->\n- Sam Patel likes tea. [^2] <!-- c:19 -->\n- Sam Patel lives in Oslo. [^2] <!-- c:20 -->\n<!-- nugget:end facts -->\n\n<!-- nugget:begin sources -->\n[^1]: a.md\n[^2]: b.md\n<!-- nugget:end sources -->\n";

    fn strip(t: &str) -> String {
        t.lines().map(strip_comments).collect::<Vec<_>>().join("\n") + "\n"
    }

    fn restore(user: &str) -> Restored {
        restore_markers(BASE, user).expect("restored")
    }

    #[test]
    fn untouched_stripped_page_gets_the_base_back() {
        let r = restore(&strip(BASE));
        assert_eq!(r.text, BASE);
        assert!(r.deleted.is_empty());
        assert!(restore_markers(BASE, BASE).is_none(), "markers present");
        assert!(restore_markers("no markers\n", "x\n").is_none());
    }

    #[test]
    fn reworded_line_keeps_its_claim_marker() {
        let user = strip(BASE).replace(
            "Sam Patel works at",
            "Sam Patel is a senior data engineer at",
        ) + "\nMy note on Sam Patel.\n";
        let out = restore(&user).text;
        assert!(out.contains(
            "- Sam Patel is a senior data engineer at [[O/Acme (e2)|Acme]]. [^1] <!-- c:18 -->\n"
        ));
        assert!(out.contains("- Sam Patel likes tea. [^2] <!-- c:19 -->\n"));
        assert!(out.contains("<!-- nugget:begin facts -->\n## Facts"));
        assert!(out.ends_with("<!-- nugget:end sources -->\n\nMy note on Sam Patel.\n"));
        assert_eq!(out.matches("# Sam Patel\n").count(), 1);
    }

    #[test]
    fn comment_lines_removed_entirely_and_added_bullets() {
        // An editor that drops comment lines altogether, and a bullet added under the facts.
        let user: String = BASE
            .lines()
            .filter(|l| !(l.starts_with("<!--") && l.ends_with("-->")))
            .map(strip_comments)
            .flat_map(|l| {
                if l.contains("lives in Oslo") {
                    vec![l, "- Sam Patel plays chess".to_owned()]
                } else {
                    vec![l]
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let out = restore(&user).text;
        assert!(out.contains(
            "- Sam Patel lives in Oslo. [^2] <!-- c:20 -->\n- Sam Patel plays chess\n<!-- nugget:end facts -->"
        ), "{out}");
        assert!(
            out.contains("<!-- nugget:begin header -->\n# Sam Patel\n<!-- nugget:end header -->")
        );
    }

    /// Review finding 1: an emptied or rewritten page is not "every fact deleted".
    #[test]
    fn emptied_or_rewritten_pages_are_left_alone() {
        assert!(restore_markers(BASE, "").is_none());
        assert!(restore_markers(BASE, "# Totally new page\n\nMy own notes.\n").is_none());
        // Only the first fact left: one of three found, not enough.
        let user = "# Sam Patel\n- Sam Patel works at [[O/Acme (e2)|Acme]]. [^1]\n";
        assert!(restore_markers(BASE, user).is_none());
    }

    /// Review finding 2: moved lines keep their markers; markers go to similar lines only.
    #[test]
    fn moved_edited_and_unrelated_lines() {
        // Moved: B before A.
        let s = strip(BASE);
        let a = "- Sam Patel works at [[O/Acme (e2)|Acme]]. [^1]\n";
        let b = "- Sam Patel likes tea. [^2]\n";
        let moved = s.replace(&format!("{a}{b}"), &format!("{b}{a}"));
        let r = restore(&moved);
        assert!(r.text.contains("- Sam Patel likes tea. [^2] <!-- c:19 -->\n- Sam Patel works at [[O/Acme (e2)|Acme]]. [^1] <!-- c:18 -->\n"), "{}", r.text);
        assert!(r.deleted.is_empty());

        // Edited and a line added after it.
        let edit_add = s.replace(
            a,
            "- Sam Patel works at [[O/Acme (e2)|Acme]] as CTO. [^1]\n- Sam Patel rides a bike\n",
        );
        let r = restore(&edit_add);
        assert!(r.text.contains("- Sam Patel works at [[O/Acme (e2)|Acme]] as CTO. [^1] <!-- c:18 -->\n- Sam Patel rides a bike\n"), "{}", r.text);

        // A replaced by an unrelated line, B reworded: only B keeps its marker.
        let unrelated = s.replace(
            &format!("{a}{b}"),
            "- Sam Patel likes green tea. [^2]\n- Cara plays chess\n",
        );
        let r = restore(&unrelated);
        assert!(
            r.text
                .contains("- Sam Patel likes green tea. [^2] <!-- c:19 -->\n- Cara plays chess\n"),
            "{}",
            r.text
        );
        assert!(!r.text.contains("c:18"));
        assert!(r.deleted.is_empty(), "replaced, not removed outright");
    }

    /// A line removed outright between kept lines is a deletion.
    #[test]
    fn removed_line_between_kept_lines_is_deleted() {
        let user = strip(BASE).replace("- Sam Patel likes tea. [^2]\n", "");
        let r = restore(&user);
        assert_eq!(r.deleted, HashSet::from([19]));
        assert!(!r.text.contains("c:19"));
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_comments("- a [^1] <!-- c:3 -->"), "- a [^1]");
        assert_eq!(strip_comments("### A <!-- p:a --> x"), "### A x");
        assert_eq!(trailing_comments("- a [^1] <!-- c:3 -->"), "<!-- c:3 -->");
        assert_eq!(
            trailing_comments("- a <!-- x --> <!-- c:3 -->"),
            "<!-- x --> <!-- c:3 -->"
        );
        assert_eq!(trailing_comments("- a"), "");
        assert_eq!(claim_of("- a [^1] <!-- c:31 -->"), Some(31));
        assert_eq!(claim_of("- a"), None);
        assert!(similarity("- B likes tea.", "- B likes green tea.") >= 0.5);
        assert!(similarity("- A works at Acme.", "- C plays chess") < 0.5);
    }
}
