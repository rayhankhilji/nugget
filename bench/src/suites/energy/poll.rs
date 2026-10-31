//! `a11y_poll`: the desktop shell's once-a-second "what is in front" read, alone, in process.
//!
//! The shell's Double Tap flow (`apps/desktop/src-tauri/src/doubletap/flow.rs`, `A11y::new` and
//! `A11y::frontmost`) keeps one `nugget_a11y::Backend` over the platform API (AT-SPI on Linux,
//! AX on macOS, UI Automation on Windows) with the default config and calls `frontmost_app()`
//! each second. This scenario does the same: one untimed first call (it connects to the
//! accessibility bus), then two arms of equal length on the same 1 Hz schedule, metered on this
//! process: one that only sleeps, then one that calls `frontmost_app()` on every tick. The
//! difference is the poll's cost. On a headless runner the call may find no app or fail (no
//! accessibility bus): that is recorded, and the cost reported is that of the failing call.
//! Work done in other processes (the AT-SPI registry, the focused app answering) counts only in
//! system energy.

use std::time::{Duration, Instant};

use super::meter::Meter;
use super::{EnergyParams, Scenario, energy_metrics, record, window_metrics};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext};
use crate::stats::summarize;

/// What one call returned.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
#[derive(Debug, Clone, PartialEq)]
enum Polled {
    App,
    NoApp,
    Error(String),
}

/// The frontmost-app read, built as the shell builds it.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn frontmost_reader() -> (&'static str, impl FnMut() -> Polled) {
    #[cfg(target_os = "linux")]
    let (api, backend) = (
        "AT-SPI",
        nugget_a11y::Backend::new(
            nugget_a11y::AtspiPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        ),
    );
    #[cfg(target_os = "macos")]
    let (api, backend) = (
        "AX",
        nugget_a11y::Backend::new(
            nugget_a11y::AxPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        ),
    );
    #[cfg(target_os = "windows")]
    let (api, backend) = (
        "UI Automation",
        nugget_a11y::Backend::new(
            nugget_a11y::UiaPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        ),
    );
    (api, move || match backend.frontmost_app() {
        Ok(Some(_)) => Polled::App,
        Ok(None) => Polled::NoApp,
        Err(e) => Polled::Error(e.to_string()),
    })
}

/// Run `ticks` ticks one second apart from now, calling `f` on each if given; returns the wall
/// time of each call.
fn at_1hz(
    ticks: usize,
    mut f: Option<&mut dyn FnMut() -> Polled>,
    seen: &mut Vec<Polled>,
) -> Vec<f64> {
    let start = Instant::now();
    let mut ms = Vec::new();
    for i in 0..ticks {
        if let Some(f) = f.as_mut() {
            let t = Instant::now();
            seen.push(f());
            ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let next = start + Duration::from_secs(i as u64 + 1);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    ms
}

pub(super) fn a11y_poll(ctx: &mut RunContext, p: &EnergyParams) -> Result<Vec<Metric>, BenchError> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let (api, mut read) = frontmost_reader();
        run(ctx, p, api, &mut read)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = p;
        super::skip(
            ctx,
            Scenario::A11yPoll,
            "no accessibility backend on this platform",
        );
        Ok(Vec::new())
    }
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
fn run(
    ctx: &mut RunContext,
    p: &EnergyParams,
    api: &str,
    read: &mut dyn FnMut() -> Polled,
) -> Result<Vec<Metric>, BenchError> {
    let name = Scenario::A11yPoll.as_str();
    let ticks = (p.poll_s.round() as usize).max(1);
    ctx.param(name, "measured");
    ctx.param(
        &format!("{name}.call"),
        format!(
            "nugget_a11y::Backend::new({api} platform, A11yConfig::default()).frontmost_app(), \
             as the desktop shell's A11y::frontmost, at 1 Hz in this process"
        ),
    );
    ctx.param(&format!("{name}.ticks"), ticks);

    let t = Instant::now();
    let first = read();
    let first_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut seen = Vec::new();
    let meter = Meter::start(&[std::process::id()]);
    at_1hz(ticks, None, &mut seen);
    let sleep = meter.stop();
    let meter = Meter::start(&[std::process::id()]);
    let call_ms = at_1hz(ticks, Some(read), &mut seen);
    let poll = meter.stop();
    record(ctx, name, &poll, Some(&sleep));

    let found = seen.iter().filter(|s| **s == Polled::App).count();
    let errors: Vec<&str> = seen
        .iter()
        .filter_map(|s| match s {
            Polled::Error(e) => Some(e.as_str()),
            _ => None,
        })
        .collect();
    ctx.param(
        &format!("{name}.result"),
        match (&first, found, errors.first()) {
            (_, n, _) if n > 0 => format!("an app on {n} of {} calls", seen.len()),
            (_, _, Some(e)) => {
                format!("error (no app read; the failing call is what is metered): {e}")
            }
            (Polled::Error(e), _, None) => {
                format!("first call failed: {e}; later calls found no app")
            }
            _ => "no frontmost app (nothing focused, e.g. a headless runner)".to_owned(),
        },
    );

    let prefix = format!("energy.{name}");
    let calls = call_ms.len().max(1) as f64;
    let mut out = vec![
        Metric::new(
            format!("{prefix}.calls"),
            call_ms.len() as f64,
            "count",
            Direction::Info,
            "frontmost_app() calls in the polling window (1 Hz)",
        ),
        Metric::new(
            format!("{prefix}.frontmost_found"),
            found as f64,
            "count",
            Direction::Info,
            "Calls that returned an app (0 on a headless runner)",
        ),
        Metric::new(
            format!("{prefix}.errors"),
            errors.len() as f64,
            "count",
            Direction::Info,
            "Calls that returned an error",
        ),
        Metric::new(
            format!("{prefix}.first_call_ms"),
            first_ms,
            "ms",
            Direction::Info,
            "The untimed first call (connects to the accessibility API; once per shell run)",
        ),
    ];
    if let Some(s) = summarize(&call_ms) {
        for (q, v) in [("p50", s.p50), ("p95", s.p95)] {
            out.push(Metric::new(
                format!("{prefix}.call_ms.{q}"),
                v,
                "ms",
                Direction::LowerIsBetter,
                format!("{q} wall time of one frontmost_app() call"),
            ));
        }
    }
    let added = |a: Option<f64>, b: Option<f64>| Some(a? - b?);
    if let Some(c) = added(poll.cpu_s, sleep.cpu_s) {
        out.push(Metric::new(
            format!("{prefix}.cpu_ms_per_call"),
            (c * 1e3 / calls).max(0.0),
            "ms",
            Direction::LowerIsBetter,
            "CPU time per call: this process's CPU while polling minus while sleeping, over the \
             calls (floored at 0)",
        ));
    }
    if let Some(c) = added(poll.cpu_percent(), sleep.cpu_percent()) {
        out.push(Metric::new(
            format!("{prefix}.cpu_percent_added"),
            c,
            "%",
            Direction::LowerIsBetter,
            "Share of one core the 1 Hz poll adds (polling minus sleeping window)",
        ));
    }
    if let Some(w) = added(poll.wakeups_per_s(), sleep.wakeups_per_s()) {
        out.push(Metric::new(
            format!("{prefix}.wakeups_per_s_added"),
            w,
            "1/s",
            Direction::LowerIsBetter,
            "Wakeups per second the poll adds to this process (polling minus sleeping window)",
        ));
    }
    out.extend(window_metrics(
        &format!("{prefix}.sleep"),
        "this process sleeping at 1 Hz",
        &sleep,
    ));
    // The sleeping arm is the baseline here: attributable_w is the poll's added power.
    out.extend(energy_metrics(
        &prefix,
        "the polling window",
        &poll,
        Some(&sleep),
    ));
    if let Some(j) = poll.attributable_j(&sleep) {
        out.push(Metric::new(
            format!("{prefix}.attributable_mj_per_call"),
            j * 1e3 / calls,
            "mJ",
            Direction::LowerIsBetter,
            "Package energy per call above the sleeping window (noisy on shared VMs)",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, Scale};

    #[test]
    fn scripted_reader_counts_results() {
        let mut ctx = RunContext::new(RunConfig {
            scale: Scale::Smoke,
            ..RunConfig::default()
        });
        let p = EnergyParams {
            poll_s: 2.0,
            ..EnergyParams::for_scale(Scale::Smoke)
        };
        let mut n = 0;
        let mut read = || {
            n += 1;
            match n {
                1 => Polled::Error("no bus".into()),
                2 => Polled::App,
                _ => Polled::NoApp,
            }
        };
        let out = run(&mut ctx, &p, "scripted", &mut read).unwrap();
        let v = |name: &str| {
            out.iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .value
        };
        assert_eq!(v("energy.a11y_poll.calls"), 2.0);
        assert_eq!(v("energy.a11y_poll.frontmost_found"), 1.0);
        assert_eq!(v("energy.a11y_poll.errors"), 0.0);
        assert!(
            ctx.params()["a11y_poll.result"]
                .as_str()
                .unwrap()
                .starts_with("an app on 1 of 2")
        );
        if cfg!(target_os = "linux") {
            assert!(v("energy.a11y_poll.cpu_ms_per_call") >= 0.0);
            assert!(v("energy.a11y_poll.sleep.window_s") >= 2.0);
        }
    }
}
