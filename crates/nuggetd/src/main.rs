//! `nuggetd` command line.
//!
//! - `nuggetd`: run the daemon in the foreground until SIGINT/SIGTERM (Ctrl+C on Windows).
//! - `nuggetd status [--json]`: ask the running daemon for its status.
//! - `nuggetd install-service` / `uninstall-service`: run at login as a per-user service.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nugget_core::ipc::{StatusResponse, method};
use nugget_core::paths::Paths;
use nuggetd::{Daemon, DaemonOptions, Startup, cli, logging, service};

/// How long exit waits for blocking work (hashing, SQLite) still running after the daemon stops.
const RUNTIME_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(name = "nuggetd", version, about = "Nugget background daemon")]
struct Args {
    /// Keep all Nugget data, config and cache under DIR (overrides NUGGET_HOME).
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show the running daemon's status.
    Status {
        /// Print the raw JSON status.
        #[arg(long)]
        json: bool,
    },
    /// Start nuggetd at login as a user-level service (launchd, systemd --user, Task Scheduler).
    InstallService,
    /// Stop and remove the user-level service.
    UninstallService,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let paths = match cli::resolve_paths(args.home.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nuggetd: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nuggetd: cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = match args.command {
        None => runtime.block_on(run(paths)),
        Some(Command::Status { json }) => {
            logging::init_cli();
            runtime.block_on(status(paths, json))
        }
        Some(Command::InstallService) => {
            logging::init_cli();
            let result = cli::service_spec(&paths, args.home).and_then(|s| service::install(&s));
            report(result)
        }
        Some(Command::UninstallService) => {
            logging::init_cli();
            report(service::uninstall())
        }
    };
    // Dropping the runtime would wait for every `spawn_blocking` job (a multi-GB model hash can
    // take minutes); give them a bounded grace period instead.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_GRACE);
    code
}

fn report(result: nugget_core::Result<String>) -> ExitCode {
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nuggetd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(paths: Paths) -> ExitCode {
    let _log_guard = match logging::init_daemon(&paths) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("nuggetd: warning: file logging disabled: {e}");
            None
        }
    };
    let daemon = match Daemon::start(DaemonOptions::new(paths)).await {
        Ok(Startup::Ready(d)) => d,
        Ok(Startup::AlreadyRunning { socket }) => {
            tracing::info!(%socket, "another nuggetd is already running; exiting");
            println!("nuggetd is already running ({socket})");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            eprintln!("nuggetd: {e}");
            return ExitCode::FAILURE;
        }
    };
    if daemon.config_created() {
        eprintln!(
            "nuggetd: wrote default config to {}",
            daemon.paths().config_file().display()
        );
    }
    match daemon.run_until(shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "daemon failed");
            eprintln!("nuggetd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn status(paths: Paths, json: bool) -> ExitCode {
    let socket = nugget_ipc::socket_display(&paths);
    let client = match nugget_ipc::Client::connect(&paths).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nuggetd is not running (no answer on {socket}: {e})");
            return ExitCode::FAILURE;
        }
    };
    match client.call::<_, StatusResponse>(method::STATUS, &()).await {
        Ok(s) if json => match serde_json::to_string_pretty(&s) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("nuggetd: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(s) => {
            print!("{}", cli::format_status(&s, &socket));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nuggetd: status failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolves on SIGINT or SIGTERM (Unix), or Ctrl+C / Ctrl+Break / console close (Windows).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => {
                tokio::select! {
                    _ = int.recv() => tracing::info!("SIGINT received"),
                    _ = term.recv() => tracing::info!("SIGTERM received"),
                }
            }
            _ => {
                tracing::warn!("cannot install signal handlers; stopping on Ctrl+C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        async fn or_pending<T, F, Fut>(sig: std::io::Result<T>, recv: F)
        where
            F: FnOnce(T) -> Fut,
            Fut: std::future::Future<Output = ()>,
        {
            match sig {
                Ok(s) => recv(s).await,
                Err(_) => std::future::pending().await,
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = or_pending(windows::ctrl_break(), |mut s| async move { s.recv().await; }) => {}
            () = or_pending(windows::ctrl_close(), |mut s| async move { s.recv().await; }) => {}
            () = or_pending(windows::ctrl_shutdown(), |mut s| async move { s.recv().await; }) => {}
            () = or_pending(windows::ctrl_logoff(), |mut s| async move { s.recv().await; }) => {}
        }
        tracing::info!("console control signal received");
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
