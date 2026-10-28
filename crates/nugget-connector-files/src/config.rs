//! Settings (the JSON a source is saved with), their JSON Schemas, and the resolved [`Config`].

use std::path::PathBuf;
use std::time::Duration;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};

/// Default cap on the size of an indexed file (50 MiB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Default watcher debounce.
pub const DEFAULT_DEBOUNCE_MS: u64 = 300;
/// Default safety-net reconcile while watching (catches events a backend dropped silently).
/// inotify and FSEvents report queue overflows (and trigger a reconcile at once), but notify's
/// ReadDirectoryChangesW backend drops overflowed events without a word, so Windows rescans
/// more often.
pub const DEFAULT_RESCAN_INTERVAL_SECS: u64 = if cfg!(windows) { 900 } else { 3600 };

/// File extensions indexed by the `files` kind when `include` is not given: the formats
/// `nugget-extract` understands (documents, text, office, PDF, mail/calendar, code, OCR images).
#[rustfmt::skip]
pub const DEFAULT_EXTENSIONS: &[&str] = &[
    // documents and text
    "md", "markdown", "mdx", "txt", "text", "rst", "adoc", "asciidoc", "org", "tex",
    "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml", "xml", "html", "htm", "xhtml", "ipynb",
    // office and PDF
    "pdf", "docx", "xlsx", "xlsm",
    // mail, calendar
    "eml", "ics",
    // code
    "rs", "py", "pyi", "js", "mjs", "cjs", "ts", "mts", "cts", "tsx", "jsx", "go", "java", "kt",
    "kts", "swift", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "rb", "php", "scala", "sh",
    "bash", "zsh", "fish", "ps1", "sql", "lua", "r", "jl", "dart", "ex", "exs", "erl", "hs", "ml",
    "vue", "svelte", "css", "scss", "less", "proto", "graphql", "gradle", "cmake",
    // images (OCR)
    "png", "jpg", "jpeg", "webp",
];

/// Attachment extensions picked up in an Obsidian vault next to `.md` notes.
pub const OBSIDIAN_ATTACHMENTS: &[&str] = &["pdf", "png", "jpg", "jpeg", "webp"];

/// Directories that are never descended into, whatever the settings say (matched by name,
/// ASCII case-insensitively): VCS metadata, dependency and build output, caches, OS trash.
pub const ALWAYS_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "_darcs",
    ".jj",
    ".pijul",
    "CVS",
    "node_modules",
    "bower_components",
    "target",
    "__pycache__",
    ".cache",
    ".venv",
    "venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".gradle",
    ".next",
    ".nuxt",
    ".parcel-cache",
    ".terraform",
    ".Trash",
    ".Trashes",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    // Credential stores: private keys, cloud and cluster credentials, password stores. Indexed
    // text is searchable by the assistant (and so reachable by a prompt-injected request), so
    // these are never read, even with `include_hidden` and a catch-all `include`.
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
];

/// Credential files never indexed, whatever the settings say (ASCII case-insensitive).
const SECRET_FILES: &[&str] = &[
    ".env",
    ".netrc",
    "_netrc",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".git-credentials",
    "credentials.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_ecdsa_sk",
    "id_ed25519_sk",
];
/// `.env.local`, `.env.production`, ...
const SECRET_PREFIXES: &[&str] = &[".env."];
/// Private keys, certificates bundles with keys, keystores and password databases.
const SECRET_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".ppk",
    ".jks",
    ".keystore",
    ".kdbx",
    ".gpg",
    ".asc",
];

/// Whether a file with this name typically holds credentials (never indexed).
pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_FILES.contains(&lower.as_str())
        || SECRET_PREFIXES.iter().any(|p| lower.starts_with(p))
        || SECRET_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// OS junk and editor temporaries, never indexed.
const JUNK_FILES: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    "ehthumbs.db",
    "desktop.ini",
    ".localized",
    "Icon\r",
];
const JUNK_PREFIXES: &[&str] = &["._", "~$", ".~lock.", ".#"];
const JUNK_SUFFIXES: &[&str] = &[
    "~",
    ".swp",
    ".swo",
    ".swx",
    ".tmp",
    ".temp",
    ".crdownload",
    ".part",
    ".partial",
    ".download",
];

/// Ignore-file names honoured in every directory.
pub const IGNORE_FILES: &[&str] = &[".gitignore", ".nuggetignore"];

fn eq_any(name: &str, list: &[&str]) -> bool {
    list.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// Whether a directory with this name is always skipped.
pub fn is_junk_dir(name: &str) -> bool {
    eq_any(name, ALWAYS_SKIP_DIRS) || name.starts_with(".Trash-")
}

/// Whether a file with this name is OS junk or an editor temporary.
pub fn is_junk_file(name: &str) -> bool {
    eq_any(name, JUNK_FILES)
        || JUNK_PREFIXES.iter().any(|p| name.starts_with(p))
        || JUNK_SUFFIXES.iter().any(|s| {
            name.len() > s.len()
                && name.is_char_boundary(name.len() - s.len())
                && name[name.len() - s.len()..].eq_ignore_ascii_case(s)
        })
}

// ---------------------------------------------------------------------------------------------
// Settings

fn default_max() -> u64 {
    DEFAULT_MAX_FILE_BYTES
}
fn default_debounce() -> u64 {
    DEFAULT_DEBOUNCE_MS
}
fn default_rescan() -> u64 {
    DEFAULT_RESCAN_INTERVAL_SECS
}
fn default_true() -> bool {
    true
}

/// Settings of the `files` kind.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesSettings {
    pub roots: Vec<PathBuf>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_max")]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    #[serde(default = "default_rescan")]
    pub rescan_interval_secs: u64,
}

/// Settings of the `obsidian` kind.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObsidianSettings {
    pub vault: PathBuf,
    #[serde(default = "default_true")]
    pub include_attachments: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_max")]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    #[serde(default = "default_rescan")]
    pub rescan_interval_secs: u64,
}

fn common_schema_props() -> serde_json::Map<String, Value> {
    let v = json!({
        "exclude": {
            "type": "array", "items": { "type": "string" }, "default": [],
            "description": "Glob patterns to skip (files and directories), matched case-insensitively. \
                A pattern without `/` matches a name at any depth (`drafts`, `*.log`); a leading `/` \
                anchors it to the root (`/archive`); a trailing `/` matches directories only."
        },
        "max_file_bytes": {
            "type": "integer", "minimum": 1, "default": DEFAULT_MAX_FILE_BYTES,
            "description": "Files larger than this are not indexed."
        },
        "follow_symlinks": {
            "type": "boolean", "default": false,
            "description": "Follow symbolic links (loops are detected). When false, links are skipped."
        },
        "include_hidden": {
            "type": "boolean", "default": false,
            "description": "Index dot-files and hidden files. VCS, dependency and cache directories are skipped regardless."
        },
        "debounce_ms": {
            "type": "integer", "minimum": 20, "maximum": 10000, "default": DEFAULT_DEBOUNCE_MS,
            "description": "Watcher debounce: bursts of events on one path within this window are coalesced."
        },
        "rescan_interval_secs": {
            "type": "integer", "minimum": 0, "default": DEFAULT_RESCAN_INTERVAL_SECS,
            "description": "While watching, run a full reconcile this often as a safety net (0 = never)."
        }
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// JSON Schema of [`FilesSettings`].
pub fn files_schema() -> Value {
    let mut props = common_schema_props();
    props.insert(
        "roots".into(),
        json!({
            "type": "array", "minItems": 1, "uniqueItems": true,
            "items": { "type": "string", "description": "Absolute folder path" },
            "description": "Folders to index. They must be absolute and must not overlap."
        }),
    );
    let defaults: Vec<String> = DEFAULT_EXTENSIONS
        .iter()
        .map(|e| format!("*.{e}"))
        .collect();
    props.insert(
        "include".into(),
        json!({
            "type": "array", "minItems": 1, "items": { "type": "string" }, "default": defaults,
            "description": "Glob patterns of files to index (same syntax as `exclude`). Omit for common \
                document, text, code, office, PDF and image types; use [\"*\"] for every file."
        }),
    );
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Folders",
        "type": "object",
        "additionalProperties": false,
        "required": ["roots"],
        "properties": props,
    })
}

/// JSON Schema of [`ObsidianSettings`].
pub fn obsidian_schema() -> Value {
    let mut props = common_schema_props();
    props.insert(
        "vault".into(),
        json!({
            "type": "string",
            "description": "Absolute path of the vault folder (the one containing `.obsidian/`)."
        }),
    );
    props.insert(
        "include_attachments".into(),
        json!({
            "type": "boolean", "default": true,
            "description": "Also index PDF and image attachments, not only Markdown notes."
        }),
    );
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Obsidian vault",
        "type": "object",
        "additionalProperties": false,
        "required": ["vault"],
        "properties": props,
    })
}

// ---------------------------------------------------------------------------------------------
// Glob matching

/// A list of gitignore-flavoured glob patterns.
#[derive(Debug, Clone)]
pub struct Matcher {
    /// Matched against the `/`-separated relative path.
    path_any: GlobSet,
    path_dir: GlobSet,
    /// Matched against the file name only (patterns without a `/`).
    name_any: GlobSet,
    name_dir: GlobSet,
    empty: bool,
}

impl Matcher {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut path_any = GlobSetBuilder::new();
        let mut path_dir = GlobSetBuilder::new();
        let mut name_any = GlobSetBuilder::new();
        let mut name_dir = GlobSetBuilder::new();
        for raw in patterns {
            let mut p = raw.trim();
            if p.is_empty() {
                return Err(Error::Invalid("empty glob pattern".into()));
            }
            let dir_only = p.len() > 1 && p.ends_with('/');
            if dir_only {
                p = &p[..p.len() - 1];
            }
            let anchored = p.starts_with('/');
            if anchored {
                p = &p[1..];
            }
            let glob = GlobBuilder::new(p)
                .case_insensitive(true)
                .literal_separator(false)
                .build()
                .map_err(|e| Error::Invalid(format!("bad glob `{raw}`: {e}")))?;
            let by_name = !anchored && !p.contains('/');
            match (by_name, dir_only) {
                (true, false) => name_any.add(glob),
                (true, true) => name_dir.add(glob),
                (false, false) => path_any.add(glob),
                (false, true) => path_dir.add(glob),
            };
        }
        let build = |b: GlobSetBuilder| {
            b.build()
                .map_err(|e| Error::Invalid(format!("bad glob set: {e}")))
        };
        Ok(Self {
            path_any: build(path_any)?,
            path_dir: build(path_dir)?,
            name_any: build(name_any)?,
            name_dir: build(name_dir)?,
            empty: patterns.is_empty(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// `rel` is the `/`-separated path below the root, `name` its last component.
    pub fn is_match(&self, rel: &str, name: &str, is_dir: bool) -> bool {
        self.path_any.is_match(rel)
            || self.name_any.is_match(name)
            || (is_dir && (self.path_dir.is_match(rel) || self.name_dir.is_match(name)))
    }
}

// ---------------------------------------------------------------------------------------------
// Resolved configuration

/// Which preset a connector runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flavor {
    Files,
    Obsidian { vault_name: String },
}

/// One configured root folder.
#[derive(Debug, Clone)]
pub struct Root {
    /// As configured (absolute).
    pub path: PathBuf,
}

/// Settings resolved into matchers and durations.
#[derive(Debug, Clone)]
pub struct Config {
    pub kind: &'static str,
    pub flavor: Flavor,
    pub roots: Vec<Root>,
    pub include: Matcher,
    pub exclude: Matcher,
    /// Extra directory names skipped at any depth (the Obsidian preset adds `.obsidian`, `.trash`).
    pub extra_skip_dirs: Vec<&'static str>,
    pub max_file_bytes: u64,
    pub follow_symlinks: bool,
    pub include_hidden: bool,
    pub debounce: Duration,
    pub rescan_interval: Option<Duration>,
}

fn check_common(max_file_bytes: u64, debounce_ms: u64) -> Result<()> {
    if max_file_bytes == 0 {
        return Err(Error::Invalid("max_file_bytes must be at least 1".into()));
    }
    if !(20..=10_000).contains(&debounce_ms) {
        return Err(Error::Invalid(
            "debounce_ms must be between 20 and 10000".into(),
        ));
    }
    Ok(())
}

fn check_roots(roots: &[PathBuf]) -> Result<()> {
    if roots.is_empty() {
        return Err(Error::Invalid(
            "at least one root folder is required".into(),
        ));
    }
    for r in roots {
        if !r.is_absolute() {
            return Err(Error::Invalid(format!(
                "root `{}` must be an absolute path",
                r.display()
            )));
        }
    }
    for (i, a) in roots.iter().enumerate() {
        for b in &roots[i + 1..] {
            if a.starts_with(b) || b.starts_with(a) {
                return Err(Error::Invalid(format!(
                    "roots `{}` and `{}` overlap",
                    a.display(),
                    b.display()
                )));
            }
        }
    }
    Ok(())
}

fn rescan(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs.max(30)))
}

fn parse<T: serde::de::DeserializeOwned>(settings: &Value) -> Result<T> {
    serde_json::from_value(settings.clone()).map_err(|e| Error::Invalid(format!("settings: {e}")))
}

impl Config {
    pub fn files(settings: &Value) -> Result<Self> {
        let s: FilesSettings = parse(settings)?;
        check_roots(&s.roots)?;
        check_common(s.max_file_bytes, s.debounce_ms)?;
        let include = match &s.include {
            Some(list) if list.is_empty() => {
                return Err(Error::Invalid(
                    "include must not be empty (use [\"*\"] for every file)".into(),
                ));
            }
            Some(list) => list.clone(),
            None => DEFAULT_EXTENSIONS
                .iter()
                .map(|e| format!("*.{e}"))
                .collect(),
        };
        Ok(Self {
            kind: crate::FILES_KIND,
            flavor: Flavor::Files,
            roots: s.roots.into_iter().map(|path| Root { path }).collect(),
            include: Matcher::new(&include)?,
            exclude: Matcher::new(&s.exclude)?,
            extra_skip_dirs: Vec::new(),
            max_file_bytes: s.max_file_bytes,
            follow_symlinks: s.follow_symlinks,
            include_hidden: s.include_hidden,
            debounce: Duration::from_millis(s.debounce_ms),
            rescan_interval: rescan(s.rescan_interval_secs),
        })
    }

    pub fn obsidian(settings: &Value) -> Result<Self> {
        let s: ObsidianSettings = parse(settings)?;
        check_roots(std::slice::from_ref(&s.vault))?;
        check_common(s.max_file_bytes, s.debounce_ms)?;
        let vault_name = s
            .vault
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Invalid("vault path has no folder name".into()))?;
        let mut include = vec!["*.md".to_string()];
        if s.include_attachments {
            include.extend(OBSIDIAN_ATTACHMENTS.iter().map(|e| format!("*.{e}")));
        }
        Ok(Self {
            kind: crate::OBSIDIAN_KIND,
            flavor: Flavor::Obsidian { vault_name },
            roots: vec![Root { path: s.vault }],
            include: Matcher::new(&include)?,
            exclude: Matcher::new(&s.exclude)?,
            extra_skip_dirs: vec![".obsidian", ".trash"],
            max_file_bytes: s.max_file_bytes,
            follow_symlinks: s.follow_symlinks,
            include_hidden: s.include_hidden,
            debounce: Duration::from_millis(s.debounce_ms),
            rescan_interval: rescan(s.rescan_interval_secs),
        })
    }

    /// Validation beyond parsing: every root must currently be a readable directory.
    pub fn check_roots_exist(&self) -> Result<()> {
        for r in &self.roots {
            if !r.path.is_dir() {
                return Err(Error::Invalid(format!(
                    "`{}` is not a folder",
                    r.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Stable hash of the root list: a change means root indices (and so keys) changed meaning.
    pub fn roots_hash(&self) -> String {
        // FNV-1a 64: stable across Rust releases, unlike `DefaultHasher`.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        feed(self.kind.as_bytes());
        for r in &self.roots {
            feed(&[0]);
            feed(r.path.to_string_lossy().as_bytes());
        }
        format!("{h:016x}")
    }

    /// Whether a directory is pruned by name (junk, preset extras, hidden policy).
    pub fn skip_dir_name(&self, name: &str) -> bool {
        is_junk_dir(name)
            || self
                .extra_skip_dirs
                .iter()
                .any(|d| d.eq_ignore_ascii_case(name))
    }

    /// Cheap path-only test used to drop watcher noise: true when some component of `rel` is a
    /// skipped directory, a hidden name (unless hidden files are included) or OS junk.
    pub fn is_noise(&self, rel: &str) -> bool {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let Some((last, dirs)) = parts.split_last() else {
            return false;
        };
        dirs.iter()
            .any(|d| self.skip_dir_name(d) || (!self.include_hidden && d.starts_with('.')))
            || self.skip_dir_name(last)
            || is_junk_file(last)
            || is_secret_file(last)
            || (!self.include_hidden && last.starts_with('.'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &[&str]) -> Matcher {
        Matcher::new(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn matcher_semantics() {
        let x = m(&["*.log", "drafts", "/archive", "build/", "docs/**/*.tmp.md"]);
        assert!(
            x.is_match("a/b/c.LOG", "c.LOG", false),
            "name glob, any depth, any case"
        );
        assert!(x.is_match("drafts", "drafts", true));
        assert!(
            x.is_match("x/drafts", "drafts", true),
            "bare name matches at any depth"
        );
        assert!(x.is_match("archive", "archive", true));
        assert!(
            !x.is_match("x/archive", "archive", true),
            "leading / anchors"
        );
        assert!(x.is_match("y/build", "build", true));
        assert!(
            !x.is_match("y/build", "build", false),
            "trailing / is dirs only"
        );
        assert!(x.is_match("docs/a/b/c.tmp.md", "c.tmp.md", false));
        assert!(!x.is_match("notes/c.md", "c.md", false));
        assert!(m(&[]).is_empty());
        assert!(Matcher::new(&["a[".into()]).is_err());
        assert!(Matcher::new(&["  ".into()]).is_err());
    }

    #[test]
    fn junk_names() {
        for n in [
            ".DS_Store",
            "Thumbs.db",
            "._foo.md",
            "~$report.docx",
            ".#note.md",
            "note.md~",
            "x.SWP",
            "file.crdownload",
        ] {
            assert!(is_junk_file(n), "{n}");
        }
        for n in ["notes.md", "temp.md", "~", "partial.txt"] {
            assert!(!is_junk_file(n), "{n}");
        }
        for n in [
            ".git",
            "node_modules",
            "TARGET",
            "__pycache__",
            ".Trash-1000",
        ] {
            assert!(is_junk_dir(n), "{n}");
        }
        assert!(!is_junk_dir("src"));
    }

    #[test]
    fn files_settings_defaults_and_validation() {
        let root = std::env::temp_dir();
        let c = Config::files(&json!({ "roots": [root] })).unwrap();
        assert_eq!(c.max_file_bytes, DEFAULT_MAX_FILE_BYTES);
        assert!(!c.follow_symlinks && !c.include_hidden);
        assert!(c.include.is_match("a/b.PDF", "b.PDF", false));
        assert!(c.include.is_match("x.md", "x.md", false));
        assert!(!c.include.is_match("x.exe", "x.exe", false));
        assert_eq!(c.debounce, Duration::from_millis(DEFAULT_DEBOUNCE_MS));
        assert!(c.check_roots_exist().is_ok());

        let bad = [
            json!({}),
            json!({ "roots": [] }),
            json!({ "roots": ["relative/dir"] }),
            json!({ "roots": [root, root.join("sub")] }),
            json!({ "roots": [root], "include": [] }),
            json!({ "roots": [root], "exclude": ["a["] }),
            json!({ "roots": [root], "max_file_bytes": 0 }),
            json!({ "roots": [root], "debounce_ms": 1 }),
            json!({ "roots": [root], "rooots": [] }),
            json!({ "roots": "nope" }),
        ];
        for b in bad {
            assert!(
                matches!(Config::files(&b), Err(Error::Invalid(_))),
                "accepted {b}"
            );
        }
        let missing =
            Config::files(&json!({ "roots": [root.join("definitely-missing-nugget")] })).unwrap();
        assert!(missing.check_roots_exist().is_err());
    }

    #[test]
    fn roots_hash_tracks_roots() {
        let a = std::env::temp_dir().join("a");
        let b = std::env::temp_dir().join("b");
        let h1 = Config::files(&json!({ "roots": [a, b] }))
            .unwrap()
            .roots_hash();
        let h2 = Config::files(&json!({ "roots": [a, b], "exclude": ["x"] }))
            .unwrap()
            .roots_hash();
        let h3 = Config::files(&json!({ "roots": [b, a] }))
            .unwrap()
            .roots_hash();
        assert_eq!(h1, h2, "filters do not change root identity");
        assert_ne!(h1, h3, "order changes root indices");
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn obsidian_presets() {
        let vault = std::env::temp_dir().join("My Vault");
        let c = Config::obsidian(&json!({ "vault": vault })).unwrap();
        assert_eq!(
            c.flavor,
            Flavor::Obsidian {
                vault_name: "My Vault".into()
            }
        );
        assert!(c.include.is_match("n.md", "n.md", false));
        assert!(c.include.is_match("a/img.png", "img.png", false));
        assert!(!c.include.is_match("a/x.txt", "x.txt", false));
        assert!(c.skip_dir_name(".obsidian") && c.skip_dir_name(".trash"));
        let c = Config::obsidian(&json!({ "vault": vault, "include_attachments": false })).unwrap();
        assert!(!c.include.is_match("a/img.png", "img.png", false));
        assert!(Config::obsidian(&json!({ "vault": "rel" })).is_err());
    }

    #[test]
    fn noise_detection() {
        let c = Config::files(&json!({ "roots": [std::env::temp_dir()] })).unwrap();
        assert!(c.is_noise(".git/index"));
        assert!(c.is_noise("a/node_modules/x/y.js"));
        assert!(c.is_noise("a/.hidden/y.md"));
        assert!(c.is_noise("a/.y.md"));
        assert!(c.is_noise("a/.note.md.swp"));
        assert!(c.is_noise("a/target"));
        assert!(!c.is_noise("a/b/c.md"));
        assert!(!c.is_noise(""));
        let c = Config::files(&json!({ "roots": [std::env::temp_dir()], "include_hidden": true }))
            .unwrap();
        assert!(!c.is_noise("a/.hidden/y.md"));
        assert!(c.is_noise(".git/HEAD"));
    }

    #[test]
    fn schemas_describe_settings() {
        let f = files_schema();
        assert_eq!(f["required"], json!(["roots"]));
        assert!(
            f["properties"]["include"]["default"]
                .as_array()
                .unwrap()
                .len()
                > 20
        );
        for k in [
            "roots",
            "include",
            "exclude",
            "max_file_bytes",
            "follow_symlinks",
            "include_hidden",
        ] {
            assert!(f["properties"].get(k).is_some(), "{k}");
        }
        let o = obsidian_schema();
        assert_eq!(o["required"], json!(["vault"]));
        assert!(o["properties"].get("include_attachments").is_some());
    }
}
