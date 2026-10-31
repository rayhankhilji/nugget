//! The reconcile engine shared by `sync` and `watch`: walk (part of) the roots with the
//! configured filters, compare against known versions, and emit upserts and deletes.
//!
//! Everything here is blocking filesystem code; callers run it on a blocking thread.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use ignore::{DirEntry, WalkBuilder};
use nugget_connector::{CancellationToken, Change, Content, Document};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use crate::config::{Config, Flavor, IGNORE_FILES, is_junk_file, is_secret_file};
use crate::key::{file_url, item_key, obsidian_url, rel_key};

/// Item key → last known connector version (`None` = unknown, always re-sent).
pub(crate) type KnownMap = BTreeMap<String, Option<String>>;

/// A subtree to reconcile: `rel` is `/`-separated below root `root`; empty = the whole root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Target {
    pub root: usize,
    pub rel: String,
}

impl Target {
    pub fn whole_root(root: usize) -> Self {
        Self {
            root,
            rel: String::new(),
        }
    }

    /// The key of the target itself (`<root>:<rel>`).
    fn key(&self) -> String {
        format!("{}:{}", self.root, self.rel)
    }

    /// Whether `key` names the target or something below it.
    pub fn covers_key(&self, key: &str) -> bool {
        let own = self.key();
        if self.rel.is_empty() {
            return key.starts_with(&own);
        }
        key == own || (key.starts_with(&own) && key.as_bytes().get(own.len()) == Some(&b'/'))
    }

    /// Keys of `known` this target covers (a range scan, not a full pass).
    pub fn covered<'a, V>(&self, known: &'a BTreeMap<String, V>) -> Vec<(&'a String, &'a V)> {
        let own = self.key();
        known
            .range(own.clone()..)
            .take_while(|(k, _)| k.starts_with(&own))
            .filter(|(k, _)| self.covers_key(k))
            .collect()
    }

    /// Whether `other` lies inside this target (or is it).
    fn contains(&self, other: &Target) -> bool {
        self.root == other.root
            && (self.rel.is_empty()
                || other.rel == self.rel
                || (other.rel.starts_with(&self.rel)
                    && other.rel.as_bytes().get(self.rel.len()) == Some(&b'/')))
    }
}

/// What to reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every root, and every known key (keys of vanished roots are deleted).
    All,
    /// Only these subtrees.
    Targets(Vec<Target>),
}

impl Scope {
    /// Drop targets nested in other targets.
    pub fn targets(mut targets: Vec<Target>) -> Self {
        targets.sort();
        targets.dedup();
        let mut out: Vec<Target> = Vec::with_capacity(targets.len());
        for t in targets {
            if !out.iter().any(|o| o.contains(&t)) {
                out.retain(|o| !t.contains(o));
                out.push(t);
            }
        }
        Scope::Targets(out)
    }

    /// The subset of `known` this scope covers.
    pub fn covered_known(&self, known: &KnownMap) -> KnownMap {
        match self {
            Scope::All => known.clone(),
            Scope::Targets(ts) => ts
                .iter()
                .flat_map(|t| t.covered(known))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

/// Messages from a reconcile pass.
#[derive(Debug)]
pub(crate) enum Emit {
    Change(Change),
    /// `scanned` files seen so far; a good moment to checkpoint.
    Progress(u64),
}

/// The result of a reconcile pass.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Every included file in scope, with its current version.
    pub found: BTreeMap<String, String>,
    /// Keys a delete was sent for.
    pub deleted: Vec<String>,
    pub scanned: u64,
    pub upserted: u64,
}

/// `mtime_ns:size`. Pre-1970 times are negative; a missing mtime counts as 0.
pub(crate) fn version_of(md: &Metadata) -> String {
    let ns: i128 = match md.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        },
        Err(_) => 0,
    };
    format!("{ns}:{}", md.len())
}

/// Keys covered by walk errors or vanished roots: never deleted on this pass.
#[derive(Default)]
struct Protected(Vec<Target>, HashSet<String>);

impl Protected {
    fn contains(&self, key: &str) -> bool {
        self.1.contains(key) || self.0.iter().any(|t| t.covers_key(key))
    }
}

/// The path of an I/O error while walking (a directory or file we could not read). The walker
/// nests wrappers freely, e.g. `WithPath { WithDepth { Io } }`; ignore-file parse errors and
/// symlink loops are not I/O errors and yield `None`.
fn io_error_path(e: &ignore::Error) -> Option<&Path> {
    fn is_io(e: &ignore::Error) -> bool {
        match e {
            ignore::Error::Io(_) => true,
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithPath { err, .. } => {
                is_io(err)
            }
            _ => false,
        }
    }
    match e {
        ignore::Error::WithDepth { err, .. } => io_error_path(err),
        ignore::Error::WithPath { path, err } => {
            io_error_path(err).or_else(|| is_io(err).then_some(path.as_path()))
        }
        _ => None,
    }
}

/// Run one reconcile pass. `emit` returns false when the receiver is gone (stop early).
/// With `force`, known versions are ignored and every found file is re-sent.
pub(crate) fn reconcile(
    cfg: &Arc<Config>,
    scope: &Scope,
    known: &KnownMap,
    force: bool,
    cancel: &CancellationToken,
    progress_every: u64,
    emit: &mut dyn FnMut(Emit) -> bool,
) -> Result<Outcome> {
    let mut out = Outcome::default();
    let mut protected = Protected::default();
    let mut send = |e: Emit| -> Result<()> {
        if emit(e) {
            Ok(())
        } else {
            Err(Error::Cancelled)
        }
    };

    // Targets per root.
    let mut per_root: BTreeMap<usize, Vec<Target>> = BTreeMap::new();
    match scope {
        Scope::All => {
            for i in 0..cfg.roots.len() {
                per_root.insert(i, vec![Target::whole_root(i)]);
            }
        }
        Scope::Targets(ts) => {
            for t in ts {
                if t.root < cfg.roots.len() {
                    per_root.entry(t.root).or_default().push(t.clone());
                }
            }
        }
    }

    for (idx, targets) in per_root {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let root = cfg.roots[idx].path.clone();
        if !root.is_dir() {
            // An unmounted drive or a moved folder: keep what we have rather than wiping it.
            tracing::warn!(root = %root.display(), "root folder is unavailable; keeping its items");
            protected.0.push(Target::whole_root(idx));
            continue;
        }
        walk_root(
            cfg,
            idx,
            &root,
            &targets,
            known,
            force,
            cancel,
            progress_every,
            &mut out,
            &mut protected,
            &mut send,
        )?;
    }

    // Deletes: everything in scope that was known but not found.
    let candidates: Vec<&String> = match scope {
        Scope::All => known.keys().collect(),
        Scope::Targets(ts) => ts
            .iter()
            .flat_map(|t| t.covered(known))
            .map(|(k, _)| k)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    for key in candidates {
        if out.found.contains_key(key) || protected.contains(key) {
            continue;
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        send(Emit::Change(Change::Delete(ItemKey::new(key.clone()))))?;
        out.deleted.push(key.clone());
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn walk_root(
    cfg: &Arc<Config>,
    idx: usize,
    root: &Path,
    targets: &[Target],
    known: &KnownMap,
    force: bool,
    cancel: &CancellationToken,
    progress_every: u64,
    out: &mut Outcome,
    protected: &mut Protected,
    send: &mut dyn FnMut(Emit) -> Result<()>,
) -> Result<()> {
    // Restrict the walk to the targets: descend only into their ancestors and themselves.
    let whole = targets.iter().any(|t| t.rel.is_empty());
    // Compared as key strings (`rel_key`), not paths: a target comes from a key, and a key of a
    // non-UTF-8 name does not join back into the same path.
    let mut scope_set: HashSet<String> = HashSet::new();
    let mut ancestors: HashSet<String> = HashSet::new();
    if !whole {
        for t in targets {
            let mut prefix = t.rel.as_str();
            while let Some((parent, _)) = prefix.rsplit_once('/') {
                ancestors.insert(parent.to_string());
                prefix = parent;
            }
            scope_set.insert(t.rel.clone());
        }
    }

    // Files are checked to still lie under the root (by canonical parent) before they are sent.
    let canon_root = std::fs::canonicalize(root).ok();

    let filter_cfg = Arc::clone(cfg);
    let filter_root = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(false)
        .hidden(!cfg.include_hidden)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(cfg.follow_symlinks)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            keep_entry(&filter_cfg, &filter_root, whole, &scope_set, &ancestors, e)
        });
    builder.add_custom_ignore_filename(".nuggetignore");

    for result in builder.build() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                if let Some(p) = io_error_path(&err)
                    && let Ok(rel) = p.strip_prefix(root)
                {
                    protected.0.push(Target {
                        root: idx,
                        rel: rel_key(rel),
                    });
                }
                tracing::warn!(error = %err, "skipping unreadable path");
                continue;
            }
        };
        if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel_key(rel_path);
        let name = entry.file_name().to_string_lossy();
        if !cfg.include.is_match(&rel, &name, false) {
            continue;
        }
        let key = item_key(idx, rel_path);
        let md = match entry.metadata() {
            Ok(md) => md,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot stat file; skipping");
                protected.1.insert(key);
                continue;
            }
        };
        if md.len() > cfg.max_file_bytes {
            tracing::debug!(path = %path.display(), size = md.len(), "over max_file_bytes; skipping");
            continue;
        }
        out.scanned += 1;
        if progress_every > 0 && out.scanned.is_multiple_of(progress_every) {
            send(Emit::Progress(out.scanned))?;
        }
        let version = version_of(&md);
        let prev = known.get(&key);
        let unchanged = !force && prev.is_some_and(|v| v.as_deref() == Some(version.as_str()));
        if unchanged {
            out.found.insert(key, version);
            continue;
        }
        let built = match &canon_root {
            Some(cr) => build_document(cfg, root, cr, path, &rel, &key, &version, &md),
            None => Err(Error::NotFound(format!("root `{}`", root.display()))),
        };
        match built {
            Ok(doc) => {
                send(Emit::Change(Change::Upsert(doc)))?;
                out.upserted += 1;
                out.found.insert(key, version);
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot read file; skipping");
                // Keep a previously indexed copy (e.g. a file locked by another program).
                protected.1.insert(key);
            }
        }
    }
    Ok(())
}

fn keep_entry(
    cfg: &Config,
    root: &Path,
    whole: bool,
    scope: &HashSet<String>,
    ancestors: &HashSet<String>,
    e: &DirEntry,
) -> bool {
    if e.depth() == 0 {
        return true;
    }
    let path = e.path();
    if !whole {
        let Ok(rel) = path.strip_prefix(root) else {
            return false;
        };
        let rel = rel_key(rel);
        let in_scope = ancestors.contains(&rel)
            || scope.contains(&rel)
            || rel
                .match_indices('/')
                .any(|(i, _)| scope.contains(&rel[..i]));
        if !in_scope {
            return false;
        }
    }
    let name = e.file_name().to_string_lossy();
    let is_dir = e.file_type().is_some_and(|t| t.is_dir());
    if is_dir {
        if cfg.skip_dir_name(&name) {
            return false;
        }
    } else if is_junk_file(&name) || is_secret_file(&name) || IGNORE_FILES.contains(&name.as_ref())
    {
        return false;
    }
    if !cfg.exclude.is_empty()
        && let Ok(rel) = path.strip_prefix(root)
        && cfg.exclude.is_match(&rel_key(rel), &name, is_dir)
    {
        return false;
    }
    true
}

/// Read up to `buf.len()` bytes.
fn read_head(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// The path the pipeline should open for `path`, checked against the scan:
/// - without `follow_symlinks`, the file's canonical parent must still lie under the root's
///   canonical path (a parent directory swapped for a symlink to elsewhere is refused);
/// - with `follow_symlinks`, links are followed by policy, so the fully resolved path is sent
///   (the pipeline opens it without following a final symlink).
fn content_path(cfg: &Config, canon_root: &Path, path: &Path) -> Result<PathBuf> {
    if cfg.follow_symlinks {
        return Ok(std::fs::canonicalize(path)?);
    }
    let Some(parent) = path.parent() else {
        return Err(Error::Invalid(format!(
            "`{}` has no parent",
            path.display()
        )));
    };
    let canon_parent = std::fs::canonicalize(parent)?;
    if !canon_parent.starts_with(canon_root) {
        return Err(Error::Integrity(format!(
            "{}: `{}` now resolves outside its root",
            nugget_extract::FILE_CHANGED,
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

#[allow(clippy::too_many_arguments)]
fn build_document(
    cfg: &Config,
    root: &Path,
    canon_root: &Path,
    path: &Path,
    rel: &str,
    key: &str,
    version: &str,
    md: &Metadata,
) -> Result<Document> {
    let open_path = content_path(cfg, canon_root, path)?;
    // Opening proves the file is readable now (and is still the regular file the walk saw:
    // no final symlink, no FIFO, same size and mtime), and the head lets us sniff the type.
    let expect = nugget_extract::FileExpect::of(md);
    let mut f: File = nugget_extract::open_regular(&open_path, Some(expect))?;
    let mut head = [0u8; 4096];
    let n = read_head(&mut f, &mut head)?;
    drop(f);

    let is_md = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
    let mime = match &cfg.flavor {
        Flavor::Obsidian { .. } if is_md => "text/markdown".to_string(),
        _ => nugget_extract::detect_mime(Some(path), &head[..n]),
    };

    // Mail and calendar files name themselves (Subject, SUMMARY), which extraction reads; their
    // file names (`mail-010.eml`) are not titles. Everything else is titled by its file name.
    let self_titled = matches!(
        mime.as_str(),
        "message/rfc822"
            | "message/global"
            | "text/calendar"
            | "application/ics"
            | "text/x-vcalendar"
    );
    let mut doc = Document::new(key, mime, Content::File(open_path));
    doc.title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty() && !self_titled);
    doc.version = Some(version.to_string());
    doc.modified_at = md.modified().ok().map(OffsetDateTime::from);
    let furl = file_url(path);
    let mut meta = Map::new();
    meta.insert("path".into(), json!(rel));
    meta.insert("size".into(), json!(md.len()));
    // What the pipeline must find when it opens the file (see `nugget_extract::FileExpect`).
    meta.insert(nugget_extract::FILE_CHECK_KEY.into(), expect.to_metadata());
    match &cfg.flavor {
        Flavor::Files => {
            meta.insert("root".into(), json!(root.to_string_lossy()));
            doc.url = furl;
        }
        Flavor::Obsidian { vault_name } => {
            meta.insert("vault".into(), json!(vault_name));
            meta.insert("vault_path".into(), json!(root.to_string_lossy()));
            if let Some(u) = furl {
                meta.insert("file_url".into(), json!(u));
            }
            doc.url = Some(obsidian_url(vault_name, rel));
        }
    }
    doc.metadata = Value::Object(meta);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(root: usize, rel: &str) -> Target {
        Target {
            root,
            rel: rel.into(),
        }
    }

    /// A root with `a.md` (as scanned: lstat metadata and version) and an outside folder.
    struct Swap {
        _dir: tempfile::TempDir,
        root: PathBuf,
        canon_root: PathBuf,
        // Read only by the Unix-only symlink tests.
        #[cfg_attr(not(unix), allow(dead_code))]
        outside: PathBuf,
        cfg: Config,
    }

    fn swap_fixture() -> Swap {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.md"), "outside secret").unwrap();
        let cfg = Config::files(&json!({ "roots": [root] })).unwrap();
        Swap {
            canon_root: std::fs::canonicalize(&root).unwrap(),
            _dir: dir,
            root,
            outside,
            cfg,
        }
    }

    fn build(f: &Swap, rel: &str, md: &Metadata) -> Result<Document> {
        let path = rel.split('/').fold(f.root.clone(), |p, c| p.join(c));
        build_document(
            &f.cfg,
            &f.root,
            &f.canon_root,
            &path,
            rel,
            &format!("0:{rel}"),
            &version_of(md),
            md,
        )
    }

    /// Run `build` on a thread so a blocking open fails the test instead of hanging it.
    #[cfg(unix)]
    fn build_bounded(f: Swap, rel: &'static str, md: Metadata) -> Result<Document> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(build(&f, rel, &md));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("building the document blocked")
    }

    #[test]
    fn documents_carry_the_scanned_file_check() {
        let f = swap_fixture();
        std::fs::write(f.root.join("a.md"), "hello").unwrap();
        let md = std::fs::symlink_metadata(f.root.join("a.md")).unwrap();
        let doc = build(&f, "a.md", &md).unwrap();
        assert_eq!(doc.content, Content::File(f.root.join("a.md")));
        assert_eq!(
            nugget_extract::FileExpect::from_metadata(&doc.metadata),
            Some(nugget_extract::FileExpect::of(&md))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_symlink_after_the_scan_is_not_sent() {
        let f = swap_fixture();
        let p = f.root.join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let md = std::fs::symlink_metadata(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(f.outside.join("secret.md"), &p).unwrap();
        let err = build(&f, "a.md", &md).unwrap_err();
        assert!(nugget_extract::is_file_changed(&err), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_fifo_after_the_scan_is_refused_without_blocking() {
        let f = swap_fixture();
        let p = f.root.join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let md = std::fs::symlink_metadata(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("mkfifo unavailable; skipping");
            return;
        }
        let err = build_bounded(f, "a.md", md).unwrap_err();
        assert!(nugget_extract::is_file_changed(&err), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_swapped_for_a_symlink_out_of_the_root_is_refused() {
        let f = swap_fixture();
        std::fs::write(f.root.join("sub").join("secret.md"), "inside").unwrap();
        let md = std::fs::symlink_metadata(f.root.join("sub").join("secret.md")).unwrap();
        // Same name, same size and mtime outside: only the parent check notices.
        std::fs::write(f.outside.join("secret.md"), "insidX").unwrap();
        let t = md.modified().unwrap();
        let out = std::fs::OpenOptions::new()
            .write(true)
            .open(f.outside.join("secret.md"))
            .unwrap();
        out.set_modified(t).unwrap();
        drop(out);
        std::fs::remove_dir_all(f.root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&f.outside, f.root.join("sub")).unwrap();
        let err = build(&f, "sub/secret.md", &md).unwrap_err();
        assert!(err.to_string().contains("outside its root"), "{err}");
    }

    #[test]
    fn a_file_changed_after_the_scan_is_not_sent() {
        let f = swap_fixture();
        let p = f.root.join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let md = std::fs::symlink_metadata(&p).unwrap();
        std::fs::write(&p, "hello, longer now").unwrap();
        let err = build(&f, "a.md", &md).unwrap_err();
        assert!(nugget_extract::is_file_changed(&err), "{err}");
    }

    #[test]
    fn target_coverage() {
        assert!(t(0, "a").covers_key("0:a"));
        assert!(t(0, "a").covers_key("0:a/b.md"));
        assert!(!t(0, "a").covers_key("0:ab.md"));
        assert!(!t(0, "a").covers_key("1:a"));
        assert!(t(1, "").covers_key("1:x/y"));
        assert!(!t(1, "").covers_key("10:x"));
        let known: KnownMap = ["0:a", "0:a b", "0:a/x", "0:a/y/z", "0:a-b", "0:b", "10:a"]
            .iter()
            .map(|k| (k.to_string(), None))
            .collect();
        let got: Vec<&str> = t(0, "a")
            .covered(&known)
            .into_iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(got, ["0:a", "0:a/x", "0:a/y/z"]);
        assert_eq!(t(1, "").covered(&known).len(), 0);
    }

    #[test]
    fn scope_drops_nested_targets() {
        let s = Scope::targets(vec![
            t(0, "a/b/c.md"),
            t(0, "a/b"),
            t(0, "a-b"),
            t(0, "a/b"),
            t(1, "x"),
            t(1, ""),
            t(0, "a/bc"),
        ]);
        assert_eq!(
            s,
            Scope::Targets(vec![t(0, "a-b"), t(0, "a/b"), t(0, "a/bc"), t(1, "")])
        );
    }

    /// A live event for a non-UTF-8 name reconciles that file (its key does not join back into
    /// the on-disk path, so scope matching must compare keys).
    #[cfg(target_os = "linux")]
    #[test]
    fn regression_targeted_reconcile_finds_non_utf8_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        let sub = root.join(OsStr::from_bytes(b"d\xff"));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(OsStr::from_bytes(b"n\xfe.md")), "x").unwrap();
        std::fs::write(sub.join("plain.md"), "y").unwrap();
        let cfg = Arc::new(Config::files(&json!({ "roots": [root] })).unwrap());
        let file_key = item_key(0, Path::new(OsStr::from_bytes(b"d\xff/n\xfe.md")));
        let (_, file_rel) = crate::key::parse_key(&file_key).unwrap();
        let dir_rel = rel_key(Path::new(OsStr::from_bytes(b"d\xff")));
        let mut known = KnownMap::new();
        known.insert(file_key.clone(), Some("0:0".into()));
        for target in [file_rel.to_string(), dir_rel] {
            let mut sent = Vec::new();
            let out = reconcile(
                &cfg,
                &Scope::targets(vec![t(0, &target)]),
                &known,
                false,
                &CancellationToken::new(),
                0,
                &mut |e| {
                    sent.push(format!("{e:?}"));
                    true
                },
            )
            .unwrap();
            assert!(out.deleted.is_empty(), "{target}: {sent:?}");
            assert!(
                out.found.contains_key(&file_key),
                "{target}: {:?}",
                out.found
            );
        }
    }
}
