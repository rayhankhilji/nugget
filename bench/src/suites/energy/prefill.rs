//! `prefill` (`--local-models` only): what a Double Tap read-ahead costs in energy.
//!
//! The real local chat model runs in `llama-server` as the app runs it ([`LocalModels`], with a
//! fixed sampling seed and temperature 0). Drafters are built as the `doubletap` suite builds
//! them (one per persona over the `draft` fixture corpus, text-only retrieval) with read-ahead
//! on and no minimum interval. After [`Drafters::prewarm_idle`](crate::suites::doubletap::Drafters)
//! (the fixed rules prefix, as the daemon does) and one unmetered read-ahead, N read-aheads of
//! distinct fixture tasks run back to back through [`nugget_draft::Drafter::read_ahead`], each
//! waited for, under one [`Meter`] on the `llama-server` process. With an energy source, an
//! equal window with the server loaded but idle is metered afterwards as the baseline, so the
//! attributable energy is the prefill work itself.

use std::sync::Arc;
use std::time::Duration;

use nugget_core::egress::NullEgress;
use nugget_core::ipc::RuntimeRole;
use nugget_draft::DraftConfig;
use nugget_infer::build_provider_with;

use super::meter::{EnergySource, Meter, baseline};
use super::{EnergyParams, Scenario, energy_metrics, record, skip, wants_baseline};
use crate::local::LocalModels;
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext};
use crate::suites::doubletap::{
    DRAFT_TIMEOUT, Probe, ReadAheadResult, build_drafters, build_stores, read_ahead_task, spread,
    with_seed,
};
use crate::suites::draft::fixtures::Corpus;

pub(super) fn prefill(
    ctx: &mut RunContext,
    p: &EnergyParams,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    let name = Scenario::Prefill.as_str();
    let local = ctx.config.local.clone();
    if !local.local_models {
        skip(ctx, Scenario::Prefill, "needs --local-models");
        return Ok(Vec::new());
    }
    let models = LocalModels::start(&local, &[RuntimeRole::Chat])?;
    models.record(ctx);
    let seed = ctx.seed();
    let mut cfg = models
        .chat_config(models.thinking)
        .ok_or_else(|| BenchError::Failed("no local chat model was started".into()))?;
    with_seed(&mut cfg, seed);
    let provider = build_provider_with(&cfg, Arc::new(NullEgress), &models.secrets())?;
    let Some(pid) = models.server_pid(RuntimeRole::Chat) else {
        skip(ctx, Scenario::Prefill, "the chat server reports no pid");
        return Ok(Vec::new());
    };

    let corpus = Corpus::load().map_err(BenchError::Failed)?;
    let n = p.prefill_runs.max(1);
    // One more task than metered: the first is the unmetered warm-up.
    let tasks = spread(corpus.tasks.len(), n + 1);
    let mut config = DraftConfig::default();
    config.local.enabled = ctx.config.draft.draft_local_budget.is_on();
    config.temperature = Some(0.0);
    config.read_ahead.enabled = true;
    config.read_ahead.min_interval_ms = 0;
    let stores = build_stores(&corpus)?;
    let drafters = build_drafters(&corpus, &stores, provider, &config);
    let probe = Probe {
        read_ahead: true,
        server_pid: Some(pid),
    };
    ctx.param(name, "measured");
    ctx.param(&format!("{name}.model"), models.chat_label());
    ctx.param(
        &format!("{name}.flow"),
        "doubletap's drafters (draft fixture corpus, text-only retrieval), read-ahead on with \
         min_interval_ms 0; Drafter::prewarm_idle and one read-ahead unmetered, then N \
         read-aheads of distinct tasks back to back, each waited for, metered on llama-server",
    );
    ctx.param(
        &format!("{name}.read_aheads"),
        tasks.len().saturating_sub(1),
    );
    ctx.param(&format!("{name}.temperature"), 0.0);
    ctx.param(&format!("{name}.seed"), seed);
    ctx.param(
        &format!("{name}.draft_local_budget"),
        ctx.config.draft.draft_local_budget.as_str(),
    );

    let (results, r) = models.block_on(async {
        let _ = drafters.prewarm_idle(DRAFT_TIMEOUT).await;
        let mut results = Vec::new();
        let mut meter = None;
        for (i, &ti) in tasks.iter().enumerate() {
            if i == 1 {
                meter = Some(Meter::start(&[pid]));
            }
            let res = read_ahead_task(&drafters, &corpus.tasks[ti], DRAFT_TIMEOUT, &probe).await;
            eprintln!(
                "energy prefill: {}/{} {}",
                i + 1,
                tasks.len(),
                describe(&res)
            );
            if i > 0 {
                results.push(res);
            }
        }
        (results, meter.map(Meter::stop))
    });
    let Some(r) = r else {
        skip(
            ctx,
            Scenario::Prefill,
            "the fixture corpus has a single task",
        );
        return Ok(Vec::new());
    };
    let base = wants_baseline(source).then(|| baseline(Duration::from_secs_f64(r.wall_s)));
    drop(models);
    record(ctx, name, &r, base.as_ref());

    let walls: Vec<f64> = results
        .iter()
        .filter_map(|x| match x {
            ReadAheadResult::Done { wall_ms, .. } => Some(*wall_ms as f64 / 1e3),
            _ => None,
        })
        .collect();
    let done = walls.len();
    let skipped = results
        .iter()
        .filter(|x| matches!(x, ReadAheadResult::Skipped { .. }))
        .count();
    let prefix = format!("energy.{name}");
    let mut out = vec![
        Metric::new(
            format!("{prefix}.completed"),
            done as f64,
            "count",
            Direction::Info,
            "Read-aheads that completed in the metered window",
        ),
        Metric::new(
            format!("{prefix}.skipped"),
            skipped as f64,
            "count",
            Direction::LowerIsBetter,
            "Read-aheads the drafter skipped",
        ),
        Metric::new(
            format!("{prefix}.timeouts"),
            (results.len() - done - skipped) as f64,
            "count",
            Direction::LowerIsBetter,
            "Read-aheads that timed out",
        ),
    ];
    if done == 0 {
        ctx.param(&format!("{name}.error"), "no read-ahead completed");
        return Ok(out);
    }
    let per = 1.0 / done as f64;
    out.push(Metric::new(
        format!("{prefix}.seconds_per_read_ahead"),
        walls.iter().sum::<f64>() * per,
        "s",
        Direction::LowerIsBetter,
        "Mean wall time of a completed read-ahead (prompt build, retrieval and prefill)",
    ));
    if let Some(c) = r.cpu_s {
        out.push(Metric::new(
            format!("{prefix}.cpu_s_per_read_ahead"),
            c * per,
            "s",
            Direction::LowerIsBetter,
            "llama-server CPU time over the metered window per completed read-ahead",
        ));
    }
    if let Some(w) = r.wakeups {
        out.push(Metric::new(
            format!("{prefix}.wakeups_per_read_ahead"),
            w * per,
            "count",
            Direction::Info,
            "llama-server wakeups per completed read-ahead",
        ));
    }
    if let Some(j) = r.package_j {
        out.push(Metric::new(
            format!("{prefix}.package_j_per_read_ahead"),
            j * per,
            "J",
            Direction::Info,
            "System package energy per read-ahead (the whole machine, baseline not subtracted)",
        ));
    }
    if let Some(j) = base.as_ref().and_then(|b| r.attributable_j(b)) {
        out.push(Metric::new(
            format!("{prefix}.attributable_j_per_read_ahead"),
            j * per,
            "J",
            Direction::LowerIsBetter,
            "Package energy per read-ahead above the baseline (server loaded, idle)",
        ));
    }
    out.extend(energy_metrics(
        &prefix,
        "the read-aheads",
        &r,
        base.as_ref(),
    ));
    Ok(out)
}

fn describe(r: &ReadAheadResult) -> String {
    match r {
        ReadAheadResult::Done { wall_ms, .. } => format!("done in {wall_ms} ms"),
        ReadAheadResult::Skipped { reason } => format!("skipped ({reason})"),
        ReadAheadResult::TimedOut => "timed out".into(),
    }
}
