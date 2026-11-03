use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::identity::{DIRS, LEGACY_DIRS};
use crate::migrate::{self, Move, Outcome, Rules};
use crate::{Error, Result};

/// Per-user directories. Override the root with `HEED_HOME` (or `NUGGET_HOME`; tests, portable
/// installs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

/// What the first [`Paths::discover`] of this process did about the pre-rename directories, for
/// the caller to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationReport {
    /// A `HEED_HOME`/`NUGGET_HOME` root: nothing to migrate.
    Rooted,
    /// Each directory moved (or not) and why.
    Dirs(Vec<(Move, std::result::Result<Outcome, String>)>),
    /// The old daemon is running on the old directories: they stay in use until it stops, and
    /// the move happens on a later launch.
    Deferred,
}

static REPORT: OnceLock<(Paths, MigrationReport)> = OnceLock::new();

impl Paths {
    /// The directories to use, after moving the pre-rename ones into place once per process (see
    /// [`crate::migrate`]).
    pub fn discover() -> Result<Self> {
        if let Some(home) = crate::env::var("HOME") {
            return Ok(Self::rooted(PathBuf::from(home)));
        }
        let project = |(q, o, a): (&str, &str, &str)| {
            directories::ProjectDirs::from(q, o, a).map(|d| Self {
                data: d.data_local_dir().to_path_buf(),
                config: d.config_dir().to_path_buf(),
                cache: d.cache_dir().to_path_buf(),
            })
        };
        let new =
            project(DIRS).ok_or_else(|| Error::Other("cannot resolve home directory".into()))?;
        let Some(legacy) = project(LEGACY_DIRS) else {
            return Ok(new);
        };
        let (paths, _) = REPORT.get_or_init(|| {
            let running = legacy_daemon_running(&legacy.data);
            migrate_to(&legacy, &new, running)
        });
        Ok(paths.clone())
    }

    /// What this process's first [`Paths::discover`] did; `None` before it ran.
    pub fn migration_report() -> Option<&'static MigrationReport> {
        REPORT.get().map(|(_, r)| r)
    }

    pub fn rooted(root: PathBuf) -> Self {
        Self {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
        }
    }

    pub fn db_file(&self) -> PathBuf {
        self.data.join("nugget.db")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.data.join("models")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
}

/// Moves `legacy`'s data and config directories to `new`'s (the cache is rebuilt, not moved) and
/// says which directories to use. While the old daemon runs (`legacy_running`), the old ones.
pub fn migrate_to(legacy: &Paths, new: &Paths, legacy_running: bool) -> (Paths, MigrationReport) {
    let mut moves = vec![Move {
        from: legacy.data.clone(),
        to: new.data.clone(),
    }];
    // On macOS data and config are one directory.
    if legacy.config != legacy.data {
        moves.push(Move {
            from: legacy.config.clone(),
            to: new.config.clone(),
        });
    }
    let pending = moves
        .iter()
        .any(|m| m.from.is_dir() && !m.to.join(migrate::MARKER).exists() && !m.to.exists());
    if legacy_running && pending {
        return (legacy.clone(), MigrationReport::Deferred);
    }
    let rules = Rules {
        skip: &is_runtime_state,
        link: &|p: &Path| p.starts_with("models"),
    };
    let done = moves
        .into_iter()
        .map(|m| {
            let r = migrate::migrate_dir(&m, &rules).map_err(|e| e.to_string());
            (m, r)
        })
        .collect::<Vec<_>>();
    // A data directory that failed to move keeps the old one in use (nothing is lost).
    let paths = if done
        .iter()
        .any(|(m, r)| m.from == legacy.data && r.is_err())
    {
        legacy.clone()
    } else {
        new.clone()
    };
    (paths, MigrationReport::Dirs(done))
}

/// A daemon's sockets, locks and pid files: not data, and wrong in a new place.
fn is_runtime_state(p: &Path) -> bool {
    p.starts_with("ipc")
        || p.starts_with("runtime")
        || p.extension()
            .is_some_and(|e| e == "lock" || e == "sock" || e == "pid")
}

/// Whether a daemon answers on the old data directory's socket.
fn legacy_daemon_running(data: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(data.join("ipc").join("nuggetd.sock")).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = data;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("heed-paths-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn split(r: &Path, tag: &str) -> Paths {
        Paths {
            data: r.join(tag).join("data"),
            config: r.join(tag).join("config"),
            cache: r.join(tag).join("cache"),
        }
    }

    #[test]
    fn upgrade_moves_data_and_config_and_skips_runtime_state() {
        let r = root("upgrade");
        let (old, new) = (split(&r, "nugget"), split(&r, "heed"));
        std::fs::create_dir_all(old.data.join("ipc")).unwrap();
        std::fs::write(old.data.join("ipc/nuggetd.lock"), b"").unwrap();
        std::fs::write(old.data.join("nugget.db"), b"db").unwrap();
        std::fs::create_dir_all(&old.config).unwrap();
        std::fs::write(old.config.join("config.toml"), b"[vault]\n").unwrap();
        let (paths, report) = migrate_to(&old, &new, false);
        assert_eq!(paths, new);
        let MigrationReport::Dirs(d) = report else {
            panic!()
        };
        assert!(
            d.iter()
                .all(|(_, r)| matches!(r, Ok(Outcome::Migrated { .. }))),
            "{d:?}"
        );
        assert_eq!(std::fs::read(new.data.join("nugget.db")).unwrap(), b"db");
        assert!(new.config.join("config.toml").exists());
        assert!(!new.data.join("ipc").exists());
        // A second launch: nothing to do.
        let (_, again) = migrate_to(&old, &new, false);
        let MigrationReport::Dirs(d) = again else {
            panic!()
        };
        assert!(
            d.iter().all(|(_, r)| *r == Ok(Outcome::AlreadyDone)),
            "{d:?}"
        );
    }

    #[test]
    fn a_running_old_daemon_keeps_the_old_directories_for_now() {
        let r = root("running");
        let (old, new) = (split(&r, "nugget"), split(&r, "heed"));
        std::fs::create_dir_all(&old.data).unwrap();
        std::fs::write(old.data.join("nugget.db"), b"db").unwrap();
        let (paths, report) = migrate_to(&old, &new, true);
        assert_eq!((paths, report), (old.clone(), MigrationReport::Deferred));
        assert!(!new.data.exists());
        // Once it is done, a running old daemon no longer matters.
        migrate_to(&old, &new, false);
        assert_eq!(migrate_to(&old, &new, true).0, new);
    }

    #[test]
    fn partial_earlier_migration_finishes_the_rest() {
        let r = root("partial");
        let (old, new) = (split(&r, "nugget"), split(&r, "heed"));
        std::fs::create_dir_all(&old.data).unwrap();
        std::fs::write(old.data.join("nugget.db"), b"db").unwrap();
        // The data moved on an earlier launch; config did not exist yet then.
        migrate_to(&old, &new, false);
        std::fs::create_dir_all(&old.config).unwrap();
        std::fs::write(old.config.join("config.toml"), b"x").unwrap();
        let (_, report) = migrate_to(&old, &new, false);
        let MigrationReport::Dirs(d) = report else {
            panic!()
        };
        assert_eq!(d[0].1, Ok(Outcome::AlreadyDone));
        assert!(
            matches!(d[1].1, Ok(Outcome::Migrated { files: 1, .. })),
            "{d:?}"
        );
    }
}
