//! Parsing one Obsidian note: YAML front matter (a small, tolerant subset), `[[wikilinks]]`,
//! `![[embeds]]`, relative Markdown links to notes, and `#tags`.
//!
//! Notes are untrusted text. Everything here is linear in the input and allocates at most a few
//! copies of it; lists are capped.

use std::collections::BTreeMap;

/// Most links, embeds, tags or aliases kept per note.
const MAX_ITEMS: usize = 500;
/// Most front-matter properties kept as metadata, and the longest value kept.
const MAX_PROPERTIES: usize = 50;
const MAX_PROPERTY_CHARS: usize = 500;

/// A parsed note.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Note {
    /// Front matter `title`, when present.
    pub title: Option<String>,
    pub aliases: Vec<String>,
    /// Front matter tags and inline `#tags`, without `#`, deduplicated ignoring case.
    pub tags: Vec<String>,
    /// Targets of `[[wikilinks]]` and relative `[text](note.md)` links, without headings or
    /// aliases, deduplicated.
    pub links: Vec<String>,
    /// Targets of `![[embeds]]`.
    pub embeds: Vec<String>,
    /// Other front matter fields (scalars as strings, lists joined with `, `).
    pub properties: BTreeMap<String, String>,
    /// The note without its front matter.
    pub body: String,
}

/// A front matter value.
#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Str(String),
    List(Vec<String>),
}

impl Yaml {
    fn items(&self) -> Vec<String> {
        match self {
            Yaml::List(l) => l.clone(),
            Yaml::Str(s) => s
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    fn as_text(&self) -> String {
        match self {
            Yaml::Str(s) => s.clone(),
            Yaml::List(l) => l.join(", "),
        }
    }
}

/// Parse a whole note.
pub fn parse(text: &str) -> Note {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (front, body) = split_front_matter(text);
    let mut note = Note {
        body: body.to_string(),
        ..Note::default()
    };
    let mut tags = Dedup::default();
    if let Some(front) = front {
        for (key, value) in parse_yaml(front) {
            match key.to_ascii_lowercase().as_str() {
                "title" => note.title = Some(value.as_text()).filter(|t| !t.is_empty()),
                "tags" | "tag" => {
                    for t in value.items() {
                        tags.push(t.trim_start_matches('#'));
                    }
                }
                "aliases" | "alias" => {
                    note.aliases = match value {
                        // A single alias may contain commas or spaces.
                        Yaml::Str(s) if !s.is_empty() => vec![s],
                        v => v.items(),
                    };
                    note.aliases.truncate(MAX_ITEMS);
                }
                _ if note.properties.len() < MAX_PROPERTIES => {
                    let v: String = value.as_text().chars().take(MAX_PROPERTY_CHARS).collect();
                    note.properties.insert(key, v);
                }
                _ => {}
            }
        }
    }
    let mut links = Dedup::default();
    let mut embeds = Dedup::default();
    scan_body(body, &mut links, &mut embeds, &mut tags);
    note.tags = tags.0;
    note.links = links.0;
    note.embeds = embeds.0;
    note
}

/// Keeps first occurrences, compared ignoring case, up to [`MAX_ITEMS`].
#[derive(Default)]
struct Dedup(Vec<String>);

impl Dedup {
    fn push(&mut self, s: &str) {
        let s = s.trim();
        if s.is_empty()
            || self.0.len() >= MAX_ITEMS
            || self.0.iter().any(|x| x.to_lowercase() == s.to_lowercase())
        {
            return;
        }
        self.0.push(s.to_string());
    }
}

/// `(front matter, body)`. Front matter is a `---` line at the very start, closed by `---` or
/// `...` on a line of its own.
pub fn split_front_matter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (None, text);
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        let l = line.trim_end_matches(['\n', '\r']);
        if l == "---" || l == "..." {
            return (Some(&rest[..at]), &rest[at + line.len()..]);
        }
        at += line.len();
    }
    // Never closed: not front matter.
    (None, text)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        let inner = &s[1..s.len() - 1];
        if b[0] == b'"' {
            inner.replace("\\\"", "\"").replace("\\\\", "\\")
        } else {
            inner.replace("''", "'")
        }
    } else {
        s.to_string()
    }
}

/// Split an inline list body (`a, "b, c", d`) on commas outside quotes.
fn split_inline_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    for c in s.chars() {
        match (c, quote) {
            ('"' | '\'', None) => {
                quote = Some(c);
                cur.push(c);
            }
            (c, Some(q)) if c == q => {
                quote = None;
                cur.push(c);
            }
            (',', None) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.iter()
        .map(|s| unquote(s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A tolerant reader for the YAML Obsidian writes: top-level `key: value` pairs whose values are
/// scalars, `[inline, lists]`, block lists (`- item`) or block scalars (`|`, `>`). Nested maps
/// and anything else are skipped. Keys keep their first occurrence.
fn parse_yaml(src: &str) -> Vec<(String, Yaml)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<(String, Yaml)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with([' ', '\t', '-'])
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = unquote(key);
        let value = value.trim();
        if key.is_empty() || out.iter().any(|(k, _)| *k == key) {
            continue;
        }
        // Indented lines that follow belong to this key.
        let start = i;
        while i < lines.len()
            && (lines[i].starts_with([' ', '\t', '-']) || lines[i].trim().is_empty())
        {
            i += 1;
        }
        let block = &lines[start..i];
        let parsed = if value.is_empty() {
            let items: Vec<String> = block
                .iter()
                .filter_map(|l| l.trim().strip_prefix('-'))
                .map(unquote)
                .filter(|s| !s.is_empty())
                .collect();
            Yaml::List(items)
        } else if value.starts_with('[') && value.ends_with(']') {
            Yaml::List(split_inline_list(&value[1..value.len() - 1]))
        } else if value.starts_with('|') || value.starts_with('>') {
            let sep = if value.starts_with('|') { "\n" } else { " " };
            Yaml::Str(
                block
                    .iter()
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join(sep)
                    .trim()
                    .to_string(),
            )
        } else {
            Yaml::Str(unquote(value))
        };
        out.push((key, parsed));
    }
    out
}

/// Scan Markdown outside code for links, embeds and tags.
fn scan_body(body: &str, links: &mut Dedup, embeds: &mut Dedup, tags: &mut Dedup) {
    let mut fence: Option<&str> = None;
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if t.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        // Indented code blocks (4 spaces or a tab) are left alone too.
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        scan_line(line, links, embeds, tags);
    }
}

fn scan_line(line: &str, links: &mut Dedup, embeds: &mut Dedup, tags: &mut Dedup) {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < chars.len() {
        let (pos, c) = chars[i];
        let rest = &line[pos..];
        // Inline code: skip to the closing run of backticks.
        if c == '`' {
            let run = rest.chars().take_while(|&c| c == '`').count();
            let ticks = &rest[..run];
            match rest[run..].find(ticks) {
                Some(end) => {
                    let skip_to = pos + run + end + run;
                    while i < chars.len() && chars[i].0 < skip_to {
                        i += 1;
                    }
                    prev = Some('`');
                    continue;
                }
                None => {
                    i += run;
                    prev = Some('`');
                    continue;
                }
            }
        }
        if rest.starts_with("[[")
            && let Some(end) = rest[2..].find("]]")
        {
            let inner = &rest[2..2 + end];
            let target = inner.split('|').next().unwrap_or("");
            let target = target.split(['#', '^']).next().unwrap_or("").trim();
            if !target.is_empty() && !inner.contains("[[") {
                if prev == Some('!') {
                    embeds.push(target);
                } else {
                    links.push(target);
                }
            }
            let skip_to = pos + 2 + end + 2;
            while i < chars.len() && chars[i].0 < skip_to {
                i += 1;
            }
            prev = Some(']');
            continue;
        }
        if c == ']'
            && rest.starts_with("](")
            && let Some(end) = rest[2..].find(')')
        {
            let href = rest[2..2 + end].trim();
            let href = href.split_whitespace().next().unwrap_or("");
            let href = href.trim_start_matches('<').trim_end_matches('>');
            let path = href.split('#').next().unwrap_or("");
            if !path.contains("://") && path.to_ascii_lowercase().ends_with(".md") {
                let decoded = percent_decode(path);
                let target = decoded.trim_start_matches("./");
                let target = &target[..target.len() - 3];
                links.push(target);
            }
        }
        if c == '#' && prev.is_none_or(|p| p.is_whitespace() || p == '(' || p == ',') {
            let tag: String = rest[1..]
                .chars()
                .take_while(|&c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                .collect();
            let tag = tag.trim_end_matches('/');
            if !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) {
                tags.push(tag);
            }
        }
        prev = Some(c);
        i += 1;
    }
}

/// Decode `%XX` escapes (invalid ones are kept as-is).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Percent-encode everything but unreserved characters (for `obsidian://` URLs).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
