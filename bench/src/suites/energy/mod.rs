//! `bench/energy`: what Nugget costs in energy and battery, measured where it can be and reported
//! as CPU time and wakeups where it cannot (DD2; `docs/PLAN.md`: "battery cost per 1k items").
//!
//! Every scenario meters its processes with `meter::Meter` (CPU time and wakeups of the
//! processes and their descendants, and system package energy from RAPL on Linux or
//! `powermetrics` on macOS when readable; see that module for sources and fallbacks). System
//! energy is the whole machine's, so a scenario with an energy source also meters an equal
//! window with nothing launched (`meter::baseline`) and reports `attributable = window −
//! baseline`. On a shared VM that difference is noisy; the CPU and wakeup figures are not.
//!
//! Scenarios (metric prefix `energy.<scenario>.`), chosen with `--energy-scenarios` or
//! `NUGGET_BENCH_ENERGY_SCENARIOS` ([`Scenario::parse_list`]):
//! - `daemon_idle` (`daemon.rs`): the real `nuggetd` on a seeded home, after its startup work
//!   (knowledge run, vault render) and a settle period, idle with no IPC traffic for the window.
//! - `index_1k` (`daemon.rs`): a fresh daemon indexes 1,000 Markdown files dropped into a watched
//!   folder, until all are indexed and searchable; per 1k items. Text-only: the daemon's config
//!   has no model providers, so no embedding model runs (a known gap).
//! - `a11y_poll` (`poll.rs`): the desktop shell's once-a-second frontmost-app read, in process, at
//!   1 Hz against a window that only sleeps at 1 Hz.
//! - `prefill` (`prefill.rs`, `--local-models` only): read-aheads of Double Tap fixture tasks
//!   through [`nugget_draft::Drafter::read_ahead`] against the real local chat server, metering
//!   `llama-server`.
//! - `shell_idle` (`shell.rs`): the desktop shell from [`SHELL_ENV`], started hidden in the tray
//!   against a daemon, idle for the window; shell (with its webview processes) and daemon
//!   metered separately.
//! - `shell_focus` (`shell.rs`): a scripted window of focus changes among three text-field
//!   windows, read-ahead off then on, with the fake llama-server as the daemon's local
//!   provider; shell, daemon and server metered separately. Both need a desktop session
//!   (`bench/scripts/energy-linux.sh` on Linux) and run only when listed.
//!
//! The daemon scenarios reuse `perf_daemon`'s binary lookup (`$NUGGET_BENCH_NUGGETD`, else
//! `nuggetd` next to `nugget-bench`), seeding and config, and are skipped (recorded in
//! `config.params`, not failed) when there is no binary. Windows scale with `--scale`
//! ([`EnergyParams::for_scale`]).

pub(crate) mod daemon;
pub(crate) mod meter;
pub(crate) mod poll;
pub(crate) mod prefill;
pub(crate) mod shell;

use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext, Scale};
use crate::suites::perf_daemon::find_nuggetd;
use meter::{EnergySource, Reading};

/// Environment variable with the scenario list (same syntax as `--energy-scenarios`).
pub const SCENARIOS_ENV: &str = "NUGGET_BENCH_ENERGY_SCENARIOS";

/// `energy` options (flattened into the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct EnergyOptions {
    /// `energy`: comma-separated scenarios to run (`daemon_idle`, `index_1k`, `a11y_poll`,
    /// `prefill`, `shell_idle`, `shell_focus`; short forms `idle`, `index`, `poll`; `all`).
    /// Default: `daemon_idle,index_1k,a11y_poll,prefill`.
    #[arg(long, value_name = "LIST", env = SCENARIOS_ENV)]
    pub energy_scenarios: Option<String>,
}

/// One energy scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    DaemonIdle,
    Index1k,
    A11yPoll,
    Prefill,
    /// The desktop shell, minimized, idle with its window hidden (`shell.rs`).
    ShellIdle,
    /// A scripted minute of focus changes with read-ahead on vs off (`shell.rs`).
    ShellFocus,
}

impl Scenario {
    /// What runs when no list is given.
    pub const DEFAULT: [Scenario; 4] = [
        Scenario::DaemonIdle,
        Scenario::Index1k,
        Scenario::A11yPoll,
        Scenario::Prefill,
    ];
    /// Every scenario, in run order (`all`).
    pub const ALL: [Scenario; 6] = [
        Scenario::DaemonIdle,
        Scenario::Index1k,
        Scenario::A11yPoll,
        Scenario::Prefill,
        Scenario::ShellIdle,
        Scenario::ShellFocus,
    ];

    /// The name in metric names, params and the scenario list.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DaemonIdle => "daemon_idle",
            Self::Index1k => "index_1k",
            Self::A11yPoll => "a11y_poll",
            Self::Prefill => "prefill",
            Self::ShellIdle => "shell_idle",
            Self::ShellFocus => "shell_focus",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::DaemonIdle => "idle",
            Self::Index1k => "index",
            Self::A11yPoll => "poll",
            Self::Prefill => "prefill",
            Self::ShellIdle => "shell_idle",
            Self::ShellFocus => "shell_focus",
        }
    }

    /// A comma-separated list of names or short forms (`all` for every one), in the order
    /// given, without repeats.
    pub fn parse_list(list: &str) -> Result<Vec<Scenario>, String> {
        let mut out = Vec::new();
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let found: Vec<Scenario> = if name == "all" {
                Self::ALL.to_vec()
            } else {
                let s = Self::ALL
                    .into_iter()
                    .find(|s| s.as_str() == name || s.short() == name)
                    .ok_or_else(|| {
                        let known: Vec<&str> = Self::ALL.iter().map(|s| s.as_str()).collect();
                        format!(
                            "unknown energy scenario `{name}` (known: {}, all)",
                            known.join(", ")
                        )
                    })?;
                vec![s]
            };
            for s in found {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        if out.is_empty() {
            return Err("empty energy scenario list".into());
        }
        Ok(out)
    }
}

/// Workload and window lengths.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyParams {
    /// Items seeded into the store of the `daemon_idle` daemon.
    pub idle_items: usize,
    /// Quiet time after the daemon's startup work, before the idle window (Tokio's idle
    /// blocking threads linger 10 s).
    pub settle_s: f64,
    /// `daemon_idle` window.
    pub idle_s: f64,
    /// Files dropped into the watched folder by `index_1k` (metrics are per 1k items).
    pub index_items: usize,
    /// Length of each `a11y_poll` arm (polling, sleeping).
    pub poll_s: f64,
    /// Read-aheads metered by `prefill`.
    pub prefill_runs: usize,
    /// Upper bound on each wait for the daemon.
    pub timeout_s: f64,
    /// Shell scenarios: quiet time after the shell is set up (its probe says so), before the
    /// window (`shell_idle`'s window is `idle_s`).
    pub shell_settle_s: f64,
    /// `shell_focus`: length of the scripted window of focus changes (each run).
    pub focus_s: f64,
    /// `shell_focus`: seconds between focus changes.
    pub focus_every_s: f64,
}

impl EnergyParams {
    /// `smoke` takes seconds (CI and tests); `quick` about a minute per window; `standard` five
    /// minutes; `full` ten (the PLAN's idle window).
    pub fn for_scale(scale: Scale) -> Self {
        Self {
            idle_items: scale.pick(200, 2_000, 10_000, 10_000),
            settle_s: scale.pick(1.0, 12.0, 15.0, 15.0),
            idle_s: scale.pick(2.0, 60.0, 300.0, 600.0),
            index_items: scale.pick(100, 1_000, 1_000, 1_000),
            poll_s: scale.pick(2.0, 60.0, 300.0, 600.0),
            prefill_runs: scale.pick(2, 4, 8, 12),
            timeout_s: scale.pick(180.0, 300.0, 1_200.0, 3_600.0),
            shell_settle_s: scale.pick(3.0, 15.0, 20.0, 20.0),
            focus_s: scale.pick(15.0, 60.0, 60.0, 60.0),
            focus_every_s: 5.0,
        }
    }
}

pub struct EnergyBench;

impl Benchmark for EnergyBench {
    fn name(&self) -> &'static str {
        "energy"
    }

    fn description(&self) -> &'static str {
        "Energy and battery cost: idle daemon, joules per 1k items indexed, the shell's \
         frontmost-app poll, read-ahead prefill (RAPL / powermetrics, else CPU time and wakeups)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let list = ctx.config.energy.energy_scenarios.clone();
        let scenarios = match list.as_deref() {
            Some(l) => Scenario::parse_list(l).map_err(BenchError::Failed)?,
            None => Scenario::DEFAULT.to_vec(),
        };
        run_with(ctx, &EnergyParams::for_scale(ctx.scale()), &scenarios)
    }
}

/// Run `scenarios` with `p` (tests pass small params).
pub fn run_with(
    ctx: &mut RunContext,
    p: &EnergyParams,
    scenarios: &[Scenario],
) -> Result<Vec<Metric>, BenchError> {
    let (source, note) = meter::energy_source();
    ctx.param(
        "scenarios",
        scenarios.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    );
    ctx.param("energy_source", source.as_str());
    if let Some(n) = &note {
        ctx.param("energy_source.note", n.clone());
    }
    ctx.param("sample_interval_s", meter::SAMPLE_INTERVAL.as_secs_f64());
    ctx.param(
        "attribution",
        "package energy is the whole machine's: each scenario with an energy source also meters \
         an equal window with nothing launched (a11y_poll: its own sleeping window) and reports \
         attributable = window minus baseline, as mean watts; noisy on shared VMs",
    );
    ctx.param("settle_s", p.settle_s);
    ctx.param("timeout_s", p.timeout_s);
    let mut out = Vec::new();
    for &s in scenarios {
        let name = s.as_str();
        eprintln!("energy: {name}");
        match s {
            Scenario::DaemonIdle | Scenario::Index1k => match find_nuggetd() {
                Ok(exe) => {
                    ctx.param("daemon_exe", exe.display().to_string());
                    let m = if s == Scenario::DaemonIdle {
                        daemon::daemon_idle(ctx, p, &exe, source)?
                    } else {
                        daemon::index_1k(ctx, p, &exe, source)?
                    };
                    out.extend(m);
                }
                Err(why) => skip(ctx, s, &why),
            },
            Scenario::A11yPoll => out.extend(poll::a11y_poll(ctx, p)?),
            Scenario::Prefill => out.extend(prefill::prefill(ctx, p, source)?),
            Scenario::ShellIdle | Scenario::ShellFocus => {
                out.extend(shell_hook(ctx, p, s, source)?)
            }
        }
    }
    Ok(out)
}

/// Record that `s` did not run, and why.
fn skip(ctx: &mut RunContext, s: Scenario, why: &str) {
    eprintln!("note: energy {} skipped: {why}", s.as_str());
    ctx.param(s.as_str(), format!("skipped: {why}"));
}

/// The desktop shell scenarios (`shell.rs`): `nugget-desktop --minimized` from [`SHELL_ENV`]
/// against a daemon, the shell metered with its webview children; skipped (recorded) when the
/// shell, a display or the focus tools are missing.
fn shell_hook(
    ctx: &mut RunContext,
    p: &EnergyParams,
    s: Scenario,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    if s == Scenario::ShellIdle {
        shell::shell_idle(ctx, p, source)
    } else {
        shell::shell_focus(ctx, p, source)
    }
}

/// Environment variable naming the desktop shell binary the shell scenarios launch.
pub const SHELL_ENV: &str = "NUGGET_BENCH_SHELL";

/// Record where a reading's figures came from (`<scenario>.energy_source`, `.process_source`,
/// `.window_s`, `.energy_note`; `<scenario>.baseline_s` with a baseline).
pub(crate) fn record(ctx: &mut RunContext, scenario: &str, r: &Reading, base: Option<&Reading>) {
    ctx.param(
        &format!("{scenario}.energy_source"),
        r.energy_source.as_str(),
    );
    ctx.param(&format!("{scenario}.process_source"), r.process_source);
    ctx.param(&format!("{scenario}.window_s"), round3(r.wall_s));
    ctx.param(&format!("{scenario}.processes"), r.processes);
    if let Some(n) = &r.energy_note {
        ctx.param(&format!("{scenario}.energy_note"), n.clone());
    }
    if let Some(b) = base {
        ctx.param(&format!("{scenario}.baseline_s"), round3(b.wall_s));
    }
}

/// `{prefix}.cpu_percent`, `.wakeups_per_s` and `.window_s` of a window, where measured.
pub(crate) fn window_metrics(prefix: &str, what: &str, r: &Reading) -> Vec<Metric> {
    let mut out = vec![Metric::new(
        format!("{prefix}.window_s"),
        r.wall_s,
        "s",
        Direction::Info,
        format!("Length of the metered window ({what})"),
    )];
    if let Some(c) = r.cpu_percent() {
        out.push(Metric::new(
            format!("{prefix}.cpu_percent"),
            c,
            "%",
            Direction::LowerIsBetter,
            format!("CPU time of {what} (descendants included) as a share of one core"),
        ));
    }
    if let Some(w) = r.wakeups_per_s() {
        out.push(Metric::new(
            format!("{prefix}.wakeups_per_s"),
            w,
            "1/s",
            Direction::LowerIsBetter,
            format!(
                "Wakeups of {what} per second (Linux: voluntary + involuntary context switches \
                 of all threads; macOS: idle + interrupt wakeups)"
            ),
        ));
    }
    out
}

/// System energy of a window: `{prefix}.package_w`; with a baseline, `.baseline_w` and
/// `.attributable_w`; on macOS `.energy_impact`. Nothing without an energy source.
pub(crate) fn energy_metrics(
    prefix: &str,
    what: &str,
    r: &Reading,
    base: Option<&Reading>,
) -> Vec<Metric> {
    let mut out = Vec::new();
    if let Some(w) = r.package_w {
        out.push(Metric::new(
            format!("{prefix}.package_w"),
            w,
            "W",
            Direction::Info,
            format!(
                "Mean system package power during {what} ({}; the whole machine)",
                r.energy_source.as_str()
            ),
        ));
    }
    if let Some(b) = base {
        if let Some(w) = b.package_w {
            out.push(Metric::new(
                format!("{prefix}.baseline_w"),
                w,
                "W",
                Direction::Info,
                "Mean system package power over the baseline window (nothing launched)",
            ));
        }
        if let Some(w) = r.attributable_w(b) {
            out.push(Metric::new(
                format!("{prefix}.attributable_w"),
                w,
                "W",
                Direction::LowerIsBetter,
                format!(
                    "Mean package power during {what} minus the baseline's (noisy on shared VMs; \
                     can be negative)"
                ),
            ));
        }
    }
    if let Some(e) = r.energy_impact {
        out.push(Metric::new(
            format!("{prefix}.energy_impact"),
            e,
            "score/s",
            Direction::LowerIsBetter,
            format!("macOS energy impact of {what} per second (powermetrics tasks, unitless)"),
        ));
    }
    out
}

/// Whether scenarios should meter a baseline window (only system energy needs one).
pub(crate) fn wants_baseline(source: EnergySource) -> bool {
    source != EnergySource::None
}

pub(crate) fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, run_benchmark};
    use crate::{Report, Status};

    #[test]
    fn scenario_lists() {
        assert_eq!(
            Scenario::parse_list("idle, index_1k,poll,idle").unwrap(),
            [Scenario::DaemonIdle, Scenario::Index1k, Scenario::A11yPoll]
        );
        assert_eq!(Scenario::parse_list("all").unwrap(), Scenario::ALL);
        assert_eq!(
            Scenario::parse_list("prefill,all").unwrap()[..2],
            [Scenario::Prefill, Scenario::DaemonIdle]
        );
        assert!(
            Scenario::parse_list("idle,bogus")
                .unwrap_err()
                .contains("unknown energy scenario `bogus`")
        );
        assert!(Scenario::parse_list(" , ").is_err());
        for s in Scenario::ALL {
            assert!(crate::runner::valid_metric_name(&format!(
                "energy.{}.x",
                s.as_str()
            )));
        }
    }

    #[test]
    fn scales() {
        let smoke = EnergyParams::for_scale(Scale::Smoke);
        let full = EnergyParams::for_scale(Scale::Full);
        assert!(smoke.idle_s <= 5.0 && smoke.poll_s <= 5.0);
        assert_eq!(full.idle_s, 600.0, "the PLAN's ten idle minutes");
        assert_eq!(EnergyParams::for_scale(Scale::Quick).index_items, 1_000);
    }

    /// Without `NUGGET_BENCH_SHELL` (unit tests never launch a desktop).
    #[test]
    fn shell_scenarios_are_recorded_as_skipped() {
        if std::env::var_os(SHELL_ENV).is_some() {
            eprintln!("skipping: {SHELL_ENV} is set");
            return;
        }
        let r: Report = run_benchmark(
            &Only(
                EnergyParams::for_scale(Scale::Smoke),
                vec![Scenario::ShellIdle, Scenario::ShellFocus],
            ),
            &RunConfig {
                scale: Scale::Smoke,
                ..RunConfig::default()
            },
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert!(r.metrics.is_empty());
        let p = &r.config.params;
        assert!(p["shell_idle"].as_str().unwrap().starts_with("skipped"));
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(p["shell_focus"].as_str().unwrap().contains(SHELL_ENV));
        }
        assert_eq!(
            p["scenarios"],
            serde_json::json!(["shell_idle", "shell_focus"])
        );
    }

    /// The daemon and poll scenarios at a tiny size against the `nuggetd` built next to this
    /// test binary (CI's workspace build has one); the daemon ones are skipped when there is
    /// none, and `prefill` without `--local-models`.
    #[test]
    fn energy_smoke() {
        let have_daemon = match find_nuggetd() {
            Ok(_) => true,
            Err(why) => {
                eprintln!("daemon scenarios will be skipped: {why}");
                false
            }
        };
        let p = EnergyParams {
            idle_items: 100,
            settle_s: 0.5,
            idle_s: 1.0,
            index_items: 30,
            poll_s: 1.0,
            ..EnergyParams::for_scale(Scale::Smoke)
        };
        let report: Report = run_benchmark(
            &Only(p, Scenario::DEFAULT.to_vec()),
            &RunConfig {
                scale: Scale::Smoke,
                seed: 7,
                ..RunConfig::default()
            },
        );
        // A `nuggetd` left over from an older build of a changed store cannot open the seeded
        // one; that says nothing about the suite (a workspace build has a matching binary).
        let stale = report
            .message
            .as_deref()
            .is_some_and(|m| m.contains("newer than this build supports"));
        if stale {
            eprintln!("skipping: stale nuggetd binary: {:?}", report.message);
            return;
        }
        assert_eq!(report.status, Status::Ok, "{:?}", report.message);
        let params = &report.config.params;
        let v = |n: &str| {
            report
                .metric(n)
                .unwrap_or_else(|| panic!("missing {n}"))
                .value
        };
        if have_daemon {
            assert!(v("energy.daemon_idle.window_s") >= 1.0);
            assert!(v("energy.index_1k.wall_s") > 0.0);
            assert_eq!(v("energy.index_1k.items"), 30.0);
            if cfg!(target_os = "linux") {
                assert!(v("energy.daemon_idle.cpu_percent") >= 0.0);
                assert!(v("energy.daemon_idle.wakeups_per_s") >= 0.0);
                assert!(v("energy.index_1k.cpu_s_per_1k_items") > 0.0);
                assert_eq!(params["daemon_idle.process_source"], "proc");
            }
        } else {
            assert!(
                params["daemon_idle"]
                    .as_str()
                    .unwrap()
                    .starts_with("skipped")
            );
        }
        if cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )) {
            assert!(v("energy.a11y_poll.calls") >= 1.0);
        }
        assert!(params["prefill"].as_str().unwrap().starts_with("skipped"));
        let source = params["energy_source"].as_str().unwrap();
        assert!(["rapl", "powermetrics", "none"].contains(&source));
        if source == "none" {
            assert!(report.metric("energy.daemon_idle.package_w").is_none());
        }
    }

    struct Only(EnergyParams, Vec<Scenario>);

    impl Benchmark for Only {
        fn name(&self) -> &'static str {
            "energy"
        }
        fn description(&self) -> &'static str {
            "selected energy scenarios"
        }
        fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            run_with(ctx, &self.0, &self.1)
        }
    }
}
