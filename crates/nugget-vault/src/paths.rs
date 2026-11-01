//! Page paths and filesystem safety.
//!
//! Every page lives at `<Kind>/<Name> (<short id>).md`. The name part is sanitised so the path is
//! valid on Windows, macOS and Linux; the short id keeps it unique and lets links survive renames.
//!
//! All writes go through [`SafeFs`]: paths are relative to the canonical vault root, may only use
//! normal components, never pass through a symlink (directories are checked one by one, and a
//! symlinked page is refused), and are written to a temp file in the same directory that is then
//! renamed over the target.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nugget_core::ids::EntityId;
use nugget_core::{Error, Result};
use nugget_graph::EntityKind;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Hidden directory inside the vault for merge bases, conflict sidecars and the page manifest.
pub const META_DIR: &str = ".nugget";
/// Suffix of the temp files used for atomic writes (the watcher ignores them).
pub const TMP_SUFFIX: &str = ".nugget-tmp";
/// Largest page the vault reads back (anything bigger is not ours and is left alone).
pub const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Directory name for a kind, e.g. `Person`.
pub fn kind_dir(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Person => "Person",
        EntityKind::Organization => "Organization",
        EntityKind::Project => "Project",
        EntityKind::Commitment => "Commitment",
        EntityKind::Preference => "Preference",
        EntityKind::Event => "Event",
        EntityKind::Place => "Place",
        EntityKind::Topic => "Topic",
    }
}

/// A short, file-name-safe, unique form of an entity id.
///
/// Ids made only of `[a-z0-9_-]` and at most 24 characters long are used as they are (so
/// `ent_000042` stays readable). Anything else becomes `~` plus 12 hex digits of its SHA-256; the
/// `~` cannot occur in a verbatim id, so the two forms never collide.
pub fn short_id(id: &EntityId) -> String {
    let s = id.as_str();
    let plain = !s.is_empty()
        && s.len() <= 24
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if plain {
        s.to_owned()
    } else {
        let h = Sha256::digest(s.as_bytes());
        let mut out = String::from("~");
        for b in &h[..6] {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

/// Windows device names that cannot be used as a file name (with or without an extension).
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
];

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end();
    let upper = stem.to_uppercase();
    RESERVED.contains(&upper.as_str())
}

/// Invisible or direction-changing characters that make names look like something else.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// Make `name` safe as the name part of a file name on every OS and as an Obsidian link target.
///
/// NFC-normalised; control and invisible characters dropped; path separators, characters Windows
/// forbids (`<>:"/\|?*`) and characters that break Obsidian links (`#^[]`) replaced by `-`;
/// whitespace collapsed; leading and trailing dots and spaces trimmed (no hidden files, nothing
/// Windows silently strips, no `..`); at most `max_chars` characters and 160 bytes; Windows device
/// names prefixed with `_`. Never empty.
pub fn sanitize_name(name: &str, max_chars: usize) -> String {
    let max_chars = max_chars.clamp(8, 200);
    let mut out = String::with_capacity(name.len());
    let mut space = false;
    for c in name.nfc() {
        let c = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => '-',
            c if c.is_control() || is_invisible(c) => continue,
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if c == ' ' {
            if !space && !out.is_empty() {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    let trim = |s: &str| s.trim_matches(|c: char| c == '.' || c == ' ').to_owned();
    let mut out = trim(&out);
    if out.chars().count() > max_chars || out.len() > 160 {
        let mut cut = String::new();
        for c in out.chars() {
            if cut.chars().count() + 1 > max_chars || cut.len() + c.len_utf8() > 160 {
                break;
            }
            cut.push(c);
        }
        out = trim(&cut);
    }
    if out.is_empty() {
        out = "Untitled".into();
    }
    if is_reserved(&out) {
        out.insert(0, '_');
    }
    out
}

/// The page's path stem relative to the vault root, e.g. `Person/Sam Lee (ent_000001)`. This is
/// also the link target.
pub fn page_stem(kind: EntityKind, name: &str, id: &EntityId, max_chars: usize) -> String {
    format!(
        "{}/{} ({})",
        kind_dir(kind),
        sanitize_name(name, max_chars),
        short_id(id)
    )
}

/// `page_stem` plus `.md`.
pub fn page_rel_path(kind: EntityKind, name: &str, id: &EntityId, max_chars: usize) -> String {
    format!("{}.md", page_stem(kind, name, id, max_chars))
}

/// Lowercase hex SHA-256 (write fingerprints).
pub fn fingerprint(text: &str) -> String {
    let h = Sha256::digest(text.as_bytes());
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Split a vault-relative path (always `/`-separated) into checked components.
fn components(rel: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = rel.split('/').collect();
    if rel.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(Error::Invalid(format!("bad vault path {rel:?}")));
    }
    for p in &parts {
        let mut comps = Path::new(p).components();
        let ok = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
        if !ok || p.contains('\\') || p.contains(':') || p.contains('\0') {
            return Err(Error::Invalid(format!("unsafe vault path {rel:?}")));
        }
    }
    Ok(parts)
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filesystem access confined to the vault root.
#[derive(Debug, Clone)]
pub struct SafeFs {
    root: PathBuf,
}

impl SafeFs {
    /// Create the root if needed and canonicalise it.
    pub fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(Error::Invalid(format!(
                "vault root {} is not a directory",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve `rel` to an absolute path, checking every existing component is a real directory
    /// (not a symlink). With `create`, missing directories are created. Returns `None` when a
    /// parent directory is missing and `create` is false.
    fn resolve(&self, rel: &str, create: bool) -> Result<Option<PathBuf>> {
        let parts = components(rel)?;
        let (file, dirs) = parts.split_last().expect("non-empty");
        let mut cur = self.root.clone();
        for d in dirs {
            cur.push(d);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::Invalid(format!(
                        "refusing to follow symlink {}",
                        cur.display()
                    )));
                }
                Ok(m) if !m.is_dir() => {
                    return Err(Error::Invalid(format!(
                        "{} is not a directory",
                        cur.display()
                    )));
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    if !create {
                        return Ok(None);
                    }
                    match fs::create_dir(&cur) {
                        Ok(()) => {}
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e.into()),
                    }
                    if fs::symlink_metadata(&cur)?.file_type().is_symlink() {
                        return Err(Error::Invalid(format!(
                            "refusing to follow symlink {}",
                            cur.display()
                        )));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        // Belt and braces: the parent must still canonicalise to somewhere under the root.
        if !dirs.is_empty() || create {
            let parent = fs::canonicalize(&cur)?;
            if !parent.starts_with(&self.root) {
                return Err(Error::Invalid(format!("{rel} escapes the vault root")));
            }
        }
        cur.push(file);
        Ok(Some(cur))
    }

    /// Absolute path for `rel` without touching the disk beyond component checks.
    pub fn abs(&self, rel: &str) -> Result<PathBuf> {
        components(rel)?;
        Ok(self.root.join(rel))
    }

    /// Vault-relative `/`-separated form of an absolute or relative path under the root.
    pub fn rel_of(&self, path: &Path) -> Option<String> {
        let p = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .ok()
                .map(Path::to_path_buf)
                .or_else(|| {
                    let canon = path
                        .parent()
                        .and_then(|d| fs::canonicalize(d).ok())?
                        .join(path.file_name()?);
                    canon.strip_prefix(&self.root).ok().map(Path::to_path_buf)
                })?
        } else {
            path.to_path_buf()
        };
        let mut parts = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str()?.to_owned()),
                Component::CurDir => {}
                _ => return None,
            }
        }
        let rel = parts.join("/");
        components(&rel).ok()?;
        Some(rel)
    }

    /// Read a page. `Ok(None)` if it does not exist. Symlinks, non-files, oversized files and
    /// invalid UTF-8 are errors (the caller must then leave the page alone).
    pub fn read(&self, rel: &str) -> Result<Option<String>> {
        let Some(path) = self.resolve(rel, false)? else {
            return Ok(None);
        };
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(Error::Invalid(format!(
                "refusing to read symlinked page {rel}"
            )));
        }
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{rel} is not a file")));
        }
        if meta.len() > MAX_PAGE_BYTES {
            return Err(Error::Invalid(format!("{rel} is too large")));
        }
        let mut buf = Vec::new();
        fs::File::open(&path)?
            .take(MAX_PAGE_BYTES + 1)
            .read_to_end(&mut buf)?;
        String::from_utf8(buf)
            .map(Some)
            .map_err(|_| Error::Invalid(format!("{rel} is not valid UTF-8")))
    }

    /// Whether anything (file, directory or symlink) exists at `rel`. Errors (such as a symlinked
    /// parent directory) count as existing, so callers go on to fail safely.
    pub fn entry_exists(&self, rel: &str) -> bool {
        match self.resolve(rel, false) {
            Ok(Some(p)) => fs::symlink_metadata(p).is_ok(),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    /// Whether a regular (non-symlink) file exists at `rel`.
    pub fn is_file(&self, rel: &str) -> bool {
        match self.resolve(rel, false) {
            Ok(Some(p)) => fs::symlink_metadata(p).is_ok_and(|m| m.is_file()),
            _ => false,
        }
    }

    /// Remove temp files ([`TMP_SUFFIX`]) that a process killed mid-write left behind (they are
    /// never pages, but would pile up). Files of this process are kept: its writes may be in
    /// flight. Symlinks are never followed. Best effort; returns how many were removed.
    pub fn sweep_temp_files(&self) -> usize {
        let own = format!(".{}.", std::process::id());
        let mut removed = 0;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let name = e.file_name();
                let name = name.to_string_lossy();
                if ft.is_dir() {
                    stack.push(e.path());
                } else if ft.is_file()
                    && let Some(stem) = name.strip_suffix(TMP_SUFFIX)
                    && name.starts_with('.')
                    // `.<name>.<pid>.<n>`: keep this process's own.
                    && !stem
                        .rsplit_once('.')
                        .is_some_and(|(head, _)| format!("{head}.").ends_with(&own))
                    && fs::remove_file(e.path()).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Atomically replace `rel` with `text`: temp file in the same directory, fsync, rename.
    pub fn write(&self, rel: &str, text: &str) -> Result<()> {
        #[cfg(test)]
        if fail::should_fail(rel) {
            return Err(Error::Storage(format!("injected write failure: {rel}")));
        }
        let path = self.resolve(rel, true)?.expect("created");
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "refusing to overwrite symlink {rel}"
                )));
            }
            Ok(m) if !m.is_file() => {
                return Err(Error::Invalid(format!("{rel} is not a file")));
            }
            _ => {}
        }
        let dir = path.parent().expect("has parent");
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("page");
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.{}.{n}{TMP_SUFFIX}", std::process::id()));
        let res = (|| -> std::io::Result<()> {
            // `create_new` fails on any existing entry, including a planted symlink.
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = res {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    /// Remove a regular file. Missing files are fine; symlinks and directories are refused.
    pub fn remove(&self, rel: &str) -> Result<()> {
        #[cfg(test)]
        if fail::should_fail(rel) {
            return Err(Error::Storage(format!("injected removal failure: {rel}")));
        }
        let Some(path) = self.resolve(rel, false)? else {
            return Ok(());
        };
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => Ok(fs::remove_file(&path)?),
            Ok(_) => Err(Error::Invalid(format!("refusing to remove {rel}"))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Remove empty directories left behind under the kind folders (best effort).
    pub fn prune_empty_dir(&self, rel_dir: &str) {
        if let Ok(Some(p)) = self.resolve(&format!("{rel_dir}/x"), false)
            && let Some(dir) = p.parent()
            && fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        {
            let _ = fs::remove_dir(dir); // fails (and is ignored) unless empty
        }
    }
}

/// Failure injection for tests: the next write or removal of a given path fails (once), standing
/// in for a crash or a full disk at that point.
#[cfg(test)]
pub(crate) mod fail {
    use std::cell::RefCell;

    thread_local! {
        static PATTERNS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub fn next_change_to(rel: &str) {
        PATTERNS.with(|p| p.borrow_mut().push(rel.to_owned()));
    }

    pub fn should_fail(rel: &str) -> bool {
        PATTERNS.with(|p| {
            let mut p = p.borrow_mut();
            match p.iter().position(|pat| pat == rel) {
                Some(i) => {
                    p.remove(i);
                    true
                }
                None => false,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_removes_dead_writers_temp_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let fs_ = SafeFs::new(dir.path()).unwrap();
        fs_.write("Person/A (e1).md", "page").unwrap();
        let person = fs_.root().join("Person");
        let dead = person.join(format!(".A (e1).md.999999999.3{TMP_SUFFIX}"));
        let mine = person.join(format!(".A (e1).md.{}.4{TMP_SUFFIX}", std::process::id()));
        std::fs::write(&dead, "half").unwrap();
        std::fs::write(&mine, "in flight").unwrap();
        assert_eq!(fs_.sweep_temp_files(), 1);
        assert!(!dead.exists() && mine.exists());
        assert_eq!(
            fs_.read("Person/A (e1).md").unwrap().as_deref(),
            Some("page")
        );
    }

    #[test]
    fn reserved_names_get_prefixed() {
        for n in ["CON", "con", "Nul", "COM1", "lpt9", "aux.txt", "CON .x"] {
            let s = sanitize_name(n, 80);
            assert!(s.starts_with('_'), "{n} -> {s}");
        }
        assert_eq!(sanitize_name("Console", 80), "Console");
        assert_eq!(sanitize_name("COM10", 80), "COM10");
    }

    #[test]
    fn separators_dots_and_controls() {
        assert_eq!(sanitize_name("..", 80), "Untitled");
        assert_eq!(sanitize_name("../../etc/passwd", 80), "-..-etc-passwd");
        assert_eq!(sanitize_name("a/b\\c:d", 80), "a-b-c-d");
        assert_eq!(sanitize_name("  .hidden.  ", 80), "hidden");
        assert_eq!(sanitize_name("x\u{0}y\u{202E}z\n w", 80), "xyz w");
        assert_eq!(sanitize_name("[[Link|x]]#^", 80), "--Link-x----");
        assert_eq!(sanitize_name("", 80), "Untitled");
        assert_eq!(sanitize_name("   ", 80), "Untitled");
    }

    #[test]
    fn nfc_and_length_cap() {
        let decomposed = "Re\u{301}sume\u{301}";
        assert_eq!(sanitize_name(decomposed, 80), "Résumé");
        let long = "é".repeat(500);
        let s = sanitize_name(&long, 80);
        assert_eq!(s.chars().count(), 80);
        let wide = "😀".repeat(100);
        assert!(sanitize_name(&wide, 200).len() <= 160);
        let cut_on_dot = format!("{}.{}", "a".repeat(9), "b".repeat(20));
        assert_eq!(sanitize_name(&cut_on_dot, 10), "aaaaaaaaa");
    }

    #[test]
    fn short_ids_are_safe_and_distinct() {
        assert_eq!(short_id(&EntityId::new("ent_000001")), "ent_000001");
        let a = short_id(&EntityId::new("01HXYZ/../x"));
        let b = short_id(&EntityId::new("01HXYZ/../y"));
        assert!(a.starts_with('~') && a.len() == 13);
        assert_ne!(a, b);
        assert!(short_id(&EntityId::new("ENT_1")).starts_with('~'));
    }

    #[test]
    fn component_checks() {
        for bad in [
            "", "/abs", "a//b", "../x", "a/../b", "./x", "a\\b", "c:x", "a/",
        ] {
            assert!(components(bad).is_err(), "{bad}");
        }
        assert!(components("Person/Sam (ent_1).md").is_ok());
    }
}
