//! `perf --backend real --perf-phases sync`: a source's **first sync** through the real
//! `nuggetd` (LL1).
//!
//! The daemon starts on an empty home (same hermetic config as the daemon profile: no
//! providers, so search is text-only and knowledge extraction deterministic). `N` Markdown files
//! (`--perf-items`, size mix `--perf-sizes`, 1,000 per folder) are written first; then a files
//! source is added over them and the clock runs until `ingest.status` reports all `N` items
//! indexed with an empty queue and the last file is found by `search` (`daemon.sync.*`). The
//! daemon's own freshness window (change observed → committed) is read at that point. Then
//! live freshness: files are written one at a time into the watched folder, each timed from the
//! write until `search` finds its unique marker (`daemon.sync.live.*`). Knowledge extraction
//! runs alongside, as in the app.

use std::path::Path;
use std::time::{Duration, Instant};

use nugget_core::ipc::{IngestStatus, method};
use nugget_core::paths::Paths;
use serde_json::json;

use crate::corpus::{Corpus, CorpusConfig, marker};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext, Scale};
use crate::stats::summarize;
use crate::suites::perf_daemon::{
    Daemon, Env, find_nuggetd, search, seed_home, wait_for, watch_folder, write_config,
};

/// The marker version of the synced files (`marker(i, SYNC_VERSION)` finds file `i`).
const SYNC_VERSION: u32 = 11;
/// The marker version of the live-freshness files.
const LIVE_VERSION: u32 = 12;
/// Files per folder in the synced tree.
const PER_DIR: usize = 1_000;

/// Workload of the first-sync run.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncParams {
    pub files: usize,
    pub words_per_doc: usize,
    pub length_sigma: f64,
    /// Files written one by one after the sync, for live freshness.
    pub live_files: usize,
    pub timeout_s: f64,
}

impl SyncParams {
    pub fn for_scale(scale: Scale) -> Self {
        Self {
            files: scale.pick(300, 2_000, 10_000, 100_000),
            words_per_doc: 120,
            length_sigma: 0.0,
            live_files: scale.pick(5, 20, 30, 50),
            timeout_s: scale.pick(180.0, 600.0, 1_800.0, 3_600.0),
        }
    }
}

/// Entry point: the run at `--scale` (or `--perf-items`), or nothing when no binary is found.
pub fn run(ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
    let exe = match find_nuggetd() {
        Ok(exe) => exe,
        Err(why) => {
            eprintln!("note: first-sync run skipped: {why}");
            ctx.param("sync", format!("skipped: {why}"));
            return Ok(Vec::new());
        }
    };
    let mut p = SyncParams::for_scale(ctx.scale());
    let opts = ctx.config.perf.clone();
    if let Some(n) = opts.perf_items {
        p.timeout_s = p.timeout_s.max(n as f64 / 20.0);
        p.files = n;
    }
    (p.words_per_doc, p.length_sigma) = opts.perf_sizes.shape(p.words_per_doc);
    run_with(ctx, &p, &exe)
}

pub fn run_with(ctx: &mut RunContext, p: &SyncParams, exe: &Path) -> Result<Vec<Metric>, BenchError> {
    if p.files == 0 {
        return Err(BenchError::Failed("the first-sync run needs files".into()));
    }
    ctx.param("sync", "measured");
    ctx.param("sync_files", p.files);
    ctx.param("sync_words_per_doc", p.words_per_doc);
    ctx.param("sync_length_sigma", p.length_sigma);
    ctx.param("sync_live_files", p.live_files);
    let seed = ctx.seed();
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-sync-")
        .tempdir()?;
    let home = tmp.path().join("home");
    let paths = Paths::rooted(home.clone());
    seed_home(&paths, &Corpus::generate(CorpusConfig { docs: 0, ..CorpusConfig::default() }), 384, seed)?;
    let tree = tmp.path().join("tree");
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.files,
        words_per_doc: p.words_per_doc,
        length_sigma: p.length_sigma,
        seed: seed ^ 0x5F1C_0001,
        ..CorpusConfig::default()
    });
    let mut bytes = 0usize;
    for doc in &corpus.docs {
        let dir = tree.join(format!("d{:04}", doc.id / PER_DIR));
        if doc.id % PER_DIR == 0 {
            std::fs::create_dir_all(&dir)?;
        }
        let text = format!(
            "# {}\n\n{}\n\n{}\n",
            doc.title,
            doc.body,
            marker(doc.id, SYNC_VERSION)
        );
        bytes += text.len();
        std::fs::write(dir.join(format!("note-{:06}.md", doc.id)), text)?;
    }
    ctx.param("sync_bytes", bytes);
    let env = Env {
        exe: exe.to_path_buf(),
        home,
        paths,
        work: tmp.path().to_path_buf(),
        timeout: Duration::from_secs_f64(p.timeout_s),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut out = Vec::new();
    rt.block_on(profile(&env, p, &tree, &mut out))?;
    Ok(out)
}

async fn profile(
    env: &Env,
    p: &SyncParams,
    tree: &Path,
    out: &mut Vec<Metric>,
) -> Result<(), BenchError> {
    let call_timeout = Duration::from_secs(60).min(env.timeout);
    write_config(&env.paths, None)?;
    let mut d = Daemon::spawn(env, "sync")?;
    let (_, client) = d.ready(env, Instant::now()).await?;
    let c = &client;

    let t = Instant::now();
    let src = watch_folder(&mut d, c, tree, env.timeout, Duration::from_millis(50)).await?;
    let src = &src;
    let want = p.files as u64;
    let probe = &marker(p.files - 1, SYNC_VERSION);
    let st: IngestStatus = wait_for(&mut d, env.timeout, "the first sync", || async move {
        let st: IngestStatus = crate::suites::perf_daemon::call(
            c,
            method::INGEST_STATUS,
            json!({}),
            call_timeout,
        )
        .await?;
        let indexed = st.sources.iter().any(|s| s.id == *src && s.items >= want);
        if !(indexed && st.queue_depth == 0) {
            return Ok(None);
        }
        let hits = search(c, probe, call_timeout).await?;
        Ok(hits.iter().any(|h| h.item.source == *src).then_some(st))
    })
    .await?;
    let wall = t.elapsed().as_secs_f64().max(1e-9);
    out.extend([
        Metric::new(
            "daemon.sync.items_per_s",
            p.files as f64 / wall,
            "items/s",
            Direction::HigherIsBetter,
            "First sync: Markdown files already on disk, from adding the files source until \
             `ingest.status` reports all indexed with an empty queue and the last one is found \
             by `search` (text-only: no embedding provider; knowledge extraction runs alongside)",
        ),
        Metric::new(
            "daemon.sync.wall_s",
            wall,
            "s",
            Direction::LowerIsBetter,
            "First sync wall time (see daemon.sync.items_per_s)",
        ),
    ]);
    if let Some(ms) = st.freshness_p50_ms {
        out.push(Metric::new(
            "daemon.sync.freshness_p50_ms",
            ms as f64,
            "ms",
            Direction::LowerIsBetter,
            "Freshness (change observed by the pipeline → committed) over the daemon's recent \
             window at the end of the first sync, median (`ingest.status`)",
        ));
    }
    if let Some(ms) = st.freshness_p95_ms {
        out.push(Metric::new(
            "daemon.sync.freshness_p95_ms",
            ms as f64,
            "ms",
            Direction::LowerIsBetter,
            "As daemon.sync.freshness_p50_ms, 95th percentile",
        ));
    }

    // Live freshness: one file at a time into the watched tree.
    let live_dir = tree.join("live");
    std::fs::create_dir_all(&live_dir)?;
    let mut lat = Vec::with_capacity(p.live_files);
    for i in 0..p.live_files {
        let m = marker(i, LIVE_VERSION);
        let t = Instant::now();
        std::fs::write(
            live_dir.join(format!("live-{i:03}.md")),
            format!("# Live note {i}\n\nWritten while watching.\n\n{m}\n"),
        )?;
        let m = &m;
        wait_for(&mut d, env.timeout, "a live file", || async move {
            let hits = search(c, m, call_timeout).await?;
            Ok(hits.iter().any(|h| h.item.source == *src).then_some(()))
        })
        .await?;
        lat.push(t.elapsed().as_secs_f64() * 1e3);
    }
    if let Some(s) = summarize(&lat) {
        out.extend([
            Metric::new(
                "daemon.sync.live.freshness_p50_ms",
                s.p50,
                "ms",
                Direction::LowerIsBetter,
                "After the first sync: a file written into the watched folder until `search` \
                 finds it (watcher debounce included), median",
            ),
            Metric::new(
                "daemon.sync.live.freshness_p95_ms",
                s.p95,
                "ms",
                Direction::LowerIsBetter,
                "As daemon.sync.live.freshness_p50_ms, 95th percentile",
            ),
        ]);
    }
    drop(client);
    d.stop().await?;
    Ok(())
}
