//! Export formats: Pocket's `ril_export.html`, Netscape bookmark files (Pinboard, Raindrop,
//! browsers; Instapaper's HTML export has the same shape as Pocket's), and CSV exports
//! (Instapaper: `URL,Title,Selection,Folder,Timestamp[,Tags]`; Pocket since 2024:
//! `title,url,time_added,tags,status`; anything else with a `url` column).
//!
//! Exports are untrusted text: the parsers are single-pass, never fail on malformed input (they
//! skip what they cannot read), and cap the number of articles.

use std::collections::BTreeMap;

/// Most articles read from one export file.
pub const MAX_ARTICLES: usize = 200_000;

/// One saved article as an export lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Article {
    pub url: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Pocket list (`Unread`, `Read Archive`), Instapaper folder, or bookmark folder.
    pub folder: Option<String>,
    /// When it was saved, in milliseconds since the Unix epoch.
    pub added_ms: Option<i64>,
    /// Instapaper's highlighted selection or a bookmark's description.
    pub selection: Option<String>,
}

/// Which exporter produced a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Pocket,
    Instapaper,
    Bookmarks,
    Csv,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Pocket => "pocket",
            Source::Instapaper => "instapaper",
            Source::Bookmarks => "bookmarks",
            Source::Csv => "csv",
        }
    }
}

/// Parse an export file, guessing the format from its content.
pub fn parse(text: &str) -> (Source, Vec<Article>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let head = text.trim_start();
    if head.starts_with('<') {
        parse_html(text)
    } else {
        parse_csv(text)
    }
}

// ---- timestamps and tags ----

/// Unix seconds, milliseconds, or an ISO 8601 date.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        if n <= 0 {
            return None;
        }
        // Seconds until the year 33658; anything larger is milliseconds.
        return Some(if n < 1_000_000_000_000 { n * 1000 } else { n });
    }
    parse_iso(s)
}

fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    let (y, mo, d) = (n(0..4)? as i64, n(5..7)?, n(8..10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (mut h, mut mi, mut sec, mut off) = (0, 0, 0, 0i64);
    if b.len() >= 16 && matches!(b[10], b'T' | b't' | b' ') {
        h = n(11..13)?;
        mi = n(14..16)?;
        let mut rest = &s[16..];
        if let Some(r) = rest.strip_prefix(':') {
            sec = r.get(..2)?.parse().ok()?;
            rest = &r[2..];
        }
        if let Some(r) = rest.strip_prefix('.') {
            rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
            let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() != 4 {
                return None;
            }
            let o = digits[..2].parse::<i64>().ok()? * 60 + digits[2..].parse::<i64>().ok()?;
            off = if sign == '-' { -o } else { o };
        }
    }
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + sec as i64 - off * 60) * 1000)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` of a time in milliseconds (UTC).
pub fn format_date(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Tags as exports write them: `a,b`, `a|b`, `a;b` or a JSON array of strings.
pub fn parse_tags(s: &str) -> Vec<String> {
    let s = s.trim();
    let raw: Vec<String> = if s.starts_with('[') {
        match nugget_plugin_sdk::serde_json::from_str::<Vec<String>>(s) {
            Ok(v) => v,
            Err(_) => s
                .trim_matches(['[', ']'])
                .split(',')
                .map(|t| t.trim().trim_matches(['"', '\'']).to_string())
                .collect(),
        }
    } else {
        let sep = if s.contains('|') {
            '|'
        } else if s.contains(';') {
            ';'
        } else {
            ','
        };
        s.split(sep).map(str::to_string).collect()
    };
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().trim_start_matches('#').trim();
        if !t.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(t)) && out.len() < 100 {
            out.push(t.to_string());
        }
    }
    out
}

// ---- HTML ----

/// Decode the HTML character references exports use.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';').filter(|&j| j > 0 && j <= 10);
        let decoded = end.and_then(|j| {
            let name = &after[..j];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &after[j + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A start or end tag with lowercased name and attribute names.
#[derive(Debug)]
struct Tag {
    name: String,
    end: bool,
    attrs: BTreeMap<String, String>,
}

enum Token<'a> {
    Tag(Tag),
    Text(&'a str),
}

/// A forgiving HTML tokenizer: tags, text, comments and doctypes skipped, `<script>` and
/// `<style>` bodies skipped.
fn tokenize(html: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        let Some(lt) = html[i..].find('<').map(|p| p + i) else {
            out.push(Token::Text(&html[i..]));
            break;
        };
        if lt > i {
            out.push(Token::Text(&html[i..lt]));
        }
        let rest = &html[lt..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(b.len(), |e| lt + e + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = rest.find('>').map_or(b.len(), |e| lt + e + 1);
            continue;
        }
        let end = rest.starts_with("</");
        let name_start = lt + if end { 2 } else { 1 };
        let name_len = html[name_start..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(html.len() - name_start);
        if name_len == 0 || !b[name_start].is_ascii_alphabetic() {
            // A stray `<`: text.
            out.push(Token::Text(&html[lt..lt + 1]));
            i = lt + 1;
            continue;
        }
        let name = html[name_start..name_start + name_len].to_ascii_lowercase();
        let (attrs, close) = parse_attrs(html, name_start + name_len);
        i = close;
        if !end && (name == "script" || name == "style") {
            let needle = format!("</{name}");
            i = html[i..]
                .to_ascii_lowercase()
                .find(&needle)
                .map_or(b.len(), |p| i + p);
            continue;
        }
        out.push(Token::Tag(Tag { name, end, attrs }));
    }
    out
}

/// Attributes from `at` up to the closing `>`; returns them and the index after `>`.
fn parse_attrs(html: &str, mut at: usize) -> (BTreeMap<String, String>, usize) {
    let b = html.as_bytes();
    let mut attrs = BTreeMap::new();
    loop {
        while at < b.len() && (b[at].is_ascii_whitespace() || b[at] == b'/') {
            at += 1;
        }
        if at >= b.len() {
            return (attrs, b.len());
        }
        if b[at] == b'>' {
            return (attrs, at + 1);
        }
        let start = at;
        while at < b.len() && !matches!(b[at], b'=' | b'>' | b'/') && !b[at].is_ascii_whitespace() {
            at += 1;
        }
        let name = html[start..at].to_ascii_lowercase();
        while at < b.len() && b[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut value = String::new();
        if at < b.len() && b[at] == b'=' {
            at += 1;
            while at < b.len() && b[at].is_ascii_whitespace() {
                at += 1;
            }
            if at < b.len() && (b[at] == b'"' || b[at] == b'\'') {
                let q = b[at];
                let vs = at + 1;
                let ve = html[vs..].find(q as char).map_or(b.len(), |p| vs + p);
                value = decode_entities(&html[vs..ve]);
                at = (ve + 1).min(b.len());
            } else {
                let vs = at;
                while at < b.len() && !b[at].is_ascii_whitespace() && b[at] != b'>' {
                    at += 1;
                }
                value = decode_entities(&html[vs..at]);
            }
        }
        if !name.is_empty() && attrs.len() < 64 {
            attrs.entry(name).or_insert(value);
        }
        if start == at {
            at += 1;
        }
    }
}

fn clean_text(s: &str) -> String {
    decode_entities(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pocket / Instapaper HTML exports and Netscape bookmark files: every `<a href>` is an article,
/// the latest `<h1>`–`<h3>` heading is its folder, and a `<dd>` right after it is its
/// description.
fn parse_html(html: &str) -> (Source, Vec<Article>) {
    let tokens = tokenize(html);
    let lower_head = html[..html.len().min(512)].to_ascii_lowercase();
    let mut source = if lower_head.contains("netscape-bookmark") {
        Source::Bookmarks
    } else {
        Source::Pocket
    };
    let mut articles: Vec<Article> = Vec::new();
    let mut folder: Option<String> = None;
    let mut collect = Collect::None;
    let mut last_was_anchor = false;
    for tok in tokens {
        if articles.len() >= MAX_ARTICLES {
            break;
        }
        match tok {
            Token::Text(t) => match &mut collect {
                Collect::Heading(s) => s.push_str(t),
                Collect::Title(a) => a.title.push_str(t),
                Collect::Description => {
                    if let Some(a) = articles.last_mut() {
                        a.selection.get_or_insert_with(String::new).push_str(t);
                    }
                }
                Collect::None => {}
            },
            Token::Tag(tag) => {
                let heading = matches!(tag.name.as_str(), "h1" | "h2" | "h3");
                match (tag.name.as_str(), tag.end) {
                    (_, false) if heading => {
                        flush(&mut collect, &mut articles, &mut folder);
                        collect = Collect::Heading(String::new());
                    }
                    (_, true) if heading => flush(&mut collect, &mut articles, &mut folder),
                    ("a", false) => {
                        flush(&mut collect, &mut articles, &mut folder);
                        let Some(href) = tag.attrs.get("href") else {
                            continue;
                        };
                        let tags = tag
                            .attrs
                            .get("tags")
                            .map(|t| parse_tags(t))
                            .unwrap_or_default();
                        let added = tag
                            .attrs
                            .get("time_added")
                            .or_else(|| tag.attrs.get("add_date"))
                            .and_then(|t| parse_time(t));
                        if tag.attrs.contains_key("add_date") && source == Source::Pocket {
                            source = Source::Bookmarks;
                        }
                        collect = Collect::Title(Article {
                            url: href.trim().to_string(),
                            title: String::new(),
                            tags,
                            folder: folder.clone(),
                            added_ms: added,
                            selection: None,
                        });
                    }
                    ("a", true) => {
                        flush(&mut collect, &mut articles, &mut folder);
                        last_was_anchor = true;
                        continue;
                    }
                    ("dd", false) if last_was_anchor => {
                        flush(&mut collect, &mut articles, &mut folder);
                        collect = Collect::Description;
                    }
                    ("dt" | "li" | "dl" | "ul" | "ol", _) => {
                        flush(&mut collect, &mut articles, &mut folder);
                    }
                    _ => {}
                }
                last_was_anchor = false;
            }
        }
    }
    flush(&mut collect, &mut articles, &mut folder);
    for a in &mut articles {
        if let Some(s) = &a.selection {
            let s = clean_text(s);
            a.selection = (!s.is_empty()).then_some(s);
        }
    }
    (source, articles)
}

/// What text the HTML parser is collecting.
enum Collect {
    None,
    Heading(String),
    Title(Article),
    Description,
}

/// Finish what was being collected: a heading becomes the current folder, an anchor an article.
fn flush(collect: &mut Collect, articles: &mut Vec<Article>, folder: &mut Option<String>) {
    match std::mem::replace(collect, Collect::None) {
        Collect::Heading(s) => {
            let s = clean_text(&s);
            *folder = (!s.is_empty()).then_some(s);
        }
        Collect::Title(mut a) => {
            a.title = clean_text(&a.title);
            articles.push(a);
        }
        Collect::Description | Collect::None => {}
    }
}

// ---- CSV ----

/// Split CSV text into records (RFC 4180: quoted fields, `""` escapes, newlines in quotes, CRLF).
pub fn csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut at_field_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                quoted = true;
                at_field_start = false;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(std::mem::take(&mut field));
                if !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
                at_field_start = true;
                if records.len() > MAX_ARTICLES {
                    return records;
                }
            }
            _ => {
                field.push(c);
                at_field_start = false;
            }
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

fn parse_csv(text: &str) -> (Source, Vec<Article>) {
    let mut records = csv_records(text).into_iter();
    let Some(header) = records.next() else {
        return (Source::Csv, Vec::new());
    };
    let header: Vec<String> = header
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    let col = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let Some(url_col) = col(&["url", "link", "href", "address"]) else {
        return (Source::Csv, Vec::new());
    };
    let title_col = col(&["title", "name"]);
    let time_col = col(&[
        "time_added",
        "timestamp",
        "added",
        "created",
        "created_at",
        "saved_at",
        "date",
    ]);
    let tags_col = col(&["tags", "labels", "tag"]);
    let folder_col = col(&["folder", "status", "list", "state"]);
    let selection_col = col(&[
        "selection",
        "excerpt",
        "description",
        "note",
        "notes",
        "highlight",
    ]);
    let source = if col(&["selection"]).is_some() && col(&["folder"]).is_some() {
        Source::Instapaper
    } else if col(&["time_added"]).is_some() {
        Source::Pocket
    } else {
        Source::Csv
    };
    let get = |r: &[String], c: Option<usize>| -> Option<String> {
        c.and_then(|c| r.get(c))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let articles = records
        .take(MAX_ARTICLES)
        .filter_map(|r| {
            let url = get(&r, Some(url_col))?;
            Some(Article {
                title: get(&r, title_col).unwrap_or_default(),
                tags: get(&r, tags_col)
                    .map(|t| parse_tags(&t))
                    .unwrap_or_default(),
                folder: get(&r, folder_col).map(|f| match (source, f.as_str()) {
                    // Pocket's CSV says `unread` / `archive`; name them like its HTML export.
                    (Source::Pocket, "unread") => "Unread".to_string(),
                    (Source::Pocket, "archive") => "Read Archive".to_string(),
                    _ => f,
                }),
                added_ms: get(&r, time_col).and_then(|t| parse_time(&t)),
                selection: get(&r, selection_col),
                url,
            })
        })
        .collect();
    (source, articles)
}
