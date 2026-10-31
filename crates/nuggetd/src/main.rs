//! `nuggetd` command line.
//!
//! - `nuggetd`: run the daemon in the foreground until SIGINT/SIGTERM (Ctrl+C on Windows).
//! - `nuggetd status [--json]`: ask the running daemon for its status.
//! - `nuggetd install-service` / `uninstall-service`: run at login as a per-user service.
//! - `nuggetd mcp --client <id>`: the stdio MCP bridge apps launch (token in `NUGGET_MCP_TOKEN`).
//! - `nuggetd mcp install --client claude-desktop|claude-code|cursor`: set an app up.
//! - `nuggetd backup [--to DIR]` / `nuggetd backup set-passphrase`: back up now, or set the
//!   backup passphrase (through the daemon when it runs, offline otherwise).
//! - `nuggetd restore --from DIR [--passphrase-file FILE]`: restore a backup (daemon stopped).
//! - `nuggetd export --to DIR`: export everything in open formats.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nugget_core::ipc::{StatusResponse, method};
use nugget_core::paths::Paths;
use nuggetd::install::{App, InstallOptions};
use nuggetd::{Daemon, DaemonOptions, Startup, backup_cli, cli, logging, service};

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
    /// MCP: run the stdio bridge for a client (`--client <id>`, token in NUGGET_MCP_TOKEN), or
    /// set an app up (`install`).
    #[command(args_conflicts_with_subcommands = true)]
    Mcp {
        /// The MCP client id to bridge (from `mcp install` or the app's Connected AI apps page).
        #[arg(long, value_name = "ID")]
        client: Option<String>,
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// Back up now (to the configured folder, or DIR), or set the backup passphrase.
    #[command(args_conflicts_with_subcommands = true)]
    Backup {
        /// Write the backup into DIR instead of the configured folder.
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
    /// Restore a backup directory (stop the daemon first). The current data is kept aside.
    Restore {
        /// The backup directory (`nugget-backup-<date and time>`).
        #[arg(long, value_name = "DIR")]
        from: PathBuf,
        /// Read the backup passphrase from FILE (else NUGGET_BACKUP_PASSPHRASE, else a prompt).
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
    /// Export everything in open formats (unencrypted) into a new folder in DIR.
    Export {
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BackupAction {
    /// Set the backup passphrase (seals the store key into every later backup).
    SetPassphrase {
        /// Read the passphrase from FILE (else NUGGET_BACKUP_PASSPHRASE, else a prompt).
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum McpAction {
    /// Create an MCP client and add Nugget to an app's MCP configuration.
    Install {
        /// The app to set up.
        #[arg(long, value_enum)]
        client: App,
        /// Name of the client in Nugget (default: the app's name).
        #[arg(long)]
        name: Option<String>,
        /// Config file to merge into (default: the app's usual location).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
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
    let home = args.home.clone();
    let code = match args.command {
        None => runtime.block_on(run(paths, home)),
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
        Some(Command::Mcp {
            action:
                Some(McpAction::Install {
                    client,
                    name,
                    config,
                    yes,
                }),
            ..
        }) => {
            logging::init_cli();
            let opts = InstallOptions {
                app: client,
                name,
                config,
                yes,
            };
            report(runtime.block_on(nuggetd::install::install(&paths, opts)))
        }
        Some(Command::Mcp {
            client: Some(client),
            action: None,
        }) => {
            // stdout carries MCP messages only; logs go to stderr.
            logging::init_cli();
            match std::env::var(nugget_mcp::TOKEN_ENV) {
                Ok(token) if !token.trim().is_empty() => {
                    match runtime.block_on(nuggetd::mcp_bridge::run(&paths, &client, token.trim()))
                    {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(e) => {
                            eprintln!("nuggetd mcp: {e}");
                            ExitCode::FAILURE
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "nuggetd mcp: set {} to the client's token",
                        nugget_mcp::TOKEN_ENV
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::Backup { to, action }) => {
            logging::init_cli();
            let vault = default_vault(&paths, args.home);
            report(runtime.block_on(async {
                match action {
                    Some(BackupAction::SetPassphrase { passphrase_file }) => {
                        backup_cli::set_passphrase(&paths, passphrase_file.as_deref()).await
                    }
                    None => backup_cli::backup(&paths, to, &vault).await,
                }
            }))
        }
        Some(Command::Restore {
            from,
            passphrase_file,
        }) => {
            logging::init_cli();
            let vault = default_vault(&paths, args.home);
            report(runtime.block_on(backup_cli::restore(
                &paths,
                from,
                passphrase_file.as_deref(),
                &vault,
            )))
        }
        Some(Command::Export { to }) => {
            logging::init_cli();
            let vault = default_vault(&paths, args.home);
            report(runtime.block_on(backup_cli::export(&paths, to, &vault)))
        }
        Some(Command::Mcp { .. }) => {
            eprintln!(
                "nuggetd mcp: use `nuggetd mcp --client <id>` (the stdio bridge) or `nuggetd mcp install --client <app>`"
            );
            ExitCode::FAILURE
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

async fn run(paths: Paths, home_flag: Option<PathBuf>) -> ExitCode {
    let _log_guard = match logging::init_daemon(&paths) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("nuggetd: warning: file logging disabled: {e}");
            None
        }
    };
    let mut opts = DaemonOptions::new(paths);
    match cli::home_override(home_flag) {
        // A normal install keeps its vault in the home directory.
        Ok(None) => {
            opts.default_vault_dir = std::env::home_dir().map(|h| h.join("Nugget Vault"));
        }
        Ok(Some(home)) => opts.home = Some(home),
        Err(e) => tracing::warn!(error = %e, "cannot resolve the home override"),
    }
    let daemon = match Daemon::start(opts).await {
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

/// The vault directory the daemon uses when `[vault] path` is unset (see [`run`]).
fn default_vault(paths: &Paths, home_flag: Option<PathBuf>) -> PathBuf {
    match cli::home_override(home_flag) {
        Ok(None) => std::env::home_dir()
            .map(|h| h.join("Nugget Vault"))
            .unwrap_or_else(|| nuggetd::knowledge::default_vault_dir(paths)),
        _ => nuggetd::knowledge::default_vault_dir(paths),
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
