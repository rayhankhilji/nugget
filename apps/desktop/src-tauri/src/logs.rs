//! the desktop shell's own log file.
//!
//! Plain-text lines in `<data>/logs/nugget-desktop.<date>.log`, next to the daemon's
//! `nuggetd.<date>.log`: one folder holds everything a bug report needs, it follows
//! `NUGGET_HOME` the same way the daemon does, and it is known before Tauri is built (Tauri's
//! `app_log_dir` needs a running app, and the first lines matter most). A new file each day, the
//! last [`MAX_LOG_FILES`] kept; at the default `info` level the shell writes a few lines an hour.
//! `NUGGET_LOG` filters as before. Lines also go to stdout (seen in a terminal or `npm run tauri
//! dev`; release builds on Windows have none).

use std::path::{Path, PathBuf};

use nugget_core::paths::Paths;
use serde::Serialize;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// File name prefix: `nugget-desktop.<YYYY-MM-DD>.log` (the daemon's files start `nuggetd`, so
/// neither prunes the other's).
pub const PREFIX: &str = "nugget-desktop";
/// Daily files kept before the oldest is deleted.
const MAX_LOG_FILES: usize = 7;

/// `<data>/logs`, shared with `nuggetd` (`nuggetd::logging::log_dir`).
pub fn log_dir(paths: &Paths) -> PathBuf {
    paths.data.join("logs")
}

/// Where to look, for error messages, Status and the troubleshooting docs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LogPaths {
    /// The folder with both logs.
    pub dir: String,
    /// The shell's files, as a pattern (`…/nugget-desktop.<date>.log`).
    pub shell: String,
    /// The daemon's files (`…/nuggetd.<date>.log`).
    pub daemon: String,
}

impl LogPaths {
    pub fn new(dir: &Path) -> Self {
        let at = |name: &str| dir.join(name).display().to_string();
        Self {
            dir: dir.display().to_string(),
            shell: at(&format!("{PREFIX}.<date>.log")),
            daemon: at("nuggetd.<date>.log"),
        }
    }

    /// The sentence user-facing errors end with.
    pub fn hint(&self) -> String {
        format!(
            "The background service's log is {}, and the app's is {}.",
            self.daemon, self.shell
        )
    }
}

/// The log locations for this user (`None` when no data directory can be found).
pub fn paths() -> Option<LogPaths> {
    Paths::discover().ok().map(|p| LogPaths::new(&log_dir(&p)))
}

/// " " + [`LogPaths::hint`], or nothing when there is no data directory.
pub fn hint_suffix() -> String {
    paths()
        .map(|p| format!(" {}", p.hint()))
        .unwrap_or_default()
}

fn filter() -> EnvFilter {
    EnvFilter::try_from_env("NUGGET_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Install the global subscriber: stdout as before, plus the daily file when the log folder can
/// be created. Keep the guard until the process ends, so buffered lines reach the file.
pub fn init() -> Option<WorkerGuard> {
    let file = Paths::discover()
        .map_err(|e| e.to_string())
        .and_then(|p| open(&log_dir(&p)));
    let (writer, guard, error) = match file {
        Ok(appender) => {
            let (w, g) = tracing_appender::non_blocking(appender);
            (Some(w), Some(g), None)
        }
        Err(e) => (None, None, Some(e)),
    };
    let file_layer = writer.map(|w| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(w)
    });
    let _ = tracing_subscriber::registry()
        .with(filter())
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .try_init();
    match (&error, paths()) {
        (Some(e), _) => tracing::warn!(error = %e, "no log file; logging to stdout only"),
        (None, Some(p)) => tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            log = %p.shell,
            "nugget-desktop starting"
        ),
        (None, None) => {}
    }
    guard
}

/// The rolling appender in `dir` (created private on Unix, like the daemon's).
fn open(dir: &Path) -> Result<RollingFileAppender, String> {
    create_private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(PREFIX)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(|e| format!("cannot open a log file in {}: {e}", dir.display()))
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_appender_writes_a_dated_file_in_a_private_folder() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("nugget-desktop-logs-{}", std::process::id()));
        let dir = tmp.join("logs");
        let mut appender = open(&dir).unwrap();
        appender.write_all(b"hello\n").unwrap();
        appender.flush().unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            names[0].starts_with("nugget-desktop.") && names[0].ends_with(".log"),
            "{names:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
        let _ = std::fs::remove_dir_all(tmp);
    }
}
