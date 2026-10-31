//! Pages that lost their invisible markers.
//!
//! Some editors, formatters, web clippers and sync tools drop every HTML comment. A page saved
//! that way has no `<!-- nugget:begin … -->` regions and no `<!-- c:N -->` fact markers left, so
//! the merge would see only user text: the user's edits would never reach the graph, and the
//! generated regions would be written again above a stale copy of the whole page.
//!
//! When the user's file has no region markers but the last render (the merge base) had them,
//! [`restore_markers`] puts the markers back: it aligns the base, with its comments removed,
//! with the user's lines (longest common subsequence). Lines the user kept get the base line
//! back, comments included. A run of lines the user changed that replaces as many base lines
//! keeps each base line's trailing markers on the user's version, so a reworded fact line stays
//! the same claim and becomes an edit of it. Lines that were nothing but a marker are put back
//! where they were. Everything else the user wrote stays where it is, as user text.

use crate::doc::BEGIN;

/// Longest page (in lines, base times user) aligned; longer pages are left as they are.
const MAX_CELLS: usize = 4_000_000;

/// `user` with the markers of `base` put back, if `user` lost all its region markers and
/// `base` had some; `None` otherwise (nothing to restore, or too large to align).
pub(crate) fn restore_markers(base: &str, user: &str) -> Option<String> {
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

    let mut out: Vec<String> = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (0, 0);
    let (mut hb, mut hu) = (Vec::new(), Vec::new());
    while i < n || j < m {
        if i < n && j < m && bs[i] == us[j] {
            hunk(&b, &bs, &u, &hb, &hu, &mut out);
            hb.clear();
            hu.clear();
            out.push(b[i].to_owned());
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
    hunk(&b, &bs, &u, &hb, &hu, &mut out);
    let mut text = out.join("\n");
    if user.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// Emit one changed run: base lines `hb` replaced by user lines `hu`.
fn hunk(b: &[&str], bs: &[String], u: &[&str], hb: &[usize], hu: &[usize], out: &mut Vec<String>) {
    let is_marker = |i: usize| bs[i].is_empty() && b[i].contains("<!--");
    let content: Vec<usize> = hb.iter().copied().filter(|&i| !is_marker(i)).collect();
    if content.len() == hu.len() {
        // Line for line: each user line keeps its base line's markers.
        let mut k = 0;
        for &i in hb {
            if is_marker(i) {
                out.push(b[i].to_owned());
                continue;
            }
            let line = u[hu[k]];
            k += 1;
            let marks = trailing_comments(b[i]);
            if marks.is_empty() || line.trim().is_empty() {
                out.push(line.to_owned());
            } else {
                out.push(format!("{} {marks}", line.trim_end()));
            }
        }
        return;
    }
    // Otherwise the user's lines go before the first region end in the run (so they stay inside
    // the region they were written in), else where the first replaced line was, else at the end.
    let pos = hb
        .iter()
        .position(|&i| b[i].trim_start().starts_with(crate::doc::END))
        .or_else(|| hb.iter().position(|&i| !is_marker(i)))
        .unwrap_or(hb.len());
    for (k, &i) in hb.iter().enumerate() {
        if k == pos {
            out.extend(hu.iter().map(|&j| u[j].to_owned()));
        }
        if is_marker(i) {
            out.push(b[i].to_owned());
        }
    }
    if pos == hb.len() {
        out.extend(hu.iter().map(|&j| u[j].to_owned()));
    }
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

    const BASE: &str = "---\nnugget_id: \"e1\"\naliases: []\n---\n<!-- nugget:begin header -->\n# Sam Patel\n<!-- nugget:end header -->\n\n<!-- nugget:begin facts -->\n## Facts\n\n### Works at <!-- p:works_at -->\n- Sam Patel works at [[O/Acme (e2)|Acme]]. [^1] <!-- c:18 -->\n- Sam Patel likes tea. [^2] <!-- c:19 -->\n<!-- nugget:end facts -->\n\n<!-- nugget:begin sources -->\n[^1]: a.md\n[^2]: b.md\n<!-- nugget:end sources -->\n";

    fn strip(t: &str) -> String {
        t.lines().map(strip_comments).collect::<Vec<_>>().join("\n") + "\n"
    }

    #[test]
    fn untouched_stripped_page_gets_the_base_back() {
        assert_eq!(restore_markers(BASE, &strip(BASE)).unwrap(), BASE);
        assert!(restore_markers(BASE, BASE).is_none(), "markers present");
        assert!(restore_markers("no markers\n", "x\n").is_none());
    }

    #[test]
    fn reworded_line_keeps_its_claim_marker() {
        let user = strip(BASE).replace(
            "Sam Patel works at",
            "Sam Patel is a senior data engineer at",
        ) + "\nMy note on Sam Patel.\n";
        let out = restore_markers(BASE, &user).unwrap();
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
                if l.contains("likes tea") {
                    vec![l, "- Sam Patel plays chess".to_owned()]
                } else {
                    vec![l]
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let out = restore_markers(BASE, &user).unwrap();
        assert!(out.contains(
            "- Sam Patel likes tea. [^2] <!-- c:19 -->\n- Sam Patel plays chess\n<!-- nugget:end facts -->"
        ));
        assert!(
            out.contains("<!-- nugget:begin header -->\n# Sam Patel\n<!-- nugget:end header -->")
        );
    }

    #[test]
    fn comment_helpers() {
        assert_eq!(strip_comments("- a [^1] <!-- c:3 -->"), "- a [^1]");
        assert_eq!(strip_comments("### A <!-- p:a --> x"), "### A x");
        assert_eq!(trailing_comments("- a [^1] <!-- c:3 -->"), "<!-- c:3 -->");
        assert_eq!(
            trailing_comments("- a <!-- x --> <!-- c:3 -->"),
            "<!-- x --> <!-- c:3 -->"
        );
        assert_eq!(trailing_comments("- a"), "");
    }
}
