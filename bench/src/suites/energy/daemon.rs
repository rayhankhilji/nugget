//! `daemon_idle` and `index_1k`: the real `nuggetd`, launched and driven as in
//! [`crate::suites::perf_daemon`] (temporary `NUGGET_HOME`, key file, no providers, no runtime,
//! no MCP listener), metered with [`Meter`].
//!
//! The bench polls the daemon every [`POLL`] while it waits (slower than `perf_daemon`'s
//! 20 ms, so the polling is a small part of what is metered); the idle window itself has no
//! IPC traffic.

use std::path::Path;
use std::time::{Duration, Instant};

use nugget_core::ipc::{IngestStatus, KnowledgeStatus, method};
use nugget_core::paths::Paths;
use serde_json::json;

use super::meter::{EnergySource, Meter, baseline};
use super::{EnergyParams, Scenario, energy_metrics, record, wants_baseline, window_metrics};
use crate::corpus::{Corpus, CorpusConfig, marker};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext};
use crate::suites::perf_daemon::{
    Daemon, Env, INGEST_VERSION, call, ingest_texts, search, seed_home, wait_every, watch_folder,
    write_config,
};

/// How often the bench asks the daemon whether a wait is over.
const POLL: Duration = Duration::from_millis(250);
/// As in `perf_daemon`.
const WORDS_PER_DOC: usize = 120;
const DIM: usize = 384;

fn env_for(exe: &Path, work: &Path, timeout_s: f64) -> Env {
    let home = work.join("home");
    Env {
        exe: exe.to_path_buf(),
        paths: Paths::rooted(home.clone()),
        home,
        work: work.to_path_buf(),
        timeout: Duration::from_secs_f64(timeout_s),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, BenchError> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn call_timeout(env: &Env) -> Duration {
    Duration::from_secs(60).min(env.timeout)
}

/// The idle daemon: seeded home (vault on), startup work finished, settled, then a quiet window.
/// With an energy source, an equal baseline window is metered first, before the daemon starts.
pub(super) fn daemon_idle(
    ctx: &mut RunContext,
    p: &EnergyParams,
    exe: &Path,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    let name = Scenario::DaemonIdle.as_str();
    let seed = ctx.seed();
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-energy-")
        .tempdir()?;
    let env = env_for(exe, tmp.path(), p.timeout_s);
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.idle_items,
        words_per_doc: WORDS_PER_DOC,
        seed,
        ..CorpusConfig::default()
    });
    seed_home(&env.paths, &corpus, DIM, seed)?;
    write_config(&env.paths, Some(&env.work.join("vault")))?;
    ctx.param(name, "measured");
    ctx.param(
        &format!("{name}.stack"),
        "nuggetd on a seeded temporary NUGGET_HOME (perf_daemon's seeding: one chunk per item, \
         Markdown with frontmatter), vault on; no providers, [runtime] and [mcp] off; window \
         starts after the startup knowledge run and vault render finished and a settle period, \
         with a connected but silent IPC client",
    );
    ctx.param(&format!("{name}.items"), p.idle_items);
    ctx.param(&format!("{name}.idle_s"), p.idle_s);

    let window = Duration::from_secs_f64(p.idle_s);
    let base = wants_baseline(source).then(|| baseline(window));
    let settle = Duration::from_secs_f64(p.settle_s);
    let r = runtime()?.block_on(async {
        let t0 = Instant::now();
        let mut d = Daemon::spawn(&env, "idle")?;
        let (_, client) = d.ready(&env, t0).await?;
        let (c, ct) = (&client, call_timeout(&env));
        wait_every(&mut d, env.timeout, POLL, "startup work", || async move {
            let k: KnowledgeStatus = call(c, method::KNOWLEDGE_STATUS, json!({}), ct).await?;
            Ok((k.last_run.is_some()
                && k.pending_items == 0
                && k.vault.last_render.is_some()
                && k.vault.pages > 0)
                .then_some(()))
        })
        .await?;
        tokio::time::sleep(settle).await;
        d.check_alive()?;
        let meter = Meter::start(&[d.pid]);
        tokio::time::sleep(window).await;
        let r = meter.stop();
        d.check_alive()?;
        drop(client);
        d.stop().await?;
        Ok::<_, BenchError>(r)
    })?;
    drop(tmp);
    record(ctx, name, &r, base.as_ref());

    let prefix = format!("energy.{name}");
    let what = "the idle daemon";
    let mut out = window_metrics(&prefix, what, &r);
    out.extend(energy_metrics(&prefix, what, &r, base.as_ref()));
    if let Some(j) = base.as_ref().and_then(|b| r.attributable_j(b)) {
        out.push(Metric::new(
            format!("{prefix}.attributable_j"),
            j,
            "J",
            Direction::LowerIsBetter,
            "Idle daemon energy above the baseline over the window (attributable_w times the \
             window; noisy on shared VMs)",
        ));
    }
    Ok(out)
}

/// A fresh daemon (empty store, vault off) indexes `p.index_items` files dropped into a watched
/// folder, metered until all are indexed and searchable. With an energy source, an equal baseline window is metered after
/// the daemon stopped.
pub(super) fn index_1k(
    ctx: &mut RunContext,
    p: &EnergyParams,
    exe: &Path,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    let name = Scenario::Index1k.as_str();
    if p.index_items == 0 {
        return Err(BenchError::Failed("index_1k needs items".into()));
    }
    let seed = ctx.seed();
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-energy-")
        .tempdir()?;
    let env = env_for(exe, tmp.path(), p.timeout_s);
    let empty = Corpus::generate(CorpusConfig {
        docs: 0,
        seed,
        ..CorpusConfig::default()
    });
    seed_home(&env.paths, &empty, DIM, seed)?;
    write_config(&env.paths, None)?;
    ctx.param(name, "measured");
    ctx.param(
        &format!("{name}.stack"),
        "nuggetd on an empty temporary NUGGET_HOME, vault off, no providers (text-only: no \
         embedding model runs), [runtime] and [mcp] off; a files source on an empty folder, \
         then Markdown files written into it; window from the first write until ingest.status \
         shows all indexed with an empty queue and search finds the last file",
    );
    ctx.param(&format!("{name}.items"), p.index_items);
    ctx.param(&format!("{name}.words_per_doc"), WORDS_PER_DOC);

    let texts = ingest_texts(p.index_items, WORDS_PER_DOC, seed);
    let probe = &marker(p.index_items - 1, INGEST_VERSION);
    let settle = Duration::from_secs_f64(p.settle_s);
    let want = p.index_items as u64;
    let r = runtime()?.block_on(async {
        let t0 = Instant::now();
        let mut d = Daemon::spawn(&env, "index")?;
        let (_, client) = d.ready(&env, t0).await?;
        let (c, ct) = (&client, call_timeout(&env));
        tokio::time::sleep(settle).await;
        let folder = env.work.join("inbox");
        let src = watch_folder(&mut d, &client, &folder, env.timeout, POLL).await?;
        let src_id = &src;
        let meter = Meter::start(&[d.pid]);
        for (i, text) in texts.iter().enumerate() {
            std::fs::write(folder.join(format!("note-{i:05}.md")), text)?;
        }
        wait_every(&mut d, env.timeout, POLL, "indexing", || async move {
            let st: IngestStatus = call(c, method::INGEST_STATUS, json!({}), ct).await?;
            let indexed = st
                .sources
                .iter()
                .any(|s| s.id == *src_id && s.items >= want);
            if !(indexed && st.queue_depth == 0) {
                return Ok(None);
            }
            let hits = search(c, probe, ct).await?;
            Ok(hits.iter().any(|h| h.item.source == *src_id).then_some(()))
        })
        .await?;
        let r = meter.stop();
        drop(client);
        d.stop().await?;
        Ok::<_, BenchError>(r)
    })?;
    drop(tmp);
    let base = wants_baseline(source).then(|| baseline(Duration::from_secs_f64(r.wall_s)));
    record(ctx, name, &r, base.as_ref());

    let prefix = format!("energy.{name}");
    let per_1k = 1_000.0 / p.index_items as f64;
    let mut out = vec![
        Metric::new(
            format!("{prefix}.items"),
            p.index_items as f64,
            "count",
            Direction::Info,
            "Files indexed (per-1k figures are scaled from this many)",
        ),
        Metric::new(
            format!("{prefix}.wall_s"),
            r.wall_s,
            "s",
            Direction::LowerIsBetter,
            "Wall time from the first file written until all are indexed and searchable",
        ),
    ];
    if let Some(c) = r.cpu_s {
        out.push(Metric::new(
            format!("{prefix}.cpu_s_per_1k_items"),
            c * per_1k,
            "s",
            Direction::LowerIsBetter,
            "nuggetd CPU time (all threads and child processes) per 1,000 items indexed \
             (text-only: no embedding model)",
        ));
    }
    if let Some(w) = r.wakeups {
        out.push(Metric::new(
            format!("{prefix}.wakeups_per_1k_items"),
            w * per_1k,
            "count",
            Direction::LowerIsBetter,
            "nuggetd wakeups (context switches) per 1,000 items indexed",
        ));
    }
    if let Some(c) = r.cpu_percent() {
        out.push(Metric::new(
            format!("{prefix}.cpu_percent"),
            c,
            "%",
            Direction::Info,
            "nuggetd CPU while indexing, as a share of one core",
        ));
    }
    if let Some(j) = r.package_j {
        out.push(Metric::new(
            format!("{prefix}.package_j_per_1k_items"),
            j * per_1k,
            "J",
            Direction::Info,
            "System package energy during indexing per 1,000 items (the whole machine, \
             baseline not subtracted)",
        ));
    }
    if let Some(j) = base.as_ref().and_then(|b| r.attributable_j(b)) {
        out.push(Metric::new(
            format!("{prefix}.attributable_j_per_1k_items"),
            j * per_1k,
            "J",
            Direction::LowerIsBetter,
            "Battery cost of indexing: package energy above the baseline per 1,000 items \
             (text-only; noisy on shared VMs)",
        ));
    }
    out.extend(energy_metrics(&prefix, "indexing", &r, base.as_ref()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_layout() {
        let e = env_for(Path::new("/x/nuggetd"), Path::new("/w"), 2.0);
        assert_eq!(e.home, Path::new("/w/home"));
        assert_eq!(e.timeout, Duration::from_secs(2));
    }
}
