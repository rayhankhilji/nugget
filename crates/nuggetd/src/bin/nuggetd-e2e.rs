//! `nuggetd-e2e`: the daemon exactly as `nuggetd` runs it (no subcommand), plus the test hooks of
//! [`RuntimeOverrides`] read from the environment, for the desktop end-to-end test
//! (`apps/desktop/e2e`). Built only with the `fake-runtime` feature; never shipped.
//!
//! - `NUGGET_E2E_MANIFEST` + `NUGGET_E2E_MANIFEST_SIG` + `NUGGET_E2E_MANIFEST_PUBKEY`: a model
//!   manifest (JSON file), its detached signature (hex file) and the hex ed25519 public key it must
//!   verify against. Only that key is trusted (never the built-in ones); a manifest that fails
//!   verification stops the daemon from starting.
//! - `NUGGET_E2E_LLAMA_SERVER`: the `llama-server` executable to run (CPU), e.g. the repo's
//!   `nuggetd-fake-llama-server`.
//! - `NUGGET_E2E_RAM_GB` / `NUGGET_E2E_GPU` (`apple_silicon`, `nvidia`, `amd`, `intel`,
//!   `unknown`): the hardware the recommendation sees.
//!
//! `--home DIR` keeps everything under DIR, like `nuggetd --home`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use nugget_models::{GpuKind, Hardware, RuntimeAccel, parse_verifying_key, verify_manifest};
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup, cli, logging};

fn main() -> ExitCode {
    let mut home = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--home", Some(dir)) => home = Some(PathBuf::from(dir)),
            _ => {
                eprintln!("usage: nuggetd-e2e [--home DIR]");
                return ExitCode::from(2);
            }
        }
    }
    let overrides = match overrides_from_env() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nuggetd-e2e: {e}");
            return ExitCode::FAILURE;
        }
    };
    let paths = match cli::resolve_paths(home.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nuggetd-e2e: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nuggetd-e2e: cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(async move {
        let _log_guard = logging::init_daemon(&paths).ok();
        let mut opts = DaemonOptions::new(paths);
        opts.home = cli::home_override(home).ok().flatten();
        opts.runtime = overrides;
        let daemon = match Daemon::start(opts).await {
            Ok(Startup::Ready(d)) => d,
            Ok(Startup::AlreadyRunning { socket }) => {
                eprintln!("nuggetd-e2e: a daemon already answers on {socket}");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("nuggetd-e2e: {e}");
                return ExitCode::FAILURE;
            }
        };
        match daemon.run_until(shutdown_signal()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("nuggetd-e2e: {e}");
                ExitCode::FAILURE
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    code
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn overrides_from_env() -> Result<RuntimeOverrides, String> {
    let mut o = RuntimeOverrides::default();
    match (
        env("NUGGET_E2E_MANIFEST"),
        env("NUGGET_E2E_MANIFEST_SIG"),
        env("NUGGET_E2E_MANIFEST_PUBKEY"),
    ) {
        (None, None, None) => {}
        (Some(manifest), Some(sig), Some(key)) => {
            let read = |p: &str| std::fs::read(p).map_err(|e| format!("cannot read {p}: {e}"));
            let key = parse_verifying_key(&key)
                .map_err(|e| format!("NUGGET_E2E_MANIFEST_PUBKEY: {e}"))?;
            let m = verify_manifest(&read(&manifest)?, &read(&sig)?, &[key])
                .map_err(|e| format!("the e2e manifest {manifest} does not verify: {e}"))?;
            o.manifest = Some(m);
        }
        _ => {
            return Err(
                "set all of NUGGET_E2E_MANIFEST, NUGGET_E2E_MANIFEST_SIG and \
                        NUGGET_E2E_MANIFEST_PUBKEY, or none"
                    .into(),
            );
        }
    }
    if let Some(server) = env("NUGGET_E2E_LLAMA_SERVER") {
        o.server_binary = Some((PathBuf::from(server), RuntimeAccel::Cpu));
    }
    let gpu = env("NUGGET_E2E_GPU");
    if let Some(gb) = env("NUGGET_E2E_RAM_GB") {
        let gb: u64 = gb
            .trim()
            .parse()
            .map_err(|_| format!("NUGGET_E2E_RAM_GB: not a number: {gb}"))?;
        let gpu = match gpu.as_deref().map(str::trim) {
            None | Some("unknown" | "none") => GpuKind::Unknown,
            Some("apple_silicon") => GpuKind::AppleSilicon,
            Some("nvidia") => GpuKind::Nvidia,
            Some("amd") => GpuKind::Amd,
            Some("intel") => GpuKind::Intel,
            Some(other) => return Err(format!("NUGGET_E2E_GPU: unknown GPU kind {other}")),
        };
        o.hardware = Some(Hardware {
            total_ram: gb << 30,
            available_ram: (gb << 30) / 2,
            cpu_cores: 4,
            gpu,
        });
    } else if gpu.is_some() {
        return Err("NUGGET_E2E_GPU needs NUGGET_E2E_RAM_GB".into());
    }
    Ok(o)
}

/// Resolves on SIGINT or SIGTERM (Unix), or Ctrl+C elsewhere.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let (Ok(mut int), Ok(mut term)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            tokio::select! {
                _ = int.recv() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
