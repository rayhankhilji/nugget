//! `nugget-browser-host`: the native-messaging host the Nugget browser extension talks to.
//!
//! Launched by the browser (Chrome passes the caller's origin, Firefox the manifest path and the
//! add-on id): relays between stdio and the daemon. stdout carries the protocol, so this binary
//! never prints anything else there in host mode; diagnostics go to stderr, which browsers log.
//!
//! ```text
//! nugget-browser-host install   [--browser chrome|chromium|edge|firefox|all]... [--extension-id ID]...
//!                               [--firefox-id ID]... [--host-path PATH] [--dry-run]
//! nugget-browser-host uninstall [--browser ...]... [--dry-run]
//! nugget-browser-host manifest  --browser NAME
//! ```

use std::process::ExitCode;
use std::time::Duration;

use nugget_browser::bridge::AttachInfo;
use nugget_browser::host::{
    IpcDaemonLink, RelayEnd, run_relay, spawn_extension_reader, wait_extension_closed,
};
use nugget_browser::install::{self, Browser, InstallConfig};
use nugget_core::paths::Paths;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => cli(&args[1..], Mode::Install),
        Some("uninstall") => cli(&args[1..], Mode::Uninstall),
        Some("manifest") => cli(&args[1..], Mode::Manifest),
        Some("--help" | "-h" | "help") => {
            println!("{}", USAGE);
            ExitCode::SUCCESS
        }
        _ => host(&args),
    }
}

const USAGE: &str = "usage: nugget-browser-host install|uninstall|manifest [--browser NAME]... \
[--extension-id ID]... [--firefox-id ID]... [--host-path PATH] [--dry-run]\n\
Without a subcommand it runs as the native-messaging host (started by the browser).";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Install,
    Uninstall,
    Manifest,
}

fn cli(args: &[String], mode: Mode) -> ExitCode {
    match run_cli(args, mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nugget-browser-host: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli(args: &[String], mode: Mode) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cfg = InstallConfig::for_current_user(exe).map_err(|e| e.to_string())?;
    let mut browsers = Vec::new();
    let mut dry_run = false;
    let mut chromium_ids = Vec::new();
    let mut firefox_ids = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--browser" => {
                let v = value()?;
                if v == "all" {
                    browsers.extend(Browser::ALL);
                } else {
                    browsers.push(Browser::parse(&v).ok_or(format!("unknown browser {v}"))?);
                }
            }
            "--extension-id" => chromium_ids.push(value()?),
            "--firefox-id" => firefox_ids.push(value()?),
            "--host-path" => cfg.host_path = value()?,
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if browsers.is_empty() {
        browsers.extend(Browser::ALL);
    }
    if !chromium_ids.is_empty() {
        cfg.chromium_extension_ids = chromium_ids;
    }
    if !firefox_ids.is_empty() {
        cfg.firefox_extension_ids = firefox_ids;
    }
    let steps = match mode {
        Mode::Manifest => {
            for b in browsers {
                let m = install::manifest(b, &cfg);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?
                );
            }
            return Ok(());
        }
        Mode::Install => install::plan(&browsers, &cfg),
        Mode::Uninstall => install::uninstall_plan(&browsers, &cfg),
    }
    .map_err(|e| e.to_string())?;
    for line in install::apply(&steps, dry_run).map_err(|e| e.to_string())? {
        println!("{line}");
    }
    Ok(())
}

fn host(args: &[String]) -> ExitCode {
    // Chrome: argv[1] = "chrome-extension://<id>/". Firefox: argv[1] = manifest path,
    // argv[2] = add-on id. The manifest's allowed_origins/allowed_extensions already restrict
    // who can start us; the origin is passed on to the daemon for its records.
    let origin = args
        .iter()
        .find(|a| a.starts_with("chrome-extension://"))
        .cloned()
        .or_else(|| args.get(1).cloned())
        .unwrap_or_default();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nugget-browser-host: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async move {
        let paths = match Paths::discover() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("nugget-browser-host: {e}");
                return ExitCode::FAILURE;
            }
        };
        let info = AttachInfo {
            origin,
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let mut from_ext = spawn_extension_reader(tokio::io::stdin());
        let mut stdout = tokio::io::stdout();
        let mut backoff = Duration::from_millis(500);
        loop {
            // The daemon may start after the browser: keep retrying while the extension is
            // connected. Nothing from the extension needs an answer while we wait.
            let link = tokio::select! {
                link = IpcDaemonLink::connect(&paths, info.clone()) => link,
                // stdin closed while connecting: the extension is gone.
                () = wait_extension_closed(&mut from_ext) => return ExitCode::SUCCESS,
            };
            match link {
                Ok(link) => {
                    backoff = Duration::from_millis(500);
                    if run_relay(&mut from_ext, &mut stdout, &link).await
                        == RelayEnd::ExtensionClosed
                    {
                        return ExitCode::SUCCESS;
                    }
                    eprintln!("nugget-browser-host: daemon disconnected; reconnecting");
                }
                Err(e) => {
                    eprintln!("nugget-browser-host: daemon not reachable ({e}); retrying");
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        () = wait_extension_closed(&mut from_ext) => return ExitCode::SUCCESS,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
        }
    })
}
