//! Nugget plugin: saved articles from a read-later service's export.
//!
//! Settings: `{ "export": "/path/to/ril_export.html", "skip_archived": false }`. `export` is the
//! manifest's only file root: a single export file or a folder of them (searched for `.html`,
//! `.htm` and `.csv` files, dot-folders skipped). Supported: Pocket's `ril_export.html` and its
//! newer CSV export, Instapaper's CSV export (and HTML export), and Netscape bookmark files
//! (Pinboard, Raindrop, browsers). See [`formats`].
//!
//! Each saved article becomes one `text/markdown` document keyed by its normalised URL (scheme
//! and host lowercased, default port, fragment and tracking parameters dropped), so the same
//! article saved twice, or present in two exports, is one document; its tags are merged and the
//! earliest save time wins. The content is a short card (title, URL, date, folder, tags, the
//! highlighted selection); nothing is fetched from the network.
//!
//! The cursor remembers the export files' sizes and modification times; a pass whose files are
//! unchanged does nothing. Otherwise the whole export is read again, articles whose version
//! changed are sent, and articles no longer in the export are deleted (only when every export
//! file could be read).

pub mod formats;

use std::collections::{BTreeMap, BTreeSet};

use formats::{Article, Source};
use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Value, export_plugin, fs, json, log, sink,
};

/// The file root (settings field) holding the export path.
pub const ROOT: &str = "export";
/// Deepest folder nesting searched for export files, and most files read.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 1000;

pub struct ReadLater;

/// Parsed source settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub export: String,
    pub skip_archived: bool,
}

impl Settings {
    pub fn parse(v: &Value) -> Result<Self> {
        let export = v[ROOT]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::invalid("settings.export (the export file or folder) is required")
            })?;
        if export.trim() != export {
            return Err(Error::invalid(
                "settings.export has spaces around the path; remove them",
            ));
        }
        let b = export.as_bytes();
        let absolute = export.starts_with('/')
            || export.starts_with("\\\\")
            || (b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && matches!(b[2], b'\\' | b'/'));
        if !absolute {
            return Err(Error::invalid("settings.export must be an absolute path"));
        }
        let skip_archived = match &v["skip_archived"] {
            Value::Null => false,
            Value::Bool(b) => *b,
            _ => {
                return Err(Error::invalid(
                    "settings.skip_archived must be true or false",
                ));
            }
        };
        Ok(Self {
            export: export.to_string(),
            skip_archived,
        })
    }
}

/// Normalise a URL into a document key, or `None` when it is not an `http(s)` URL.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let rest = &rest[..rest.find('#').unwrap_or(rest.len())];
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = rest[..auth_end]
        .rsplit('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let authority = authority
        .strip_suffix(if scheme == "https" { ":443" } else { ":80" })
        .unwrap_or(&authority)
        .trim_end_matches('.')
        .to_string();
    if authority.is_empty() || authority.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let path_query = &rest[auth_end..];
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_query, None),
    };
    let path = if path.is_empty() { "/" } else { path };
    let params: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split('=').next().unwrap_or("").to_ascii_lowercase();
            !(name.starts_with("utm_")
                || matches!(
                    name.as_str(),
                    "fbclid"
                        | "gclid"
                        | "dclid"
                        | "msclkid"
                        | "mc_cid"
                        | "mc_eid"
                        | "igshid"
                        | "_hsenc"
                        | "_hsmi"
                        | "mkt_tok"
                        | "yclid"
                ))
        })
        .collect();
    let mut out = format!("{scheme}://{authority}{path}");
    if !params.is_empty() {
        out.push('?');
        out.push_str(&params.join("&"));
    }
    Some(out)
}

/// FNV-1a 64 over `parts`, each followed by a separator byte.
fn fnv64(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain([0xff]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The document key for a normalised URL (hashed when it would exceed 1024 bytes).
pub fn key_for(normalized: &str) -> String {
    if normalized.len() <= 1024 {
        normalized.to_string()
    } else {
        format!("url-hash:{:016x}", fnv64(&[normalized]))
    }
}

/// An export file found under the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFile {
    /// Relative to the root (`""` when the root is the file itself).
    pub path: String,
    pub size: u64,
    pub modified_at: Option<i64>,
}

fn is_export_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".html") || l.ends_with(".htm") || l.ends_with(".csv")
}

/// The export files under the root, in path order, and whether that is all of them (no
/// subfolder failed to list, none was too deep, the file cap was not reached). Only a complete
/// list may lead to deletes.
pub fn find_exports() -> Result<(Vec<ExportFile>, bool)> {
    let root = fs::stat(ROOT, "")?;
    if !root.is_dir {
        let file = ExportFile {
            path: String::new(),
            size: root.size,
            modified_at: root.modified_at,
        };
        return Ok((vec![file], true));
    }
    let mut out = Vec::new();
    let mut complete = true;
    let mut stack = vec![(String::new(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match fs::read_dir(ROOT, &dir) {
            Ok(e) => e,
            Err(e) if dir.is_empty() => return Err(e),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                log::warn(&format!(
                    "cannot list `{dir}`: {e}; nothing is deleted this pass"
                ));
                complete = false;
                continue;
            }
        };
        for e in entries {
            if e.name.starts_with('.') {
                continue;
            }
            let path = fs::join(&dir, &e.name);
            if e.is_dir {
                if depth + 1 < MAX_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    complete = false;
                }
            } else if is_export_name(&e.name) {
                if out.len() >= MAX_FILES {
                    log::warn(&format!(
                        "more than {MAX_FILES} export files; the rest are skipped and nothing is deleted"
                    ));
                    complete = false;
                    continue;
                }
                out.push(ExportFile {
                    path,
                    size: e.size,
                    modified_at: e.modified_at,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, complete))
}

/// `url` without credentials (`https://user:pw@host/x` becomes `https://host/x`).
pub fn strip_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..auth_end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// A fingerprint of the export files (paths, sizes, times).
fn fingerprint(files: &[ExportFile]) -> String {
    let parts: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "{}\u{1}{}\u{1}{}",
                f.path,
                f.size,
                f.modified_at.unwrap_or(0)
            )
        })
        .collect();
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    format!("{:016x}", fnv64(&refs))
}

/// One article after merging duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub article: Article,
    pub source: Source,
    pub file: String,
}

fn is_archived(folder: Option<&str>) -> bool {
    folder.is_some_and(|f| {
        let f = f.to_ascii_lowercase();
        f == "archive" || f == "read archive" || f == "archived"
    })
}

/// Read every export file and merge the articles by normalised URL. Returns the articles and
/// whether every file could be read.
pub fn collect(
    files: &[ExportFile],
    settings: &Settings,
) -> Result<(BTreeMap<String, Saved>, bool)> {
    let mut all: BTreeMap<String, Saved> = BTreeMap::new();
    let mut complete = true;
    for f in files {
        if sink::cancelled() {
            return Err(Error::Cancelled);
        }
        let text = match fs::read_to_string(ROOT, &f.path) {
            Ok(t) => t,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                log::warn(&format!("cannot read export `{}`: {e}", f.path));
                complete = false;
                continue;
            }
        };
        let (source, articles) = formats::parse(&text);
        log::debug(&format!(
            "`{}`: {} articles ({})",
            f.path,
            articles.len(),
            source.name()
        ));
        for mut a in articles {
            if settings.skip_archived && is_archived(a.folder.as_deref()) {
                continue;
            }
            let Some(norm) = normalize_url(&a.url) else {
                continue;
            };
            // Credentials in a saved link never reach the index.
            a.url = strip_userinfo(a.url.trim());
            match all.get_mut(&norm) {
                None => {
                    all.insert(
                        norm,
                        Saved {
                            article: a,
                            source,
                            file: f.path.clone(),
                        },
                    );
                }
                Some(existing) => merge(&mut existing.article, a),
            }
        }
    }
    Ok((all, complete))
}

/// Merge a duplicate save into the first one: union of tags, earliest time, first non-empty
/// title, folder and selection.
fn merge(into: &mut Article, other: Article) {
    for t in other.tags {
        if !into.tags.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
            into.tags.push(t);
        }
    }
    into.added_ms = match (into.added_ms, other.added_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if into.title.is_empty() {
        into.title = other.title;
    }
    if into.folder.is_none() {
        into.folder = other.folder;
    }
    if into.selection.is_none() {
        into.selection = other.selection;
    }
}

/// The document for one saved article.
pub fn document(key: String, saved: &Saved) -> Document {
    let a = &saved.article;
    let title = if a.title.trim().is_empty() {
        a.url.clone()
    } else {
        a.title.clone()
    };
    let mut card = format!(
        "# {}\n\n<{}>\n",
        title.replace('\n', " "),
        a.url.replace('>', "%3E")
    );
    let mut facts = Vec::new();
    if let Some(ms) = a.added_ms {
        facts.push(format!("Saved {}", formats::format_date(ms)));
    }
    if let Some(f) = &a.folder {
        facts.push(format!("Folder: {f}"));
    }
    if !a.tags.is_empty() {
        facts.push(format!("Tags: {}", a.tags.join(", ")));
    }
    if !facts.is_empty() {
        card.push('\n');
        card.push_str(&facts.join(" · "));
        card.push('\n');
    }
    if let Some(sel) = &a.selection {
        card.push('\n');
        for line in sel.lines() {
            card.push_str("> ");
            card.push_str(line);
            card.push('\n');
        }
    }
    let version = format!(
        "{:016x}",
        fnv64(&[
            &title,
            &a.url,
            a.folder.as_deref().unwrap_or(""),
            &a.tags.join("\u{1}"),
            &a.added_ms.unwrap_or(0).to_string(),
            a.selection.as_deref().unwrap_or(""),
        ])
    );
    let mut doc = Document::text(key, "text/markdown", card)
        .with_title(title)
        .with_url(a.url.clone())
        .with_version(version)
        .with_metadata(json!({
            "source": saved.source.name(),
            "export_file": saved.file,
            "folder": a.folder,
            "tags": a.tags,
            "added_at": a.added_ms,
            "selection": a.selection,
        }));
    if let Some(ms) = a.added_ms {
        doc = doc.with_modified_at(ms);
    }
    doc
}

impl Connector for ReadLater {
    fn validate(settings: &Value) -> Result<()> {
        let _ = Settings::parse(settings)?;
        match find_exports() {
            Ok((files, _)) if files.is_empty() => Err(Error::invalid(
                "no export file (.html or .csv) in that folder",
            )),
            Ok(_) => Ok(()),
            Err(Error::NotFound(_)) => {
                Err(Error::invalid("the export file or folder does not exist"))
            }
            Err(e) => Err(e),
        }
    }

    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
        let settings = Settings::parse(settings)?;
        let (files, listed_all) = find_exports()?;
        let print = format!("{}:{}", fingerprint(&files), settings.skip_archived);
        if let Some(c) = &cursor
            && c["fingerprint"].as_str() == Some(print.as_str())
        {
            log::debug("export unchanged");
            return Ok(c.clone());
        }
        let (articles, read_all) = collect(&files, &settings)?;
        let complete = listed_all && read_all;
        let known: BTreeMap<String, Option<String>> = sink::known_items()?.into_iter().collect();
        let mut keys = BTreeSet::new();
        let mut sent = 0usize;
        for (norm, saved) in &articles {
            if sink::cancelled() {
                return Err(Error::Cancelled);
            }
            let key = key_for(norm);
            let doc = document(key.clone(), saved);
            keys.insert(key.clone());
            if known.get(&key).is_some_and(|v| *v == doc.version) {
                continue;
            }
            sink::upsert(doc)?;
            sent += 1;
        }
        let mut deleted = 0usize;
        if complete {
            for key in known.keys() {
                if !keys.contains(key) {
                    sink::delete(key)?;
                    deleted += 1;
                }
            }
        }
        log::info(&format!(
            "{} articles in {} export files: {sent} sent, {deleted} deleted",
            articles.len(),
            files.len()
        ));
        // An incomplete read must be retried, so it does not record the fingerprint.
        let print = if complete {
            Value::String(print)
        } else {
            Value::Null
        };
        Ok(json!({ "v": 1, "fingerprint": print, "articles": articles.len() }))
    }
}

export_plugin!(connector: ReadLater);

#[cfg(test)]
mod tests;
