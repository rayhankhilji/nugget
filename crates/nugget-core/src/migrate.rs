//! One-time move from the pre-rename locations (Nugget) to Heed's ([`crate::identity`]).
//!
//! **Directories** ([`migrate_dir`]): the old directory is copied next to the new one under a
//! temporary name, every file is checked against its source (size, and bytes unless hard-linked),
//! a marker ([`MARKER`]) is written inside, and only then is it renamed into place. The old
//! directory is never deleted or changed. A new directory with the marker is never migrated again;
//! one without it (a fresh install, or data the user made there) is kept as it is. A copy left by
//! an interrupted run is thrown away and redone. Two processes racing both copy; one rename wins.
//!
//! **Keychain** ([`keychain_get`]): entries move on first read. A read that misses under the new
//! service looks under the old one; a hit is copied to the new service, read back, and returned.
//! The old entry stays. Deleting removes both, so an old copy cannot come back.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Written inside a migrated directory: where it came from, when, and what was copied.
pub const MARKER: &str = ".heed-migrated.json";
/// Suffix of the temporary copy (`<name>.heed-migrating-<pid>`).
const TMP_INFIX: &str = ".heed-migrating-";
/// A temporary copy older than this belongs to a run that died.
const STALE_TMP: Duration = Duration::from_secs(3600);

/// One directory to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What [`migrate_dir`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing at the old location: a fresh install.
    Fresh,
    /// The marker is there: done on an earlier launch.
    AlreadyDone,
    /// Copied, checked and renamed into place.
    Migrated { files: u64, bytes: u64 },
    /// The new location already holds data that this migration did not put there: kept, and the
    /// old location left alone.
    KeptNew,
}

/// Which entries to leave out (sockets and locks of a daemon, pid files) and which may be
/// hard-linked instead of copied (immutable files such as content-addressed models). Paths are
/// relative to the directory being moved.
pub struct Rules<'a> {
    pub skip: &'a dyn Fn(&Path) -> bool,
    pub link: &'a dyn Fn(&Path) -> bool,
}

impl Default for Rules<'_> {
    fn default() -> Self {
        Self {
            skip: &|_| false,
            link: &|_| false,
        }
    }
}

struct Copied {
    rel: PathBuf,
    len: u64,
    linked: bool,
}

/// Moves `m.from` to `m.to` as described in the module docs.
pub fn migrate_dir(m: &Move, rules: &Rules<'_>) -> io::Result<Outcome> {
    if m.to.join(MARKER).exists() {
        return Ok(Outcome::AlreadyDone);
    }
    if !m.from.is_dir() {
        return Ok(Outcome::Fresh);
    }
    if m.to.exists() {
        // An empty directory (made by something that only looked) holds nothing to keep.
        if m.to.is_dir() && fs::read_dir(&m.to)?.next().is_none() {
            fs::remove_dir(&m.to)?;
        } else {
            return Ok(Outcome::KeptNew);
        }
    }
    let parent =
        m.to.parent()
            .ok_or_else(|| io::Error::other("target has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let name =
        m.to.file_name()
            .ok_or_else(|| io::Error::other("target has no name"))?
            .to_string_lossy()
            .into_owned();
    clear_stale(parent, &name)?;
    let tmp = parent.join(format!("{name}{TMP_INFIX}{}", std::process::id()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    let result = copy_and_check(&m.from, &tmp, rules).and_then(|copied| {
        let bytes = copied.iter().map(|c| c.len).sum();
        let files = copied.len() as u64;
        let marker = format!(
            "{{\"from\":{},\"files\":{files},\"bytes\":{bytes},\"at\":{}}}\n",
            json_str(&m.from.to_string_lossy()),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        );
        fs::write(tmp.join(MARKER), marker)?;
        Ok((files, bytes))
    });
    let (files, bytes) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    match fs::rename(&tmp, &m.to) {
        Ok(()) => Ok(Outcome::Migrated { files, bytes }),
        // Another process got there first.
        Err(_) if m.to.exists() => {
            let _ = fs::remove_dir_all(&tmp);
            Ok(if m.to.join(MARKER).exists() {
                Outcome::AlreadyDone
            } else {
                Outcome::KeptNew
            })
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// Removes temporary copies of `name` in `parent` left by runs that died.
fn clear_stale(parent: &Path, name: &str) -> io::Result<()> {
    let prefix = format!("{name}{TMP_INFIX}");
    for e in fs::read_dir(parent)? {
        let e = e?;
        if !e.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let old = e
            .metadata()?
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age > STALE_TMP);
        if old {
            fs::remove_dir_all(e.path())?;
        }
    }
    Ok(())
}

fn copy_and_check(from: &Path, to: &Path, rules: &Rules<'_>) -> io::Result<Vec<Copied>> {
    let mut copied = Vec::new();
    copy_tree(from, to, Path::new(""), rules, &mut copied)?;
    for c in &copied {
        let (src, dst) = (from.join(&c.rel), to.join(&c.rel));
        let len = fs::metadata(&dst)?.len();
        if len != c.len || fs::metadata(&src)?.len() != c.len {
            return Err(io::Error::other(format!(
                "{}: copy is {len} bytes, source {}",
                c.rel.display(),
                c.len
            )));
        }
        if !c.linked && !same_bytes(&src, &dst)? {
            return Err(io::Error::other(format!(
                "{}: copy differs from its source",
                c.rel.display()
            )));
        }
    }
    Ok(copied)
}

fn copy_tree(
    from: &Path,
    to: &Path,
    rel: &Path,
    rules: &Rules<'_>,
    out: &mut Vec<Copied>,
) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let r = rel.join(e.file_name());
        if (rules.skip)(&r) {
            continue;
        }
        let kind = e.file_type()?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if kind.is_dir() {
            copy_tree(&src, &dst, &r, rules, out)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else if kind.is_file() {
            let linked = (rules.link)(&r) && fs::hard_link(&src, &dst).is_ok();
            let len = if linked {
                fs::metadata(&dst)?.len()
            } else {
                let n = fs::copy(&src, &dst)?;
                fs::File::open(&dst)?.sync_all()?;
                n
            };
            out.push(Copied {
                rel: r,
                len,
                linked,
            });
        }
        // Sockets, FIFOs and devices are a running process's, not data.
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (fs::File::open(a)?, fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The OS keychain as the migration sees it (tests use [`MemoryKeychain`]).
pub trait Keychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
}

/// Reads `account` under `service`, moving it from `legacy` on first read (see the module docs).
/// A copy that fails is not an error: the old entry is still returned, and the next read tries
/// again.
pub fn keychain_get(
    k: &dyn Keychain,
    service: &str,
    legacy: &str,
    account: &str,
) -> Result<Option<String>, String> {
    keychain_get_from(k, service, account, &[(legacy, account)])
}

/// [`keychain_get`] where the old entry may have had another account name too (one derived from
/// a path that moved): `legacy` lists the old `(service, account)` places, first match wins.
pub fn keychain_get_from(
    k: &dyn Keychain,
    service: &str,
    account: &str,
    legacy: &[(&str, &str)],
) -> Result<Option<String>, String> {
    if let Some(v) = k.get(service, account)? {
        return Ok(Some(v));
    }
    for (old_service, old_account) in legacy {
        let Some(v) = k.get(old_service, old_account)? else {
            continue;
        };
        if k.set(service, account, &v).is_ok()
            && k.get(service, account).ok().flatten().as_ref() != Some(&v)
        {
            // Unreadable copy: leave the old entry as the one in use.
            let _ = k.delete(service, account);
        }
        return Ok(Some(v));
    }
    Ok(None)
}

/// Where a migrated directory came from (its [`MARKER`]), if it was migrated.
pub fn migrated_from(dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dir.join(MARKER)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("from")?.as_str().map(PathBuf::from)
}

/// Deletes `account` under both services, so a later read cannot bring the old copy back.
pub fn keychain_delete(
    k: &dyn Keychain,
    service: &str,
    legacy: &str,
    account: &str,
) -> Result<(), String> {
    k.delete(service, account)?;
    k.delete(legacy, account)
}

/// An in-memory keychain (tests). Cloning shares the entries.
#[derive(Debug, Clone, Default)]
pub struct MemoryKeychain(
    std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<(String, String), String>>>,
);

impl MemoryKeychain {
    pub fn entries(&self) -> Vec<((String, String), String)> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

impl Keychain for MemoryKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Ok(m.get(&(service.to_owned(), account.to_owned())).cloned())
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.insert((service.to_owned(), account.to_owned()), value.to_owned());
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "heed-migrate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn legacy_tree(root: &Path) -> Move {
        let from = root.join("old");
        fs::create_dir_all(from.join("models")).unwrap();
        fs::create_dir_all(from.join("logs")).unwrap();
        fs::write(from.join("nugget.db"), b"encrypted store").unwrap();
        fs::write(from.join("models/abc.gguf"), vec![7u8; 200_000]).unwrap();
        fs::write(from.join("logs/daemon.log"), b"line\n").unwrap();
        fs::write(from.join("nuggetd.lock"), b"").unwrap();
        Move {
            from,
            to: root.join("new"),
        }
    }

    fn rules() -> Rules<'static> {
        Rules {
            skip: &|p| p.extension().is_some_and(|e| e == "lock"),
            link: &|p| p.starts_with("models"),
        }
    }

    #[test]
    fn fresh_install_has_nothing_to_move() {
        let root = tmp();
        let m = Move {
            from: root.join("old"),
            to: root.join("new"),
        };
        assert_eq!(migrate_dir(&m, &rules()).unwrap(), Outcome::Fresh);
        assert!(!m.to.exists());
    }

    #[test]
    fn upgrade_copies_checks_marks_and_keeps_the_old_data() {
        let root = tmp();
        let m = legacy_tree(&root);
        let out = migrate_dir(&m, &rules()).unwrap();
        assert_eq!(
            out,
            Outcome::Migrated {
                files: 3,
                bytes: 200_000 + 15 + 5
            }
        );
        assert_eq!(
            fs::read(m.to.join("nugget.db")).unwrap(),
            b"encrypted store"
        );
        assert_eq!(
            fs::read(m.to.join("models/abc.gguf")).unwrap().len(),
            200_000
        );
        assert!(!m.to.join("nuggetd.lock").exists(), "locks are not data");
        let marker = fs::read_to_string(m.to.join(MARKER)).unwrap();
        assert!(marker.contains("\"files\":3"), "{marker}");
        // The old data is untouched.
        assert_eq!(
            fs::read(m.from.join("nugget.db")).unwrap(),
            b"encrypted store"
        );
        assert!(m.from.join("nuggetd.lock").exists());
        // No temporary copy left behind.
        let left: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn a_second_launch_does_not_migrate_again() {
        let root = tmp();
        let m = legacy_tree(&root);
        migrate_dir(&m, &rules()).unwrap();
        fs::write(m.to.join("nugget.db"), b"newer, written by Heed").unwrap();
        fs::write(m.from.join("nugget.db"), b"changed by the old app").unwrap();
        assert_eq!(migrate_dir(&m, &rules()).unwrap(), Outcome::AlreadyDone);
        assert_eq!(
            fs::read(m.to.join("nugget.db")).unwrap(),
            b"newer, written by Heed"
        );
    }

    #[test]
    fn an_interrupted_earlier_run_is_redone() {
        let root = tmp();
        let m = legacy_tree(&root);
        // A half-made copy from a run that died (same pid: this process's own leftover).
        let leftover = root.join(format!("new{TMP_INFIX}{}", std::process::id()));
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("nugget.db"), b"trunc").unwrap();
        assert!(matches!(
            migrate_dir(&m, &rules()).unwrap(),
            Outcome::Migrated { .. }
        ));
        assert_eq!(
            fs::read(m.to.join("nugget.db")).unwrap(),
            b"encrypted store"
        );
        assert!(!leftover.exists());
    }

    #[test]
    fn data_already_at_the_new_location_is_kept() {
        let root = tmp();
        let m = legacy_tree(&root);
        fs::create_dir_all(&m.to).unwrap();
        fs::write(m.to.join("nugget.db"), b"made by Heed").unwrap();
        assert_eq!(migrate_dir(&m, &rules()).unwrap(), Outcome::KeptNew);
        assert_eq!(fs::read(m.to.join("nugget.db")).unwrap(), b"made by Heed");
        // An empty new directory, though, is not data.
        let root2 = tmp().join("b");
        let m2 = legacy_tree(&root2);
        fs::create_dir_all(&m2.to).unwrap();
        assert!(matches!(
            migrate_dir(&m2, &rules()).unwrap(),
            Outcome::Migrated { .. }
        ));
    }

    #[test]
    fn keychain_entries_move_on_first_read_and_stay_in_the_old_place() {
        let k = MemoryKeychain::default();
        k.set("old", "store-key", "K1").unwrap();
        assert_eq!(
            keychain_get(&k, "new", "old", "store-key")
                .unwrap()
                .as_deref(),
            Some("K1")
        );
        assert_eq!(k.get("new", "store-key").unwrap().as_deref(), Some("K1"));
        assert_eq!(
            k.get("old", "store-key").unwrap().as_deref(),
            Some("K1"),
            "never deleted"
        );
        // Nothing anywhere: a fresh install.
        assert_eq!(keychain_get(&k, "new", "old", "provider:x").unwrap(), None);
    }

    #[test]
    fn marker_names_the_old_directory_and_old_accounts_are_found() {
        let root = tmp();
        let m = legacy_tree(&root);
        migrate_dir(&m, &rules()).unwrap();
        assert_eq!(migrated_from(&m.to), Some(m.from.clone()));
        assert_eq!(migrated_from(&m.from), None);
        let k = MemoryKeychain::default();
        k.set("old", "store-key-oldpath", "K").unwrap();
        let got = keychain_get_from(
            &k,
            "new",
            "store-key-newpath",
            &[("old", "store-key-newpath"), ("old", "store-key-oldpath")],
        )
        .unwrap();
        assert_eq!(got.as_deref(), Some("K"));
        assert_eq!(
            k.get("new", "store-key-newpath").unwrap().as_deref(),
            Some("K")
        );
    }

    #[test]
    fn keychain_entry_in_both_places_uses_the_new_one() {
        let k = MemoryKeychain::default();
        k.set("old", "cloud-session", "stale").unwrap();
        k.set("new", "cloud-session", "current").unwrap();
        assert_eq!(
            keychain_get(&k, "new", "old", "cloud-session")
                .unwrap()
                .as_deref(),
            Some("current")
        );
        assert_eq!(
            k.get("old", "cloud-session").unwrap().as_deref(),
            Some("stale")
        );
        // Deleting removes both, so the old copy cannot come back on the next read.
        keychain_delete(&k, "new", "old", "cloud-session").unwrap();
        assert_eq!(
            keychain_get(&k, "new", "old", "cloud-session").unwrap(),
            None
        );
    }
}
