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
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
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
