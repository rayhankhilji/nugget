//! `perf --backend real --perf-phases search`: `search` latency through the real `nuggetd` on a
//! seeded store, without the hour of knowledge extraction the full daemon profile runs first
//! at 500k items (LL1).
//!
//! The store is seeded exactly as the daemon profile seeds it (same corpus, sizes, chunks and
//! seed), then `nuggetd` starts with `[knowledge] enabled = false` (otherwise the same hermetic
//! config: no providers, so search is text-only) and the same known-item queries are sent over
//! IPC: one first search, then `searches` warm ones (`daemon.search.*`, comparable with the
//! daemon profile's), with recall@10 as the quality check. Set `NUGGET_BENCH_SEARCH_HOME` to a
//! directory to keep the seeded home there and reuse it on later runs with the same item count
//! and seed (a 500k-item store takes minutes to seed and ~2 GB).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nugget_core::ipc::{StatusResponse, method};
use nugget_core::paths::Paths;
use serde_json::json;

use crate::corpus::{Corpus, CorpusConfig};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext};
use crate::suites::item_ref;
use crate::suites::perf::{latency_metrics, us};
use crate::suites::perf_daemon::{
    Daemon, DaemonParams, Env, call, find_nuggetd, sample, search, seed_home,
};

/// Keep (and reuse) the seeded home in this directory.
pub const SEARCH_HOME_ENV: &str = "NUGGET_BENCH_SEARCH_HOME";

pub fn run(ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
    let exe = match find_nuggetd() {
        Ok(exe) => exe,
        Err(why) => {
            eprintln!("note: search run skipped: {why}");
            ctx.param("search", format!("skipped: {why}"));
            return Ok(Vec::new());
        }
    };
    let mut p = DaemonParams::for_scale(ctx.scale());
    let opts = ctx.config.perf.clone();
    if let Some(n) = opts.perf_items {
        p.items = n;
    }
    (p.words_per_doc, p.length_sigma) = opts.perf_sizes.shape(p.words_per_doc);
    ctx.param("search", "measured");
    ctx.param("search_items", p.items);
    ctx.param("search_sizes", opts.perf_sizes.as_str());
    ctx.param("search_searches", p.searches);
    let seed = ctx.seed();
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.items,
        words_per_doc: p.words_per_doc,
        length_sigma: p.length_sigma,
        seed,
        ..CorpusConfig::default()
    });
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-search-")
        .tempdir()?;
    let home = match std::env::var_os(SEARCH_HOME_ENV) {
        Some(dir) => {
            let dir = PathBuf::from(dir).join(format!(
                "home-{}-{}-{seed}",
                p.items,
                opts.perf_sizes.as_str()
            ));
            std::fs::create_dir_all(&dir)?;
            dir
        }
        None => tmp.path().join("home"),
    };
    let paths = Paths::rooted(home.clone());
    if std::env::var_os(SEARCH_HOME_ENV).is_some() {
        // For timing the store by hand on the kept home: the queries this run sends.
        let qs = corpus.queries(p.searches + 1, 4, 0.25, seed);
        let text: Vec<&str> = qs.iter().map(|q| q.text.as_str()).collect();
        std::fs::write(home.join("queries.txt"), text.join("\n"))?;
    }
    let mut out = Vec::new();
    if !paths.db_file().is_file() {
        let t = Instant::now();
        seed_home(&paths, &corpus, p.dim, seed)?;
        out.push(Metric::new(
            "daemon.seed_s",
            t.elapsed().as_secs_f64(),
            "s",
            Direction::Info,
            "Writing the seeded store in process before the daemon starts (reference only)",
        ));
    }
    std::fs::write(
        paths.config_file(),
        "[runtime]\nenabled = false\n\n[mcp]\nenabled = false\n\n[vault]\nenabled = false\n\n\
         [knowledge]\nenabled = false\n",
    )?;
    let env = Env {
        exe,
        home,
        paths,
        work: tmp.path().to_path_buf(),
        timeout: Duration::from_secs_f64(p.timeout_s),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(profile(&env, &p, &corpus, seed, &mut out))?;
    Ok(out)
}

async fn profile(
    env: &Env,
    p: &DaemonParams,
    corpus: &Corpus,
    seed: u64,
    out: &mut Vec<Metric>,
) -> Result<(), BenchError> {
    let call_timeout = Duration::from_secs(60).min(env.timeout);
    // The same queries as the daemon profile.
    let queries = corpus.queries(p.searches + 1, 4, 0.25, seed);
    let mut d = Daemon::spawn(env, "search")?;
    let (_, client) = d.ready(env, Instant::now()).await?;
    let status: StatusResponse = call(&client, method::STATUS, json!({}), call_timeout).await?;
    if status.items != p.items as u64 {
        return Err(BenchError::Failed(format!(
            "nuggetd reports {} items, {} were seeded",
            status.items, p.items
        )));
    }
    // Let the startup work settle first (the proactive engine's first run reads the whole
    // store; the deferred integrity check reads the whole file): searched during it, the
    // numbers measure the contention, not search. The daemon profile searches after an hour
    // of extraction, when it has settled too.
    let settled = settle(&mut d, env.timeout).await?;
    out.push(Metric::new(
        "daemon.search.settle_s",
        settled,
        "s",
        Direction::Info,
        "Wait after startup until the daemon used under 5% of a core over 2 s, and at least \
         30 s, before the searches",
    ));
    let t = Instant::now();
    let first = search(&client, &queries[0].text, call_timeout).await?;
    out.push(Metric::new(
        "daemon.first_search_ms",
        t.elapsed().as_secs_f64() * 1e3,
        "ms",
        Direction::LowerIsBetter,
        "The first `search` IPC call once the daemon settled after startup (no knowledge run: \
         disabled in this phase)",
    ));
    let mut found = usize::from(
        first
            .iter()
            .any(|h| h.item == item_ref(queries[0].relevant)),
    );
    let mut lat = Vec::with_capacity(p.searches);
    for q in &queries[1..] {
        let t = Instant::now();
        let hits = search(&client, &q.text, call_timeout).await?;
        lat.push(us(t.elapsed()));
        found += usize::from(hits.iter().any(|h| h.item == item_ref(q.relevant)));
    }
    latency_metrics(
        out,
        "daemon.search.latency",
        "a warm `search` IPC round trip (top 10, text-only: no embedding provider)",
        &lat,
    )?;
    out.push(Metric::new(
        "daemon.search.recall@10",
        found as f64 / queries.len() as f64,
        "ratio",
        Direction::Info,
        "Known-item queries whose target is in the daemon's top 10 (text-only)",
    ));
    drop(client);
    d.stop().await?;
    Ok(())
}

/// Wait until the daemon is idle (under 5% of a core over a 2 s window, and at least 30 s after
/// startup); how long that took.
/// Without a CPU counter (not Linux or macOS), a fixed 30 s.
async fn settle(d: &mut Daemon, timeout: Duration) -> Result<f64, BenchError> {
    const WINDOW: Duration = Duration::from_secs(2);
    const MIN_WAIT: Duration = Duration::from_secs(30);
    let t = Instant::now();
    loop {
        d.check_alive()?;
        let a = sample(d.pid).cpu_s;
        tokio::time::sleep(WINDOW).await;
        let b = sample(d.pid).cpu_s;
        match (a, b) {
            // Not before the deferred integrity check (20 s after startup) had its turn.
            (Some(a), Some(b))
                if (b - a) / WINDOW.as_secs_f64() < 0.05 && t.elapsed() >= MIN_WAIT =>
            {
                break;
            }
            (Some(_), Some(_)) => {}
            _ => {
                tokio::time::sleep(Duration::from_secs(28)).await;
                break;
            }
        }
        if t.elapsed() >= timeout {
            return Err(BenchError::Failed(format!(
                "nuggetd did not settle within {timeout:?} after startup"
            )));
        }
    }
    Ok(t.elapsed().as_secs_f64())
}
