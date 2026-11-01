//! Markdown: YAML frontmatter, heading sections, `[[wikilinks]]`, Markdown links, bare URLs and
//! `#tags` (outside code, HTML and headings).

use nugget_core::Result;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value, json};

use crate::util::{self, LINK_CAP, SectionTracker, TAG_CAP};
use crate::{Ctx, Extracted, yaml};

/// Frontmatter larger than this is not parsed (`metadata.frontmatter_error` says so).
const MAX_FRONTMATTER: usize = 64 * 1024;
/// Most wikilinks recorded in `metadata.wikilinks`.
const MAX_WIKILINKS: usize = 1_000;

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    if util::looks_binary(data) {
        return Err(nugget_core::Error::Invalid(
            "markdown: binary content".into(),
        ));
    }
    let full = util::decode_text(data, ctx.charset);
    let (fm, body_start) = split_frontmatter(&full);
    let mut meta = util::obj();
    let mut title = None;
    let mut tags = Vec::new();
    if let Some(src) = fm {
        if src.len() > MAX_FRONTMATTER {
            meta.insert("frontmatter_error".into(), "frontmatter too large".into());
        } else {
            match yaml::to_json(src) {
                Ok(Value::Object(m)) => {
                    apply_frontmatter(&m, &mut title, &mut tags, &mut meta);
                    meta.insert("frontmatter".into(), Value::Object(m));
                }
                Ok(Value::Null) => {}
                Ok(other) => {
                    meta.insert("frontmatter".into(), other);
                }
                Err(e) => {
                    meta.insert("frontmatter_error".into(), Value::String(e));
                }
            }
        }
    }
    let text = strip_hidden(&full[body_start..]);
    drop(full);

    let scan = scan(&text);
    if title.is_none() {
        title = scan.first_h1.clone();
    }

    // Links in document order: Markdown links, wikilinks, bare URLs.
    let mut found: Vec<(usize, String)> = scan.md_links.clone();
    let mut wikilinks = Vec::new();
    let mut wl_ranges = Vec::new();
    scan_wikilinks(
        &text,
        &scan.code,
        &mut found,
        &mut wikilinks,
        &mut wl_ranges,
    );
    util::find_urls(&text, |pos, url| {
        if !scan.code.contains(pos) {
            found.push((pos, url.to_owned()));
        }
    });
    found.sort_by_key(|(pos, _)| *pos);
    let mut links = Vec::new();
    for (_, l) in &found {
        util::push_unique(&mut links, l, LINK_CAP);
    }
    if !wikilinks.is_empty() {
        meta.insert("wikilinks".into(), Value::Array(wikilinks));
    }

    let excluded = Ranges::merged(
        scan.code
            .0
            .iter()
            .chain(&scan.headings.0)
            .chain(&wl_ranges)
            .copied()
            .collect(),
    );
    scan_tags(&text, &excluded, &mut tags);

    Ok(Extracted {
        title,
        sections: scan.sections,
        links,
        tags,
        metadata: Value::Object(meta),
        text,
        truncated: false,
    })
}

/// `(frontmatter source, byte offset where the body starts)`.
fn split_frontmatter(s: &str) -> (Option<&str>, usize) {
    let s_trim = s.strip_prefix('\u{feff}').unwrap_or(s);
    let bom = s.len() - s_trim.len();
    let Some(first_nl) = s_trim.find('\n') else {
        return (None, 0);
    };
    if s_trim[..first_nl].trim_end() != "---" {
        return (None, 0);
    }
    let content_start = bom + first_nl + 1;
    let mut pos = content_start;
    for line in s[content_start..].split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return (Some(&s[content_start..pos]), pos + line.len());
        }
        pos += line.len();
    }
    (None, 0)
}

fn strings_of(v: &Value, split: bool) -> Vec<String> {
    match v {
        Value::String(s) if split => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(a) => a.iter().flat_map(|x| strings_of(x, false)).collect(),
        _ => Vec::new(),
    }
}

fn get_ci<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn apply_frontmatter(
    m: &Map<String, Value>,
    title: &mut Option<String>,
    tags: &mut Vec<String>,
    meta: &mut Map<String, Value>,
) {
    if let Some(Value::String(t)) = get_ci(m, "title")
        && !t.trim().is_empty()
    {
        *title = Some(t.trim().to_owned());
    }
    for key in ["tags", "tag"] {
        if let Some(v) = get_ci(m, key) {
            for t in strings_of(v, true) {
                util::push_unique_ci(tags, t.trim_start_matches('#'), TAG_CAP);
            }
        }
    }
    let mut aliases = Vec::new();
    for key in ["aliases", "alias"] {
        if let Some(v) = get_ci(m, key) {
            for a in strings_of(v, false) {
                util::push_unique(&mut aliases, &a, 100);
            }
        }
    }
    if !aliases.is_empty() {
        meta.insert("aliases".into(), json!(aliases));
    }
}

/// Sorted, merged byte ranges.
#[derive(Default)]
struct Ranges(Vec<(usize, usize)>);

impl Ranges {
    fn merged(mut v: Vec<(usize, usize)>) -> Self {
        v.sort_unstable();
        let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
        for (s, e) in v {
            match out.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        Self(out)
    }

    fn contains(&self, p: usize) -> bool {
        self.containing(p).is_some()
    }

    fn containing(&self, p: usize) -> Option<(usize, usize)> {
        let i = self.0.partition_point(|&(s, _)| s <= p);
        (i > 0 && p < self.0[i - 1].1).then(|| self.0[i - 1])
    }
}

/// `text` without what a Markdown renderer never shows: HTML comments (in HTML blocks and inline
/// HTML, never in code) and comment-style link reference definitions (`[//]: # (...)`,
/// `[comment]: <> (...)`: a definition whose destination is `#` or empty).
fn strip_hidden(text: &str) -> String {
    if !text.contains("<!--") && !text.contains("]:") {
        return text.to_owned();
    }
    let mut html = Vec::new();
    let mut blocks = Vec::new();
    let mut parser = Parser::new_ext(text, OPTIONS).into_offset_iter();
    for (ev, range) in parser.by_ref() {
        match ev {
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) | Event::Html(_) => {
                html.push((range.start, range.end));
            }
            _ => {}
        }
        if let Event::Start(_) = ev {
            blocks.push((range.start, range.end));
        }
    }
    let comment_labels: Vec<String> = parser
        .reference_definitions()
        .iter()
        .filter(|(_, d)| matches!(d.dest.trim(), "" | "#"))
        .map(|(label, _)| label.to_lowercase())
        .collect();
    let html = Ranges::merged(html);
    let blocks = Ranges::merged(blocks);

    let mut cut: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while let Some(rel) = text[i..].find("<!--") {
        let s = i + rel;
        i = s + 4;
        let Some((_, range_end)) = html.containing(s) else {
            continue;
        };
        // `<!-->` and `<!--->` are complete (empty) comments; an unclosed one runs to the end of
        // its HTML block.
        let end = text[s + 2..]
            .find("-->")
            .map_or(range_end, |e| s + 2 + e + 3);
        cut.push((s, end));
        i = end;
    }
    if !comment_labels.is_empty() {
        let mut pos = 0;
        for line in text.split_inclusive('\n') {
            let start = pos;
            pos += line.len();
            if blocks.contains(start) {
                continue;
            }
            if let Some(label) = comment_definition_label(line)
                && comment_labels.contains(&label.to_lowercase())
            {
                cut.push((start, pos));
            }
        }
    }
    if cut.is_empty() {
        return text.to_owned();
    }
    let cut = Ranges::merged(cut);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for &(a, b) in &cut.0 {
        out.push_str(&text[at..a]);
        at = b;
    }
    out.push_str(&text[at..]);
    out
}

/// The label of a one-line link reference definition whose destination is `#` or `<>`.
fn comment_definition_label(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = line[indent..].strip_prefix('[')?;
    let close = rest.find("]:")?;
    let label = &rest[..close];
    if label.trim().is_empty() || label.contains(['[', ']']) {
        return None;
    }
    let dest = rest[close + 2..].trim_start();
    let after = dest.strip_prefix("<>").or_else(|| dest.strip_prefix('#'))?;
    after
        .chars()
        .next()
        .is_none_or(char::is_whitespace)
        .then_some(label)
}

struct Scan {
    sections: Vec<crate::Section>,
    first_h1: Option<String>,
    /// Code blocks, inline code, raw HTML, math.
    code: Ranges,
    headings: Ranges,
    md_links: Vec<(usize, String)>,
}

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_MATH);

fn scan(text: &str) -> Scan {
    let opts = OPTIONS;
    let mut tracker = SectionTracker::default();
    let mut first_h1 = None;
    let mut code = Vec::new();
    let mut headings = Vec::new();
    let mut md_links = Vec::new();
    let mut heading: Option<(u32, usize, String)> = None;
    for (ev, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level as u32, range.start, String::new()));
                headings.push((range.start, range.end));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, raw)) = heading.take() {
                    let title = raw.replace("[[", "").replace("]]", "");
                    let title = util::normalize_ws(&title);
                    if level == 1 && first_h1.is_none() && !title.is_empty() {
                        first_h1 = Some(title.clone());
                    }
                    tracker.heading(level, &title, start);
                }
            }
            Event::Text(t) => {
                if let Some(h) = heading.as_mut() {
                    h.2.push_str(&t);
                }
            }
            Event::Code(t) => {
                if let Some(h) = heading.as_mut() {
                    h.2.push_str(&t);
                }
                code.push((range.start, range.end));
            }
            Event::Start(Tag::CodeBlock(_))
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => code.push((range.start, range.end)),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                let url = dest_url.trim();
                if !url.is_empty() && !url.starts_with('#') {
                    md_links.push((range.start, url.to_owned()));
                }
            }
            _ => {}
        }
    }
    Scan {
        sections: tracker.finish(text.len()),
        first_h1,
        code: Ranges::merged(code),
        headings: Ranges::merged(headings),
        md_links,
    }
}

fn scan_wikilinks(
    text: &str,
    code: &Ranges,
    links: &mut Vec<(usize, String)>,
    meta: &mut Vec<Value>,
    ranges: &mut Vec<(usize, usize)>,
) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find("[[") {
        let s = i + rel;
        let inner_start = s + 2;
        i = inner_start;
        if code.contains(s) {
            continue;
        }
        let line_end = text[inner_start..]
            .find('\n')
            .map_or(text.len(), |x| inner_start + x);
        let Some(close) = text[inner_start..line_end].find("]]") else {
            continue;
        };
        let inner = &text[inner_start..inner_start + close];
        if inner.trim().is_empty() || inner.len() > 1024 || inner.contains("[[") {
            continue;
        }
        let embed = s > 0 && bytes[s - 1] == b'!';
        let (target_part, alias) = match inner.split_once('|') {
            Some((t, a)) => (t.trim_end_matches('\\'), Some(a.trim())),
            None => (inner, None),
        };
        let (target, heading) = match target_part.split_once('#') {
            Some((t, h)) => (t.trim(), Some(h.trim())),
            None => (target_part.trim(), None),
        };
        if !target.is_empty() {
            links.push((s, target.to_owned()));
        }
        if meta.len() < MAX_WIKILINKS {
            let mut w = Map::new();
            w.insert("target".into(), target.into());
            if let Some(h) = heading.filter(|h| !h.is_empty()) {
                w.insert("heading".into(), h.into());
            }
            if let Some(a) = alias.filter(|a| !a.is_empty()) {
                w.insert("alias".into(), a.into());
            }
            w.insert("embed".into(), embed.into());
            meta.push(Value::Object(w));
        }
        let end = inner_start + close + 2;
        ranges.push((s - usize::from(embed), end));
        i = end;
    }
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Obsidian-style `#tags`: after start or whitespace, letters/digits/`_`/`-`/`/`, not all digits.
fn scan_tags(text: &str, excluded: &Ranges, tags: &mut Vec<String>) {
    let mut i = 0;
    while let Some(rel) = text[i..].find('#') {
        let p = i + rel;
        i = p + 1;
        let before_ok = text[..p]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if !before_ok || excluded.contains(p) {
            continue;
        }
        let rest = &text[p + 1..];
        let len = rest.find(|c: char| !is_tag_char(c)).unwrap_or(rest.len());
        let tag = rest[..len].trim_end_matches('/');
        i = p + 1 + len;
        if tag.is_empty() || tag.len() > 128 || tag.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if tag.starts_with(['/', '-']) {
            continue;
        }
        util::push_unique_ci(tags, tag, TAG_CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_split() {
        let (fm, at) = split_frontmatter("---\ntitle: x\n---\nbody");
        assert_eq!(fm, Some("title: x\n"));
        assert_eq!(&"---\ntitle: x\n---\nbody"[at..], "body");
        assert_eq!(split_frontmatter("--- \nno close").0, None);
        assert_eq!(split_frontmatter("text\n---\n").0, None);
        assert_eq!(split_frontmatter("---\n---\nb").0, Some(""));
    }

    #[test]
    fn tags_rules() {
        let mut t = Vec::new();
        scan_tags(
            "a #one and #two/sub, x#no #123 #1a #Une-été #ONE",
            &Ranges::default(),
            &mut t,
        );
        assert_eq!(t, ["one", "two/sub", "1a", "Une-été"]);
    }
}
