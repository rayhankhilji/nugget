//! Nugget plugin: import an Obsidian vault.
//!
//! Settings: `{ "vault": "/absolute/path/to/vault", "exclude": ["Templates"] }`. `vault` is the
//! manifest's only file root, so the host lets this plugin read below that folder and nothing
//! else, read-only.
//!
//! Each pass walks the vault through `nugget_plugin_sdk::fs`, skipping `.obsidian/`, `.trash/`,
//! every other dot-folder and dot-file, and the excluded folders. Every `.md` note becomes one
//! `text/markdown` document keyed by its vault-relative path (`Folder/Note.md`), versioned by
//! `<mtime>:<size>`, so only notes whose version differs from what the store holds
//! (`sink::known_items`) are read and sent. The front matter (`title`, `tags`, `aliases`, other
//! fields), inline `#tags`, `[[wikilinks]]`, `![[embeds]]` and relative links to notes go into
//! the metadata; the content is the note without its front matter.
//!
//! When the walk saw the whole vault (no folder failed to list, not cancelled), notes the store
//! holds that are no longer in it are deleted. This happens on every such pass, the reconciling
//! full pass (no cursor) included. A folder that cannot be listed makes the pass skip deletes,
//! so a transient error never wipes notes.

pub mod note;

use std::collections::{BTreeMap, BTreeSet};

use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Value, export_plugin, fs, json, log, sink,
};

/// The file root (settings field) holding the vault path.
pub const ROOT: &str = "vault";
/// Deepest folder nesting walked (guards against symlink loops inside the vault).
pub const MAX_DEPTH: usize = 64;
/// Most notes imported from one vault.
pub const MAX_NOTES: usize = 100_000;

pub struct Vault;

/// Parsed source settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub vault: String,
    /// Vault-relative folders to skip, `/`-separated, no leading or trailing `/`.
    pub exclude: Vec<String>,
}

impl Settings {
    pub fn parse(v: &Value) -> Result<Self> {
        let vault = v[ROOT]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::invalid("settings.vault (the vault folder) is required"))?;
        if !is_absolute(vault) {
            return Err(Error::invalid(
                "settings.vault must be an absolute folder path",
            ));
        }
        let mut exclude = Vec::new();
        match &v["exclude"] {
            Value::Null => {}
            Value::Array(a) if a.len() <= 64 => {
                for e in a {
                    let e = e
                        .as_str()
                        .ok_or_else(|| Error::invalid("settings.exclude must hold strings"))?
                        .trim()
                        .replace('\\', "/");
                    let e = e.trim_matches('/');
                    if e.is_empty() || e.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
                        return Err(Error::invalid(format!(
                            "settings.exclude: `{e}` must be a folder inside the vault"
                        )));
                    }
                    exclude.push(e.to_string());
                }
            }
            _ => {
                return Err(Error::invalid(
                    "settings.exclude must be a list of at most 64 folders",
                ));
            }
        }
        Ok(Self {
            vault: vault.to_string(),
            exclude,
        })
    }

    fn excluded(&self, path: &str) -> bool {
        self.exclude.iter().any(|e| {
            path.eq_ignore_ascii_case(e)
                || (path.len() > e.len()
                    && path.is_char_boundary(e.len())
                    && path[..e.len()].eq_ignore_ascii_case(e)
                    && path.as_bytes()[e.len()] == b'/')
        })
    }

    /// The vault's name: the last segment of its path.
    pub fn vault_name(&self) -> &str {
        self.vault
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
    }
}

/// `/x`, `C:\x`, `C:/x` or `\\server\share`.
fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

/// One note found by the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: String,
    pub size: u64,
    pub modified_at: Option<i64>,
}

impl Found {
    pub fn version(&self) -> String {
        format!("{}:{}", self.modified_at.unwrap_or(0), self.size)
    }
}

/// The result of walking the vault.
#[derive(Debug, Default)]
pub struct Walk {
    pub notes: Vec<Found>,
    /// Whether every folder was listed (deletes are only safe then).
    pub complete: bool,
}

/// Walk the vault for `.md` notes, in path order.
pub fn walk(settings: &Settings) -> Result<Walk> {
    let mut out = Walk {
        notes: Vec::new(),
        complete: true,
    };
    // The root itself must list, or the pass fails (a missing vault must not delete anything).
    let mut stack: Vec<(String, usize)> = vec![(String::new(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if sink::cancelled() {
            return Err(Error::Cancelled);
        }
        let entries = match fs::read_dir(ROOT, &dir) {
            Ok(e) => e,
            Err(e) if dir.is_empty() => return Err(e),
            Err(e) => {
                log::warn(&format!(
                    "cannot list `{dir}`: {e}; skipping deletes this pass"
                ));
                out.complete = false;
                continue;
            }
        };
        let mut subdirs = Vec::new();
        for e in entries {
            if e.name.starts_with('.') {
                continue;
            }
            let path = fs::join(&dir, &e.name);
            if e.is_dir {
                if settings.excluded(&path) {
                    continue;
                }
                if depth + 1 >= MAX_DEPTH {
                    log::warn(&format!("`{path}` is nested too deeply; skipped"));
                    out.complete = false;
                    continue;
                }
                subdirs.push((path, depth + 1));
            } else if e.name.len() > 3 && e.name[e.name.len() - 3..].eq_ignore_ascii_case(".md") {
                if out.notes.len() >= MAX_NOTES {
                    log::warn(&format!(
                        "more than {MAX_NOTES} notes; the rest are skipped"
                    ));
                    out.complete = false;
                    break;
                }
                out.notes.push(Found {
                    path,
                    size: e.size,
                    modified_at: e.modified_at,
                });
            }
        }
        // Reverse so the stack pops folders in name order.
        stack.extend(subdirs.into_iter().rev());
    }
    out.notes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

impl Connector for Vault {
    fn validate(settings: &Value) -> Result<()> {
        let _ = Settings::parse(settings)?;
        match fs::stat(ROOT, "") {
            Ok(e) if e.is_dir => Ok(()),
            Ok(_) => Err(Error::invalid("settings.vault is a file, not a folder")),
            Err(Error::NotFound(_)) => Err(Error::invalid("the vault folder does not exist")),
            Err(e) => Err(e),
        }
    }

    fn sync(settings: &Value, _cursor: Option<Value>) -> Result<Value> {
        let settings = Settings::parse(settings)?;
        let walk = walk(&settings)?;
        let known: BTreeMap<String, Option<String>> = sink::known_items()?.into_iter().collect();
        let mut seen = BTreeSet::new();
        let (mut sent, mut skipped) = (0usize, 0usize);
        for found in &walk.notes {
            if sink::cancelled() {
                return Err(Error::Cancelled);
            }
            if found.path.len() > 1024 {
                let short: String = found.path.chars().take(64).collect();
                log::warn(&format!("path too long for a key; skipped: {short}…"));
                continue;
            }
            seen.insert(found.path.as_str());
            let version = found.version();
            if known
                .get(&found.path)
                .is_some_and(|v| v.as_deref() == Some(&version))
            {
                continue;
            }
            match fs::read_to_string(ROOT, &found.path) {
                Ok(text) => {
                    sink::upsert(document(&settings, found, &text))?;
                    sent += 1;
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // Too large, gone since the listing, unreadable: keep what the store has.
                Err(e) => {
                    log::warn(&format!("cannot read `{}`: {e}", found.path));
                    skipped += 1;
                }
            }
        }
        let mut deleted = 0usize;
        if walk.complete {
            for key in known.keys() {
                if !seen.contains(key.as_str()) {
                    sink::delete(key)?;
                    deleted += 1;
                }
            }
        }
        log::info(&format!(
            "{} notes: {sent} sent, {deleted} deleted, {skipped} unreadable",
            walk.notes.len()
        ));
        Ok(json!({ "v": 1, "notes": walk.notes.len() }))
    }
}

/// The document for one note.
pub fn document(settings: &Settings, found: &Found, text: &str) -> Document {
    let parsed = note::parse(text);
    let file_name = found.path.rsplit('/').next().unwrap_or(&found.path);
    let stem = &file_name[..file_name.len() - 3];
    let folder = found
        .path
        .rfind('/')
        .map(|i| &found.path[..i])
        .unwrap_or("");
    let title = parsed.title.clone().unwrap_or_else(|| stem.to_string());
    let url = format!(
        "obsidian://open?vault={}&file={}",
        note::percent_encode(settings.vault_name()),
        note::percent_encode(&found.path[..found.path.len() - 3])
    );
    let mut doc = Document::text(found.path.clone(), "text/markdown", parsed.body)
        .with_title(title)
        .with_version(found.version())
        .with_url(url)
        .with_metadata(json!({
            "vault": settings.vault_name(),
            "path": found.path,
            "folder": folder,
            "tags": parsed.tags,
            "aliases": parsed.aliases,
            "links": parsed.links,
            "embeds": parsed.embeds,
            "properties": parsed.properties,
        }));
    if let Some(ms) = found.modified_at {
        doc = doc.with_modified_at(ms);
    }
    doc
}

export_plugin!(connector: Vault);

#[cfg(test)]
mod tests;
