//! `nuggetd` command line.

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
    /// Restart the user-level service (for example after an update replaced nuggetd).
    RestartService,
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
        Err(e) => return fail(format!("nuggetd: {e}")),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(format!("nuggetd: cannot start async runtime: {e}")),
    };
    let home = args.home.clone();
    if args.command.is_some() {
        // stdout carries command output (MCP messages for the bridge); logs go to stderr.
        logging::init_cli();
    }
    let code = match args.command {
        None => runtime.block_on(run(paths, home)),
        Some(Command::Status { json }) => runtime.block_on(status(paths, json)),
        Some(Command::InstallService) => {
            report(cli::service_spec(&paths, args.home).and_then(|s| service::install(&s)))
        }
        Some(Command::UninstallService) => report(service::uninstall()),
        Some(Command::RestartService) => report(service::restart()),
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
        }) => match std::env::var(nugget_mcp::TOKEN_ENV) {
            Ok(token) if !token.trim().is_empty() => {
                match runtime.block_on(nuggetd::mcp_bridge::run(&paths, &client, token.trim())) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => fail(format!("nuggetd mcp: {e}")),
                }
            }
            _ => fail(format!(
                "nuggetd mcp: set {} to the client's token",
                nugget_mcp::TOKEN_ENV
            )),
        },
        Some(Command::Backup { to, action }) => {
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
            let vault = default_vault(&paths, args.home);
            report(runtime.block_on(backup_cli::restore(
                &paths,
                from,
                passphrase_file.as_deref(),
                &vault,
            )))
        }
        Some(Command::Export { to }) => {
            let vault = default_vault(&paths, args.home);
            report(runtime.block_on(backup_cli::export(&paths, to, &vault)))
        }
        Some(Command::Mcp { .. }) => fail(
            "nuggetd mcp: use `nuggetd mcp --client <id>` (the stdio bridge) or `nuggetd mcp install --client <app>`",
        ),
    };
    // Don't wait for every `spawn_blocking` job (a model hash can take minutes).
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_GRACE);
    code
}

fn report(result: nugget_core::Result<String>) -> ExitCode {
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("nuggetd: {e}")),
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::FAILURE
}

async fn run(paths: Paths, home_flag: Option<PathBuf>) -> ExitCode {
    let _log_guard = match logging::init_daemon(&paths) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("nuggetd: warning: file logging disabled: {e}");
            None
        }
    };
    // Register stop signals first: without a listener the OS default kills the process uncleanly.
    let shutdown = shutdown_signal();
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
            return fail(format!("nuggetd: {e}"));
        }
    };
    if daemon.config_created() {
        eprintln!(
            "nuggetd: wrote default config to {}",
            daemon.paths().config_file().display()
        );
    }
    match daemon.run_until(shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "daemon failed");
            fail(format!("nuggetd: {e}"))
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
            return fail(format!(
                "nuggetd is not running (no answer on {socket}: {e})"
            ));
        }
    };
    match client.call::<_, StatusResponse>(method::STATUS, &()).await {
        Ok(s) if json => match serde_json::to_string_pretty(&s) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(format!("nuggetd: {e}")),
        },
        Ok(s) => {
            print!("{}", cli::format_status(&s, &socket));
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("nuggetd: status failed: {e}")),
    }
}

/// Registers the stop signals now and returns a future that resolves on the first of them:
/// SIGINT or SIGTERM (Unix), or Ctrl+C / Ctrl+Break / console close, logoff or shutdown (Windows).
/// A signal that cannot be registered is logged; the others still work.
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let int = signal(SignalKind::interrupt());
        let term = signal(SignalKind::terminate());
        if let Err(e) = &int {
            tracing::warn!(error = %e, "cannot listen for SIGINT");
        }
        if let Err(e) = &term {
            tracing::warn!(error = %e, "cannot listen for SIGTERM");
        }
        tracing::info!("stop signals registered");
        async move {
            async fn recv(s: std::io::Result<tokio::signal::unix::Signal>) {
                match s {
                    Ok(mut s) => {
                        s.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            }
            tokio::select! {
                () = recv(int) => tracing::info!("SIGINT received"),
                () = recv(term) => tracing::info!("SIGTERM received"),
            }
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        // Listeners register when created here, not when the future is first polled.
        fn listen<T>(name: &str, r: std::io::Result<T>) -> Option<T> {
            r.map_err(
                |e| tracing::warn!(signal = name, error = %e, "cannot listen for a console signal"),
            )
            .ok()
        }
        let c = listen("ctrl_c", windows::ctrl_c());
        let brk = listen("ctrl_break", windows::ctrl_break());
        let close = listen("ctrl_close", windows::ctrl_close());
        let shutdown = listen("ctrl_shutdown", windows::ctrl_shutdown());
        let logoff = listen("ctrl_logoff", windows::ctrl_logoff());
        tracing::info!("console control signals registered");
        async move {
            macro_rules! recv {
                ($s:expr) => {
                    async {
                        match $s {
                            Some(mut s) => {
                                s.recv().await;
                            }
                            None => std::future::pending::<()>().await,
                        }
                    }
                };
            }
            let which = tokio::select! {
                () = recv!(c) => "ctrl_c",
                () = recv!(brk) => "ctrl_break",
                () = recv!(close) => "ctrl_close",
                () = recv!(shutdown) => "ctrl_shutdown",
                () = recv!(logoff) => "ctrl_logoff",
            };
            tracing::info!(signal = which, "console control signal received");
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        async {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
