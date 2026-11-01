//! Logging: JSON lines to a daily-rolling file under `Paths::data/logs`, plus human-readable
//! stderr when attached to a terminal. The level comes from `NUGGET_LOG` (or `RUST_LOG`) and
//! defaults to `info`. Whatever the level, the MCP SDK is capped at `info` ([`content_cap`]).

use std::io::IsTerminal;
use std::path::PathBuf;

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Log files kept before the oldest is deleted.
const MAX_LOG_FILES: usize = 14;

pub fn log_dir(paths: &Paths) -> PathBuf {
    paths.data.join("logs")
}

fn filter() -> EnvFilter {
    let spec = std::env::var("NUGGET_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_default();
    filter_for(&spec)
}

fn filter_for(spec: &str) -> EnvFilter {
    EnvFilter::try_new(if spec.trim().is_empty() { "info" } else { spec })
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// A second filter, applied after `NUGGET_LOG`'s, that no directive can loosen: the MCP SDK
/// logs whole tool-call requests and results (queries, passage text) at `debug`, so its targets
/// stop at `info`. Our own MCP code logs metadata only.
fn content_cap() -> Targets {
    Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_target("rmcp", LevelFilter::INFO)
}

/// Install the daemon's global subscriber. Keep the guard alive until exit so buffered lines are
/// flushed.
pub fn init_daemon(paths: &Paths) -> Result<WorkerGuard> {
    let dir = log_dir(paths);
    crate::fsutil::create_private_dir(&dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("nuggetd")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(&dir)
        .map_err(|e| Error::Other(format!("cannot open log dir {}: {e}", dir.display())))?;
    let (file_writer, guard) = tracing_appender::non_blocking(LineSafe::new(appender));
    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_current_span(false)
        .with_writer(file_writer);
    let stderr_layer = std::io::stderr().is_terminal().then(|| {
        tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(std::io::stderr)
    });
    tracing_subscriber::registry()
        .with(filter())
        .with(content_cap())
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| Error::Other(format!("cannot install logger: {e}")))?;
    Ok(guard)
}

/// A log file writer that keeps lines whole across failed writes: a write that fails part way
/// (the disk is full) leaves a partial line in the file, so the next write starts with a line
/// break and later lines stay parseable once space is back (QA GG6-25). Each write is one
/// whole event from the non-blocking worker.
struct LineSafe<W> {
    inner: W,
    /// The last write failed: the file may end inside a line.
    broken: bool,
}

impl<W> LineSafe<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            broken: false,
        }
    }
}

impl<W: std::io::Write> std::io::Write for LineSafe<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.broken {
            self.inner.write_all(b"\n")?;
            self.broken = false;
        }
        match self.inner.write_all(buf) {
            Ok(()) => Ok(buf.len()),
            Err(e) => {
                self.broken = true;
                Err(e)
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Warnings-and-up to stderr, for one-shot CLI commands.
pub fn init_cli() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("NUGGET_LOG")
                .ok()
                .and_then(|s| EnvFilter::try_new(s).ok())
                .unwrap_or_else(|| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .finish()
        .with(content_cap())
        .try_init();
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Accepts `room` bytes, then fails like a full disk (keeping what fit).
    struct FullDisk {
        out: Vec<u8>,
        room: usize,
    }

    impl Write for FullDisk {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.room - self.out.len().min(self.room));
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "full"));
            }
            self.out.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_does_not_glue_the_next_line_to_a_partial_one() {
        let mut w = LineSafe::new(FullDisk {
            out: Vec::new(),
            room: 15,
        });
        w.write_all(b"{\"a\":1}\n").unwrap();
        assert!(w.write_all(b"{\"b\":22222}\n").is_err());
        // Still full: nothing more is lost than the line being written.
        assert!(w.write_all(b"{\"c\":3}\n").is_err());
        w.inner.room = 1000;
        w.write_all(b"{\"d\":4}\n").unwrap();
        let text = String::from_utf8(w.inner.out.clone()).unwrap();
        assert_eq!(text, "{\"a\":1}\n{\"b\":22\n{\"d\":4}\n");
        let last = text.lines().last().unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(last).is_ok());
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn debug_level_never_logs_mcp_sdk_content() {
        for spec in ["debug", "trace", "debug,rmcp=trace", "rmcp::service=debug"] {
            let buf = Buf::default();
            let w = buf.clone();
            let subscriber = tracing_subscriber::registry()
                .with(filter_for(spec))
                .with(content_cap())
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_writer(move || w.clone()),
                );
            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!(target: "rmcp::service", "received request GG8CANARY");
                tracing::info!(target: "rmcp::service", "serve started");
                tracing::debug!(target: "nuggetd::mcp", "tool call");
            });
            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            assert!(!out.contains("GG8CANARY"), "{spec}: {out}");
            assert!(out.contains("serve started"), "{spec}: {out}");
            assert_eq!(
                out.contains("tool call"),
                spec != "rmcp::service=debug",
                "{spec}"
            );
        }
    }
}
