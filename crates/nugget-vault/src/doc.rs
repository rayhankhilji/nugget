//! Page structure: YAML frontmatter, generated regions and user text.
//!
//! A page is parsed into an optional frontmatter block and a sequence of segments. A generated
//! region is the text between `<!-- nugget:begin NAME -->` and `<!-- nugget:end NAME -->` (HTML
//! comments, invisible in Obsidian's reading view). Everything else is user text.

use nugget_graph::ClaimId;

pub const BEGIN: &str = "<!-- nugget:begin ";
pub const END: &str = "<!-- nugget:end ";
const CLOSE: &str = " -->";
/// Per-fact-line claim marker prefix: `<!-- c:123 -->`.
pub const CLAIM_MARK: &str = "<!-- c:";
/// Predicate-group heading marker prefix: `### Works at <!-- p:works_at -->`.
pub const PRED_MARK: &str = "<!-- p:";
/// History-line claim marker prefix: `  - Worked at Acme (until 2026-10-05) [^2] <!-- h:7 -->`.
/// A line carrying it renders a superseded claim; it is never an editable fact.
pub const HIST_MARK: &str = "<!-- h:";
/// The line that introduces the history sub-list of a predicate group.
pub const HISTORY_INTRO: &str = "*Earlier:*";

/// Region names, in page order.
pub const R_HEADER: &str = "header";
pub const R_FACTS: &str = "facts";
pub const R_RELATED: &str = "related";
/// Why other entities were merged into this one (only on pages with a merge log).
pub const R_MERGED: &str = "merged";
pub const R_SOURCES: &str = "sources";
pub const R_REDIRECT: &str = "redirect";
pub const R_INDEX: &str = "index";
/// Name used for the frontmatter in conflict lists.
pub const R_FRONTMATTER: &str = "frontmatter";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    User(Vec<String>),
    Region { name: String, body: Vec<String> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doc {
    /// Lines between the `---` fences, if the page has frontmatter.
    pub fm: Option<Vec<String>>,
    pub segs: Vec<Segment>,
}

pub fn begin_line(name: &str) -> String {
    format!("{BEGIN}{name}{CLOSE}")
}

pub fn end_line(name: &str) -> String {
    format!("{END}{name}{CLOSE}")
}

fn marker_name<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line
        .trim()
        .strip_prefix(prefix)?
        .strip_suffix(CLOSE)?
        .trim();
    (!rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then_some(rest)
}

impl Doc {
    pub fn parse(text: &str) -> Doc {
        let text = text.replace("\r\n", "\n");
        let mut lines: Vec<&str> = text.split('\n').collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        let n = lines.len();
        let mut i = 0;
        let mut fm = None;
        if lines.first().is_some_and(|l| l.trim_end() == "---")
            && let Some(j) = (1..n).find(|&j| matches!(lines[j].trim_end(), "---" | "..."))
        {
            fm = Some(lines[1..j].iter().map(|s| s.to_string()).collect());
            i = j + 1;
        }
        let mut segs = Vec::new();
        let mut user: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        while i < n {
            if let Some(name) = marker_name(lines[i], BEGIN)
                && !seen.iter().any(|s| s == name)
                && let Some(j) = (i + 1..n).find(|&j| marker_name(lines[j], END) == Some(name))
            {
                if !user.is_empty() {
                    segs.push(Segment::User(std::mem::take(&mut user)));
                }
                seen.push(name.to_owned());
                segs.push(Segment::Region {
                    name: name.to_owned(),
                    body: lines[i + 1..j].iter().map(|s| s.to_string()).collect(),
                });
                i = j + 1;
                continue;
            }
            user.push(lines[i].to_owned());
            i += 1;
        }
        if !user.is_empty() {
            segs.push(Segment::User(user));
        }
        Doc { fm, segs }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(fm) = &self.fm {
            out.push_str("---\n");
            for l in fm {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str("---\n");
        }
        for s in &self.segs {
            match s {
                Segment::User(lines) => {
                    for l in lines {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                Segment::Region { name, body } => {
                    out.push_str(&begin_line(name));
                    out.push('\n');
                    for l in body {
                        out.push_str(l);
                        out.push('\n');
                    }
                    out.push_str(&end_line(name));
                    out.push('\n');
                }
            }
        }
        out
    }

    pub fn region(&self, name: &str) -> Option<&[String]> {
        self.segs.iter().find_map(|s| match s {
            Segment::Region { name: n, body } if n == name => Some(body.as_slice()),
            _ => None,
        })
    }

    /// Region names in order.
    pub fn region_names(&self) -> Vec<&str> {
        self.segs
            .iter()
            .filter_map(|s| match s {
                Segment::Region { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Everything the user wrote: text outside generated regions plus the body of any generated
    /// region the user changed relative to `base` (without a base, only the text outside).
    pub fn user_content(&self, base: Option<&Doc>) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.segs {
            match s {
                Segment::User(lines) => out.extend(lines.iter().cloned()),
                Segment::Region { name, body } => {
                    let unchanged = base.and_then(|b| b.region(name)) == Some(body.as_slice());
                    if !unchanged && base.is_some() {
                        out.extend(body.iter().cloned());
                    }
                }
            }
        }
        out
    }
}

// ---- frontmatter ----

/// Frontmatter split into top-level keys. Lines before the first key are kept under `""`.
pub fn fm_entries(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for l in lines {
        let top = !l.starts_with([' ', '\t', '-', '#']) && !l.trim().is_empty();
        let key = top
            .then(|| l.split_once(':'))
            .flatten()
            .map(|(k, _)| k.trim());
        match key {
            Some(k) if !k.is_empty() => out.push((k.to_owned(), vec![l.clone()])),
            _ => match out.last_mut() {
                Some((_, v)) => v.push(l.clone()),
                None => out.push((String::new(), vec![l.clone()])),
            },
        }
    }
    out
}

/// The value text of a frontmatter entry (after `key:`), continuation lines included.
pub fn fm_value(entry: &[String]) -> String {
    let mut s = entry
        .first()
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_owned())
        .unwrap_or_default();
    for l in entry.iter().skip(1) {
        s.push('\n');
        s.push_str(l);
    }
    s
}

pub fn fm_get(lines: &[String], key: &str) -> Option<String> {
    fm_entries(lines)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| fm_value(&v))
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('"')
        && let Ok(v) = serde_json::from_str::<String>(t)
    {
        return v;
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return t[1..t.len() - 1].replace("''", "'");
    }
    t.to_owned()
}

/// A scalar frontmatter value: JSON/YAML double-quoted, single-quoted or plain.
pub fn fm_scalar(value: &str) -> Option<String> {
    let v = value.trim();
    (!v.is_empty() && !v.contains('\n')).then(|| unquote(v))
}

/// A list of strings, in flow (`["a", "b"]`) or block (`- a`) style.
pub fn fm_list(value: &str) -> Option<Vec<String>> {
    let v = value.trim();
    if v.is_empty() {
        return Some(Vec::new());
    }
    if v.starts_with('[') {
        return serde_json::from_str::<Vec<String>>(v).ok().or_else(|| {
            let inner = v.strip_prefix('[')?.strip_suffix(']')?;
            Some(
                inner
                    .split(',')
                    .map(unquote)
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        });
    }
    let mut out = Vec::new();
    for l in v.lines() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        out.push(unquote(t.strip_prefix('-')?.trim()));
    }
    Some(out)
}

// ---- fact lines ----

/// A top-level Markdown bullet (`- `, `* `, `+ `), returning the text after the bullet.
pub fn bullet_body(line: &str) -> Option<&str> {
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(b) {
            return Some(rest);
        }
    }
    matches!(line, "-" | "*" | "+").then_some("")
}

/// Split a fact line into its claim id (from the trailing `<!-- c:N -->` marker) and its content
/// with the marker, footnote references and the `(you)` tag removed. `None` if not a bullet.
pub fn parse_fact(line: &str) -> Option<(Option<ClaimId>, String)> {
    let body = bullet_body(line)?;
    let mut rest = body.trim_end();
    let mut claim = None;
    if rest.ends_with("-->")
        && let Some(at) = rest.rfind(CLAIM_MARK)
    {
        let inner = rest[at + CLAIM_MARK.len()..rest.len() - 3].trim();
        if let Ok(id) = inner.parse::<ClaimId>() {
            claim = Some(id);
            rest = rest[..at].trim_end();
        }
    }
    loop {
        let before = rest.len();
        if let Some(r) = rest.strip_suffix("(you)") {
            rest = r.trim_end();
        }
        if rest.ends_with(']')
            && let Some(at) = rest.rfind("[^")
            && !rest[at + 2..rest.len() - 1].contains(['[', ']', ' '])
        {
            rest = rest[..at].trim_end();
        }
        if rest.len() == before {
            break;
        }
    }
    Some((claim, collapse_ws(rest)))
}

/// A history line: a bullet (at any indentation) ending in a `<!-- h:N -->` marker. Returns the
/// claim id; `None` for any other line (including a history marker whose id does not parse).
pub fn parse_history(line: &str) -> Option<ClaimId> {
    let t = line.trim();
    bullet_body(t)?;
    let inner = t.strip_suffix("-->")?;
    let at = inner.rfind(HIST_MARK)?;
    inner[at + HIST_MARK.len()..].trim().parse().ok()
}

/// Whether a line carries a claim or history marker that does not parse (a broken marker).
pub fn has_bad_marker(line: &str) -> bool {
    let t = line.trim_end();
    for mark in [CLAIM_MARK, HIST_MARK] {
        if let Some(at) = t.rfind(mark) {
            let ok = t[at + mark.len()..]
                .strip_suffix("-->")
                .is_some_and(|id| id.trim().parse::<ClaimId>().is_ok());
            if !ok || bullet_body(t.trim_start()).is_none() {
                return true;
            }
        }
    }
    false
}

/// Footnote labels referenced on a line (`[^1]` → `1`), excluding definitions.
pub fn footnote_refs(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = line;
    let mut line_start = true;
    while let Some(at) = s.find("[^") {
        let rest = &s[at + 2..];
        let is_start = line_start && at == 0;
        line_start = false;
        match rest.find(']') {
            Some(end) if end > 0 && !rest[..end].contains(['[', ' ']) => {
                let after = &rest[end + 1..];
                if !(is_start && after.starts_with(':')) {
                    out.push(rest[..end].to_owned());
                }
                s = after;
            }
            _ => s = rest,
        }
    }
    out
}

/// A predicate-group heading (`### Title <!-- p:pred -->`): `(title, explicit predicate)`.
pub fn parse_group_heading(line: &str) -> Option<(String, Option<String>)> {
    let rest = line.strip_prefix("### ")?.trim_end();
    if rest.ends_with("-->")
        && let Some(at) = rest.rfind(PRED_MARK)
    {
        let pred = rest[at + PRED_MARK.len()..rest.len() - 3].trim();
        if !pred.is_empty() {
            return Some((rest[..at].trim().to_owned(), Some(pred.to_owned())));
        }
    }
    Some((rest.trim().to_owned(), None))
}

pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Works at` → `works_at`.
pub fn snake(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() { "note".into() } else { out }
}

/// `works_at` → `Works at`.
pub fn humanize(pred: &str) -> String {
    let words = pred.replace(['_', '-'], " ");
    let words = collapse_ws(&words);
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Note".into(),
    }
}

/// `[[target|display]]` links on a line: `(target, display)`.
pub fn wikilinks(line: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut s = line;
    while let Some(at) = s.find("[[") {
        let rest = &s[at + 2..];
        let Some(end) = rest.find("]]") else { break };
        let inner = &rest[..end];
        if !inner.contains('\n') && !inner.is_empty() {
            let (t, d) = match inner.split_once('|') {
                Some((t, d)) => (t, Some(d.to_owned())),
                None => (inner, None),
            };
            out.push((t.trim().to_owned(), d));
        }
        s = &rest[end + 2..];
    }
    out
}

/// Link target without a heading or block anchor (`a#b` → `a`).
pub fn link_page(target: &str) -> &str {
    target.split(['#', '^']).next().unwrap_or("").trim()
}

/// Replace wikilinks by their display text (or the target's file name without the id suffix).
pub fn strip_links(line: &str) -> String {
    let mut out = String::new();
    let mut s = line;
    while let Some(at) = s.find("[[") {
        let rest = &s[at + 2..];
        let Some(end) = rest.find("]]") else { break };
        out.push_str(&s[..at]);
        let inner = &rest[..end];
        let shown = match inner.split_once('|') {
            Some((_, d)) => d.to_owned(),
            None => {
                let base = link_page(inner).rsplit('/').next().unwrap_or(inner);
                match base.rfind(" (") {
                    Some(p) if base.ends_with(')') => base[..p].to_owned(),
                    _ => base.to_owned(),
                }
            }
        };
        out.push_str(&shown);
        s = &rest[end + 2..];
    }
    out.push_str(s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trip() {
        let text = "---\nnugget_id: \"e\"\n---\n\n<!-- nugget:begin facts -->\n- a <!-- c:1 -->\n<!-- nugget:end facts -->\n\n## Notes\nmine\n";
        let d = Doc::parse(text);
        assert_eq!(d.fm.as_deref(), Some(&["nugget_id: \"e\"".to_string()][..]));
        assert_eq!(
            d.region("facts").unwrap(),
            &["- a <!-- c:1 -->".to_string()]
        );
        assert_eq!(d.render(), text);
        assert_eq!(Doc::parse(&text.replace('\n', "\r\n")).render(), text);
    }

    #[test]
    fn unclosed_and_duplicate_regions_are_user_text() {
        let d = Doc::parse("<!-- nugget:begin a -->\nx\n");
        assert!(d.region("a").is_none());
        let d = Doc::parse(
            "<!-- nugget:begin a -->\nx\n<!-- nugget:end a -->\n<!-- nugget:begin a -->\ny\n<!-- nugget:end a -->\n",
        );
        assert_eq!(d.region_names(), vec!["a"]);
        assert_eq!(d.user_content(None).len(), 3);
    }

    #[test]
    fn fact_lines() {
        assert_eq!(
            parse_fact("- Sam works at Acme. [^1][^2] <!-- c:12 -->"),
            Some((Some(12), "Sam works at Acme.".into()))
        );
        assert_eq!(
            parse_fact("- Likes tea. (you) <!-- c:3 -->"),
            Some((Some(3), "Likes tea.".into()))
        );
        assert_eq!(
            parse_fact("-   new  fact "),
            Some((None, "new fact".into()))
        );
        assert_eq!(parse_fact("  - nested"), None);
        assert_eq!(
            parse_history("  - Was at Acme (until 2026-10-05) [^2] <!-- h:7 -->"),
            Some(7)
        );
        assert_eq!(parse_history("- de-indented [^2] <!-- h:7 -->"), Some(7));
        assert_eq!(parse_history("- fact [^1] <!-- c:7 -->"), None);
        assert_eq!(parse_history("  - x <!-- h:abc -->"), None);
        assert!(has_bad_marker("  - x <!-- h:abc -->"));
        assert!(has_bad_marker("- x <!-- c: -->"));
        assert!(!has_bad_marker("- x [^1] <!-- c:3 -->"));
        assert!(!has_bad_marker("  - x [^1] <!-- h:3 -->"));
        assert!(!has_bad_marker("plain text"));
        assert_eq!(parse_fact("text"), None);
        assert_eq!(footnote_refs("a [^1][^x2] b"), vec!["1", "x2"]);
        assert!(footnote_refs("[^1]: def").is_empty());
    }

    #[test]
    fn frontmatter_values() {
        let lines: Vec<String> = [
            "a: \"x\"",
            "aliases:",
            "  - one",
            "  - 'two'",
            "tags: [a, b]",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            fm_get(&lines, "a").and_then(|v| fm_scalar(&v)),
            Some("x".into())
        );
        assert_eq!(
            fm_list(&fm_get(&lines, "aliases").unwrap()),
            Some(vec!["one".into(), "two".into()])
        );
        assert_eq!(
            fm_list(&fm_get(&lines, "tags").unwrap()),
            Some(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn links() {
        let l = "see [[Person/Sam (e1)|Sam]] and [[Topic/Tea (e2)#x]]";
        assert_eq!(
            wikilinks(l),
            vec![
                ("Person/Sam (e1)".into(), Some("Sam".into())),
                ("Topic/Tea (e2)#x".into(), None)
            ]
        );
        assert_eq!(strip_links(l), "see Sam and Tea");
        assert_eq!(snake("Works at!"), "works_at");
        assert_eq!(humanize("works_at"), "Works at");
    }
}
