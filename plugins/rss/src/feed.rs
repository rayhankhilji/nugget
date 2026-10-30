//! Feed parsing: RSS 2.0 (and 0.9x), Atom 1.0 and RSS 1.0 (RDF) into one shape.
//!
//! Feeds are untrusted input. Parsing is bounded (a node limit, roxmltree's entity-expansion
//! checks), never fetches anything (no external entities), and is tolerant of the common
//! mistakes: HTML named entities such as `&nbsp;`, bare `&`, Latin-1 bodies, missing ids.

use nugget_plugin_sdk::{Error, Result};
use roxmltree::{Document, Node, ParsingOptions};

use crate::date;

/// Most XML nodes accepted in one feed.
const MAX_NODES: u32 = 1_000_000;

/// What an entry's content is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Html,
    Text,
}

/// One entry: RSS `<item>` or Atom `<entry>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Stable id: `guid`, Atom `id`, RDF `rdf:about`, else the link, else a hash of the title.
    pub id: String,
    pub title: Option<String>,
    pub link: Option<String>,
    pub content: String,
    pub kind: Kind,
    pub author: Option<String>,
    pub categories: Vec<String>,
    /// The raw date string the feed gave (updated, else published).
    pub updated: Option<String>,
    pub published_ms: Option<i64>,
    pub updated_ms: Option<i64>,
}

impl Entry {
    /// A cheap change token: the feed's updated date when it has one, else a hash of what we
    /// index.
    pub fn version(&self) -> String {
        match &self.updated {
            Some(u) => format!("d:{u}"),
            None => format!(
                "h:{:016x}",
                fnv64(&[
                    self.title.as_deref().unwrap_or(""),
                    self.link.as_deref().unwrap_or(""),
                    &self.content,
                ])
            ),
        }
    }
}

/// A parsed feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub format: &'static str,
    pub title: Option<String>,
    pub link: Option<String>,
    pub entries: Vec<Entry>,
}

/// FNV-1a over `parts`, separated so `["ab", "c"]` and `["a", "bc"]` differ.
pub fn fnv64(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain([0xff]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Decode a feed body: UTF-8 (BOM stripped), or Latin-1 / Windows-1252 when the XML declaration
/// says so. Invalid UTF-8 is replaced rather than refused.
pub fn decode(body: &[u8]) -> String {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    let head = String::from_utf8_lossy(&body[..body.len().min(200)]).to_ascii_lowercase();
    let declared = head
        .find("encoding=")
        .map(|i| &head[i + 9..])
        .map(|r| r.trim_start_matches(['"', '\'']))
        .map(|r| r.split(['"', '\'']).next().unwrap_or(""))
        .unwrap_or("");
    if matches!(
        declared,
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252"
    ) && std::str::from_utf8(body).is_err()
    {
        return body.iter().map(|&b| cp1252(b)).collect();
    }
    String::from_utf8_lossy(body).into_owned()
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Parse a feed. `base` (the feed URL) resolves relative links.
pub fn parse(xml: &str, base: &str) -> Result<Feed> {
    let opts = || ParsingOptions {
        allow_dtd: true,
        nodes_limit: MAX_NODES,
        entity_resolver: None,
    };
    match Document::parse_with_options(xml, opts()) {
        Ok(doc) => from_doc(&doc, base),
        Err(first) => {
            // Second chance for the usual breakage: HTML entities and bare ampersands.
            let fixed = repair_entities(xml);
            let doc = Document::parse_with_options(&fixed, opts())
                .map_err(|_| Error::invalid(format!("feed is not well-formed XML: {first}")))?;
            from_doc(&doc, base)
        }
    }
}

fn from_doc(doc: &Document<'_>, base: &str) -> Result<Feed> {
    let root = doc.root_element();
    match root.tag_name().name() {
        "rss" => {
            let channel = child(root, "channel")
                .ok_or_else(|| Error::invalid("RSS feed without <channel>"))?;
            let items = channel.children().filter(|n| n.has_tag_name("item"));
            Ok(Feed {
                format: "rss",
                title: text(channel, "title"),
                link: text(channel, "link").map(|l| resolve(base, &l)),
                entries: items.map(|i| rss_item(i, base)).collect(),
            })
        }
        "RDF" => {
            let channel = child(root, "channel");
            let items = root.children().filter(|n| n.tag_name().name() == "item");
            Ok(Feed {
                format: "rdf",
                title: channel.and_then(|c| text(c, "title")),
                link: channel
                    .and_then(|c| text(c, "link"))
                    .map(|l| resolve(base, &l)),
                entries: items.map(|i| rss_item(i, base)).collect(),
            })
        }
        "feed" => {
            let base = xml_base(root, base);
            Ok(Feed {
                format: "atom",
                title: atom_text(root, "title").map(|(t, _)| t),
                link: atom_link(root, &base),
                entries: root
                    .children()
                    .filter(|n| n.tag_name().name() == "entry")
                    .map(|e| atom_entry(e, &base, doc))
                    .collect(),
            })
        }
        other => Err(Error::invalid(format!(
            "not a feed: the document's root is <{other}>"
        ))),
    }
}

fn rss_item(item: Node<'_, '_>, base: &str) -> Entry {
    let guid = child(item, "guid");
    let guid_text = guid.and_then(|_| text(item, "guid"));
    let guid_is_link = guid.is_some_and(|g| g.attribute("isPermaLink") != Some("false"));
    let link = text(item, "link")
        .or_else(|| guid_text.clone().filter(|_| guid_is_link))
        .map(|l| resolve(base, &l));
    let about = item
        .attributes()
        .find(|a| a.name() == "about")
        .map(|a| a.value().trim().to_string())
        .filter(|a| !a.is_empty());
    let title = text(item, "title");
    // `content:encoded` (full HTML) beats `description` (often a summary).
    let content = text(item, "encoded")
        .or_else(|| text(item, "description"))
        .unwrap_or_default();
    let pub_date = text(item, "pubDate");
    let dc_date = text(item, "date");
    let updated = text(item, "updated")
        .or_else(|| dc_date.clone())
        .or_else(|| pub_date.clone());
    let published_ms = pub_date
        .as_deref()
        .or(dc_date.as_deref())
        .and_then(date::parse);
    let id = guid_text
        .or(about)
        .or_else(|| link.clone())
        .unwrap_or_else(|| {
            format!(
                "hash:{:016x}",
                fnv64(&[title.as_deref().unwrap_or(""), &content])
            )
        });
    Entry {
        id,
        title,
        link,
        content,
        kind: Kind::Html,
        author: text(item, "creator").or_else(|| text(item, "author")),
        categories: item
            .children()
            .filter(|n| matches!(n.tag_name().name(), "category" | "subject"))
            .filter_map(|n| node_text(n))
            .collect(),
        updated_ms: updated.as_deref().and_then(date::parse),
        updated,
        published_ms,
    }
}

fn atom_entry(entry: Node<'_, '_>, base: &str, doc: &Document<'_>) -> Entry {
    let base = xml_base(entry, base);
    let link = atom_link(entry, &base);
    let title = atom_text(entry, "title").map(|(t, _)| t);
    let (content, kind) = child(entry, "content")
        .filter(|c| c.attribute("src").is_none())
        .and_then(|c| atom_content(c, doc))
        .or_else(|| child(entry, "summary").and_then(|c| atom_content(c, doc)))
        .unwrap_or((String::new(), Kind::Text));
    let updated = text(entry, "updated").or_else(|| text(entry, "published"));
    let id = text(entry, "id")
        .or_else(|| link.clone())
        .unwrap_or_else(|| {
            format!(
                "hash:{:016x}",
                fnv64(&[title.as_deref().unwrap_or(""), &content])
            )
        });
    Entry {
        id,
        title,
        link,
        content,
        kind,
        author: child(entry, "author").and_then(|a| text(a, "name")),
        categories: entry
            .children()
            .filter(|n| n.tag_name().name() == "category")
            .filter_map(|n| {
                n.attribute("label")
                    .or(n.attribute("term"))
                    .map(|t| t.trim().to_string())
            })
            .filter(|t| !t.is_empty())
            .collect(),
        published_ms: text(entry, "published").as_deref().and_then(date::parse),
        updated_ms: updated.as_deref().and_then(date::parse),
        updated,
    }
}

/// Atom text constructs: `type="text"` (default), `"html"` (escaped HTML) or `"xhtml"` (a `div`
/// of inline XHTML, kept as markup).
fn atom_content(node: Node<'_, '_>, doc: &Document<'_>) -> Option<(String, Kind)> {
    match node.attribute("type").unwrap_or("text") {
        "html" | "text/html" => node_text(node).map(|t| (t, Kind::Html)),
        "xhtml" | "application/xhtml+xml" => {
            let div = node.children().find(|n| n.is_element())?;
            let inner: String = div
                .children()
                .map(|c| &doc.input_text()[c.range()])
                .collect();
            let inner = inner.trim();
            (!inner.is_empty()).then(|| (inner.to_string(), Kind::Html))
        }
        t if t == "text" || t.starts_with("text/") => node_text(node).map(|t| (t, Kind::Text)),
        // Base64 or other media types: nothing to index as text.
        _ => None,
    }
}

fn atom_text(node: Node<'_, '_>, name: &str) -> Option<(String, Kind)> {
    let n = child(node, name)?;
    let t = node_text(n)?;
    let kind = match n.attribute("type") {
        Some("html") | Some("xhtml") => Kind::Html,
        _ => Kind::Text,
    };
    Some(match kind {
        Kind::Html => (strip_tags(&t), Kind::Text),
        Kind::Text => (t, Kind::Text),
    })
}

fn atom_link(node: Node<'_, '_>, base: &str) -> Option<String> {
    let links: Vec<_> = node
        .children()
        .filter(|n| n.tag_name().name() == "link")
        .collect();
    links
        .iter()
        .find(|n| n.attribute("rel").is_none_or(|r| r == "alternate"))
        .or_else(|| links.first())
        .and_then(|n| n.attribute("href"))
        .map(|h| resolve(base, h.trim()))
}

fn xml_base(node: Node<'_, '_>, base: &str) -> String {
    match node.attributes().find(|a| {
        a.name() == "base" && a.namespace() == Some("http://www.w3.org/XML/1998/namespace")
    }) {
        Some(a) => resolve(base, a.value().trim()),
        None => base.to_string(),
    }
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.tag_name().name() == name)
}

/// The trimmed text of a node (all descendant text, CDATA included), or `None` when empty.
fn node_text(node: Node<'_, '_>) -> Option<String> {
    let t: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn text(node: Node<'_, '_>, name: &str) -> Option<String> {
    node_text(child(node, name)?)
}

/// Resolve `href` against `base`: absolute URLs as-is, `//host/x`, `/path`, and relative paths.
pub fn resolve(base: &str, href: &str) -> String {
    if href.contains("://") || href.starts_with("mailto:") {
        return href.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return href.to_string();
    };
    if let Some(h) = href.strip_prefix("//") {
        return format!("{scheme}://{h}");
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..authority_end]);
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    if href.is_empty() || href.starts_with('#') || href.starts_with('?') {
        let cut = base.find(['#', '?']).unwrap_or(base.len());
        return format!("{}{href}", &base[..cut]);
    }
    let path = &rest[authority_end..];
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{origin}{dir}{href}")
}

/// Remove HTML tags (for titles given as HTML). Good enough for display text; not a sanitizer.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Turn HTML named entities into numeric references and bare `&` into `&amp;`, keeping the five
/// XML entities and numeric references.
pub fn repair_entities(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() + 64);
    let mut rest = xml;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after
            .char_indices()
            .take(32)
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '#'))
            .filter(|(_, c)| *c == ';')
            .map(|(j, _)| j);
        match end {
            Some(j) if j > 0 => {
                let name = &after[..j];
                if name.starts_with('#') || matches!(name, "amp" | "lt" | "gt" | "quot" | "apos") {
                    out.push('&');
                    out.push_str(name);
                    out.push(';');
                } else if let Some(cp) = html_entity(name) {
                    out.push_str(&format!("&#{cp};"));
                } else {
                    out.push_str("&amp;");
                    out.push_str(name);
                    out.push(';');
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push_str("&amp;");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The HTML named entities that show up in feeds.
fn html_entity(name: &str) -> Option<u32> {
    Some(match name {
        "nbsp" => 160,
        "iexcl" => 161,
        "cent" => 162,
        "pound" => 163,
        "euro" => 8364,
        "yen" => 165,
        "sect" => 167,
        "copy" => 169,
        "laquo" => 171,
        "raquo" => 187,
        "reg" => 174,
        "deg" => 176,
        "plusmn" => 177,
        "middot" => 183,
        "frac12" => 189,
        "iquest" => 191,
        "times" => 215,
        "divide" => 247,
        "ndash" => 8211,
        "mdash" => 8212,
        "lsquo" => 8216,
        "rsquo" => 8217,
        "sbquo" => 8218,
        "ldquo" => 8220,
        "rdquo" => 8221,
        "bdquo" => 8222,
        "bull" => 8226,
        "hellip" => 8230,
        "prime" => 8242,
        "trade" => 8482,
        "larr" => 8592,
        "rarr" => 8594,
        "auml" => 228,
        "ouml" => 246,
        "uuml" => 252,
        "Auml" => 196,
        "Ouml" => 214,
        "Uuml" => 220,
        "szlig" => 223,
        "eacute" => 233,
        "egrave" => 232,
        "Eacute" => 201,
        "aacute" => 225,
        "agrave" => 224,
        "ccedil" => 231,
        "ntilde" => 241,
        "oacute" => 243,
        "iacute" => 237,
        "uacute" => 250,
        _ => return None,
    })
}
