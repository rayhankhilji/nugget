//! `nugget-bench` CLI.
//!
//! ```text
//! cargo run --release -p nugget-bench -- perf memory
//! cargo run --release -p nugget-bench -- memory --backend real --scale quick --strict
//! cargo run -p nugget-bench -- all --scale smoke --no-write
//! cargo run -p nugget-bench -- --list
//! cargo run -p nugget-bench -- cua --cua-agent reference --cua-env xvfb --strict
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use nugget_bench::report::SCHEMA_JSON;
use nugget_bench::suites::cua::{CuaAgentKind, CuaBench, CuaEnv, CuaOptions};
use nugget_bench::{
    Backend, Benchmark, Comparison, Report, RunConfig, Scale, Status, find, registry, run_benchmark,
};

/// Default report directory: `bench/results` in the source tree.
const DEFAULT_RESULTS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/results");

#[derive(Debug, Parser)]
#[command(
    name = "nugget-bench",
    version,
    about = "Run Nugget benchmark suites and write JSON reports"
)]
struct Cli {
    /// Suites to run (`memory`, `perf`, `draft`, `cua`), or `all`.
    #[arg(value_name = "SUITE")]
    suites: Vec<String>,

    /// List the registered suites and exit.
    #[arg(long)]
    list: bool,

    /// Print the report JSON schema and exit.
    #[arg(long)]
    schema: bool,

    /// Workload size. Use `standard` or `full` on a release build for publishable numbers.
    #[arg(long, value_enum, default_value_t = Scale::Standard)]
    scale: Scale,

    /// Seed for corpus and workload generation.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// What `memory` runs against: the in-crate fake index (fast), or the real store, ingest
    /// pipeline, files watcher and retriever in process. Other suites ignore it.
    #[arg(long, value_enum, default_value_t = Backend::Fake)]
    backend: Backend,

    /// Real backend: OpenAI-compatible embeddings endpoint (e.g. `http://127.0.0.1:8080/v1`)
    /// instead of the in-process deterministic embedder.
    #[arg(long, value_name = "URL")]
    embed_url: Option<String>,

    /// Model name sent to `--embed-url`.
    #[arg(long, value_name = "MODEL", requires = "embed_url")]
    embed_model: Option<String>,

    /// `cua`: the agent under test (`reference` validates the harness, `null` is the floor,
    /// `naive` is the red-team control that must trip the safety metrics).
    #[arg(long, value_enum, default_value_t = CuaAgentKind::Reference)]
    cua_agent: CuaAgentKind,

    /// `cua`: in process, over the apps' loopback socket, or on an Xvfb desktop with kiosk
    /// Chromium windows and AT-SPI (Linux).
    #[arg(long, value_enum, default_value_t = CuaEnv::Inproc)]
    cua_env: CuaEnv,

    /// `cua`: run only tasks matching any of these comma-separated ids, tags, apps or
    /// difficulties (e.g. `adversarial,shop`).
    #[arg(long, value_name = "TERMS")]
    cua_tasks: Option<String>,

    /// Directory for `<suite>-<timestamp>.json` reports.
    #[arg(long, env = "NUGGET_BENCH_RESULTS_DIR", default_value = DEFAULT_RESULTS_DIR)]
    out_dir: PathBuf,

    /// Do not write report files.
    #[arg(long)]
    no_write: bool,

    /// Print each report's JSON to stdout (the human summary goes to stderr).
    #[arg(long)]
    json: bool,

    /// Exit non-zero if a selected suite is not implemented yet (3) or a metric misses its
    /// target (4).
    #[arg(long)]
    strict: bool,
}

/// Resolve suite names, expanding `all` and rejecting unknown names.
fn select(names: &[String]) -> Result<Vec<Box<dyn Benchmark>>, String> {
    if names.is_empty() {
        return Err("no suite given; pass one or more of the names from --list, or `all`".into());
    }
    let mut out: Vec<Box<dyn Benchmark>> = Vec::new();
    for name in names {
        if name == "all" {
            for b in registry() {
                if !out.iter().any(|o| o.name() == b.name()) {
                    out.push(b);
                }
            }
            continue;
        }
        let b = find(name).ok_or_else(|| {
            let known: Vec<&str> = registry().iter().map(|b| b.name()).collect();
            format!("unknown suite `{name}` (known: {}, all)", known.join(", "))
        })?;
        if !out.iter().any(|o| o.name() == b.name()) {
            out.push(b);
        }
    }
    Ok(out)
}

fn summary(report: &Report) -> String {
    let mut s = format!(
        "== {} [{}] {} ms, scale={}, seed={}, backend={}, {} build\n",
        report.suite,
        match report.status {
            Status::Ok => "ok",
            Status::NotImplemented => "not implemented",
            Status::Failed => "FAILED",
        },
        report.duration_ms,
        report.config.scale,
        report.config.seed,
        report
            .config
            .params
            .get("backend")
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
        report.environment.build_profile,
    );
    let width = report
        .metrics
        .iter()
        .map(|m| m.name.len())
        .max()
        .unwrap_or(0);
    for m in &report.metrics {
        let target = match (m.target, m.target_met) {
            (Some(t), Some(true)) => format!("  (target {t}: met)"),
            (Some(t), Some(false)) => format!("  (target {t}: MISSED)"),
            _ => String::new(),
        };
        s.push_str(&format!(
            "  {:width$}  {:>14.4} {}{}\n",
            m.name, m.value, m.unit, target
        ));
    }
    for t in &report.targets {
        let op = match t.comparison {
            Comparison::AtLeast => ">=",
            Comparison::AtMost => "<=",
        };
        s.push_str(&format!(
            "  target {}: {} {op} {} -> {}\n",
            t.metric,
            t.value,
            t.target,
            if t.pass { "PASS" } else { "FAIL" }
        ));
    }
    if let Some(msg) = &report.message {
        s.push_str(&format!("  {msg}\n"));
    }
    s
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.schema {
        println!("{SCHEMA_JSON}");
        return ExitCode::SUCCESS;
    }
    if cli.list {
        for b in registry() {
            let state = if b.implemented() {
                ""
            } else {
                " [not yet implemented]"
            };
            println!("{:8} {}{}", b.name(), b.description(), state);
        }
        return ExitCode::SUCCESS;
    }
    let suites = match select(&cli.suites) {
        Ok(s) => s
            .into_iter()
            .map(|b| -> Box<dyn Benchmark> {
                if b.name() == "cua" {
                    Box::new(CuaBench::new(CuaOptions {
                        agent: cli.cua_agent,
                        env: cli.cua_env,
                        filter: cli.cua_tasks.clone(),
                    }))
                } else {
                    b
                }
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let timed = |b: &dyn Benchmark| {
        b.name() == "perf" || (b.name() == "memory" && cli.backend == Backend::Real)
    };
    if cfg!(debug_assertions) && suites.iter().any(|b| timed(b.as_ref())) {
        eprintln!("note: debug build; timings are not representative (use `cargo run --release`)");
    }

    let config = RunConfig {
        scale: cli.scale,
        seed: cli.seed,
        backend: cli.backend,
        embed_url: cli.embed_url.clone(),
        embed_model: cli.embed_model.clone(),
    };
    let mut failed = false;
    let mut unimplemented = false;
    let mut missed = false;
    for bench in suites {
        let report = run_benchmark(bench.as_ref(), &config);
        let text = summary(&report);
        if cli.json {
            eprint!("{text}");
            match report.to_json_pretty() {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("error: cannot serialise report: {e}");
                    failed = true;
                }
            }
        } else {
            print!("{text}");
        }
        if !cli.no_write {
            match report.write_to_dir(&cli.out_dir) {
                Ok(path) => eprintln!("  wrote {}", path.display()),
                Err(e) => {
                    eprintln!(
                        "error: cannot write report to {}: {e}",
                        cli.out_dir.display()
                    );
                    failed = true;
                }
            }
        }
        let _ = std::io::stdout().flush();
        match report.status {
            Status::Ok => {}
            Status::NotImplemented => unimplemented = true,
            Status::Failed => failed = true,
        }
        missed |= report.targets_missed();
    }
    exit_code(failed, missed, unimplemented, cli.strict)
}

/// 1: a suite failed or a report could not be written. With `--strict`: 4 if a target was
/// missed, else 3 if a suite is not implemented. Otherwise 0.
fn exit_code(failed: bool, missed: bool, unimplemented: bool, strict: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else if strict && missed {
        ExitCode::from(4)
    } else if strict && unimplemented {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_suites_and_options() {
        let cli = Cli::try_parse_from([
            "nugget-bench",
            "perf",
            "memory",
            "--scale",
            "quick",
            "--seed",
            "9",
        ])
        .unwrap();
        assert_eq!(cli.suites, ["perf", "memory"]);
        assert_eq!(cli.scale, Scale::Quick);
        assert_eq!(cli.seed, 9);
        assert!(!cli.no_write);
        assert_eq!(cli.backend, Backend::Fake, "fake stays the default");
        assert!(Cli::try_parse_from(["nugget-bench", "perf", "--scale", "huge"]).is_err());

        let real = Cli::try_parse_from([
            "nugget-bench",
            "memory",
            "--backend",
            "real",
            "--embed-url",
            "http://127.0.0.1:8080/v1",
            "--embed-model",
            "nomic",
            "--strict",
        ])
        .unwrap();
        assert_eq!(real.backend, Backend::Real);
        assert_eq!(real.embed_url.as_deref(), Some("http://127.0.0.1:8080/v1"));
        assert_eq!(real.embed_model.as_deref(), Some("nomic"));
        assert!(real.strict);
        assert!(Cli::try_parse_from(["nugget-bench", "memory", "--backend", "mock"]).is_err());

        let cua = Cli::try_parse_from([
            "nugget-bench",
            "cua",
            "--cua-agent",
            "null",
            "--cua-env",
            "socket",
            "--cua-tasks",
            "adversarial",
        ])
        .unwrap();
        assert_eq!(cua.cua_agent, CuaAgentKind::Null);
        assert_eq!(cua.cua_env, CuaEnv::Socket);
        assert_eq!(cua.cua_tasks.as_deref(), Some("adversarial"));
        assert!(Cli::try_parse_from(["nugget-bench", "cua", "--cua-env", "vm"]).is_err());
        assert!(
            Cli::try_parse_from(["nugget-bench", "memory", "--embed-model", "x"]).is_err(),
            "--embed-model needs --embed-url"
        );
    }

    #[test]
    fn exit_codes() {
        let code = |f, m, u, s| format!("{:?}", exit_code(f, m, u, s));
        assert_eq!(
            code(false, false, false, true),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            code(false, true, true, false),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            code(false, true, true, true),
            format!("{:?}", ExitCode::from(4))
        );
        assert_eq!(
            code(false, false, true, true),
            format!("{:?}", ExitCode::from(3))
        );
        assert_eq!(
            code(true, true, true, true),
            format!("{:?}", ExitCode::FAILURE)
        );
    }

    #[test]
    fn selection() {
        let names = |v: &[&str]| -> Result<Vec<&'static str>, String> {
            let owned: Vec<String> = v.iter().map(|s| (*s).to_owned()).collect();
            select(&owned).map(|s| s.iter().map(|b| b.name()).collect())
        };
        assert_eq!(names(&["perf"]).unwrap(), ["perf"]);
        assert_eq!(names(&["all"]).unwrap(), ["memory", "perf", "draft", "cua"]);
        assert_eq!(
            names(&["perf", "all", "perf"]).unwrap(),
            ["perf", "memory", "draft", "cua"]
        );
        assert!(names(&[]).is_err());
        assert!(names(&["bogus"]).unwrap_err().contains("unknown suite"));
    }

    #[test]
    fn default_results_dir_is_bench_results() {
        let p = PathBuf::from(DEFAULT_RESULTS_DIR);
        assert_eq!(p.file_name().unwrap(), "results");
        assert!(p.parent().unwrap().join("Cargo.toml").exists());
    }
}
