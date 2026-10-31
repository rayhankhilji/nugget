//! The real daemon at scale: the `daemon.*` metrics of `perf --backend real`.
//!
//! [`crate::suites::scale`] measures the store and retriever in process; this profile launches
//! the `nuggetd` binary on a pre-seeded home and drives it only through its public IPC, so it
//! sees what a user sees: process start, the knowledge worker, the vault, the files watcher and
//! everything else the daemon runs in the background.
//!
//! - **Seeding.** A temporary `NUGGET_HOME` gets a hex key file (`data/store.key`, the
//!   `NUGGET_KEYCHAIN=off` path, so no OS keychain is touched) and an encrypted store written
//!   directly with [`Store::upsert_item_with_chunks`]: one chunk per synthetic item with a
//!   deterministic [`HashEmbedder`] vector, typed as Markdown with frontmatter (a project, two
//!   people, a topic tag) so the deterministic knowledge extractors find entities. The config
//!   has no model providers, no on-device runtime and no MCP listener: search is text-only and
//!   extraction is deterministic (no LLM), which keeps the profile hermetic.
//! - **First start** (vault off): startup (spawn to the first `status` answer), the first
//!   `search`, then the startup knowledge run over the whole corpus (`knowledge.status` until
//!   its last run leaves nothing pending), then warm searches.
//! - **Second start** (vault on): startup again, the first full vault render (the vault has
//!   never been rendered, so every entity page is written), then idle CPU and wakeups over a
//!   quiet window with no IPC traffic, then ingest through the daemon: files dropped into a
//!   watched folder until all are indexed and searchable.
//!
//! The binary is `$NUGGET_BENCH_NUGGETD`, else `nuggetd` next to the running executable (or one
//! directory up, for test executables under `target/<profile>/deps`). When there is none, the
//! profile is skipped (recorded in `config.params.daemon`), not failed.
//!
//! CPU, wakeups and peak RSS come from `/proc` on Linux (every thread of the process: CPU from
//! `schedstat`, wakeups as voluntary plus involuntary context switches); macOS reports RSS and
//! CPU time from `ps`; other platforms skip the process metrics.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nugget_core::ids::SourceId;
use nugget_core::ipc::{
    IngestStatus, KnowledgeStatus, SearchHit, SourceInfo, SourceState, StatusResponse, method,
};
use nugget_core::paths::Paths;
use nugget_ipc::Client;
use nugget_retrieve::eval::HashEmbedder;
use nugget_store::{NewChunk, NewItem, Store, StoreKey, UpsertOutcome, content_hash};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::corpus::{Corpus, CorpusConfig, Rng, marker, word};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext, Scale};
use crate::suites::perf::{latency_metrics, us};
use crate::suites::{SYNTHETIC_SOURCE, doc_text, item_ref};

/// Environment variable naming the `nuggetd` binary to profile.
pub const NUGGETD_ENV: &str = "NUGGET_BENCH_NUGGETD";

/// Version used for the markers of ingested files, so they never collide with seeded items.
pub(crate) const INGEST_VERSION: u32 = 9;
/// How often readiness conditions are polled.
const POLL: Duration = Duration::from_millis(20);

/// Workload of the daemon profile.
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonParams {
    /// Items seeded into the store before the daemon starts.
    pub items: usize,
    pub words_per_doc: usize,
    pub dim: usize,
    /// Warm `search` calls after extraction (plus the first one).
    pub searches: usize,
    /// Quiet time after the vault render before the idle window. Long enough at real scales
    /// for post-render work and Tokio's idle blocking threads (10 s keep-alive) to wind down;
    /// at `smoke` the idle numbers still include some of that.
    pub settle_s: f64,
    /// Length of the idle window.
    pub idle_s: f64,
    /// Files dropped into the watched folder.
    pub ingest_files: usize,
    /// Upper bound on each wait (startup, extraction, render, ingest).
    pub timeout_s: f64,
}

impl DaemonParams {
    pub fn for_scale(scale: Scale) -> Self {
        Self {
            items: scale.pick(300, 2_000, 10_000, 100_000),
            words_per_doc: 120,
            dim: 384,
            searches: scale.pick(20, 100, 200, 300),
            settle_s: scale.pick(1.0, 5.0, 10.0, 15.0),
            idle_s: scale.pick(3.0, 5.0, 10.0, 30.0),
            ingest_files: scale.pick(50, 200, 500, 2_000),
            timeout_s: scale.pick(180.0, 300.0, 1_200.0, 3_600.0),
        }
    }
}

/// The `nuggetd` binary to profile, or why there is none.
pub fn find_nuggetd() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(NUGGETD_ENV) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("{NUGGETD_ENV}={} is not a file", p.display()))
        };
    }
    let name = format!("nuggetd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
    let mut tried = Vec::new();
    for dir in exe.ancestors().skip(1).take(2) {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(format!(
        "no nuggetd binary (looked for {}; build it with `cargo build -p nuggetd` in the same \
         profile, or set {NUGGETD_ENV})",
        tried.join(", ")
    ))
}

/// Entry point for `perf --backend real`: the profile at `--scale`, or nothing when no binary
/// is found.
pub fn run(ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
    match find_nuggetd() {
        Ok(exe) => run_with(ctx, &DaemonParams::for_scale(ctx.scale()), &exe),
        Err(why) => {
            eprintln!("note: daemon profile skipped: {why}");
            ctx.param("daemon", format!("skipped: {why}"));
            Ok(Vec::new())
        }
    }
}

pub fn run_with(
    ctx: &mut RunContext,
    p: &DaemonParams,
    exe: &Path,
) -> Result<Vec<Metric>, BenchError> {
    if p.items == 0 || p.searches == 0 || p.ingest_files == 0 {
        return Err(BenchError::Failed(
            "the daemon profile needs items, searches and ingest files".into(),
        ));
    }
    ctx.param("daemon", "measured");
    ctx.param("daemon_exe", exe.display().to_string());
    ctx.param(
        "daemon_stack",
        "nuggetd binary on a temporary NUGGET_HOME (key file, NUGGET_KEYCHAIN=off), driven over \
         IPC; config: no providers (text-only search, deterministic extraction, no LLM), \
         [runtime] and [mcp] off, other sections default; first start with the vault off, \
         second with it on",
    );
    ctx.param("daemon_items", p.items);
    ctx.param("daemon_words_per_doc", p.words_per_doc);
    ctx.param("daemon_embedding_dim", p.dim);
    ctx.param("daemon_searches", p.searches);
    ctx.param("daemon_settle_s", p.settle_s);
    ctx.param("daemon_idle_s", p.idle_s);
    ctx.param("daemon_ingest_files", p.ingest_files);
    ctx.param("daemon_timeout_s", p.timeout_s);

    let seed = ctx.seed();
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-daemon-")
        .tempdir()?;
    let home = tmp.path().join("home");
    let paths = Paths::rooted(home.clone());
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.items,
        words_per_doc: p.words_per_doc,
        seed,
        ..CorpusConfig::default()
    });
    let mut out = Vec::new();
    let t = Instant::now();
    seed_home(&paths, &corpus, p.dim, seed)?;
    out.push(Metric::new(
        "daemon.seed_s",
        t.elapsed().as_secs_f64(),
        "s",
        Direction::Info,
        "Writing the seeded store in process before the daemon starts (embedding included; \
         reference only)",
    ));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let env = Env {
        exe: exe.to_path_buf(),
        home,
        paths,
        work: tmp.path().to_path_buf(),
        timeout: Duration::from_secs_f64(p.timeout_s),
    };
    rt.block_on(profile(&env, p, &corpus, seed, &mut out))?;
    drop(tmp);
    Ok(out)
}

/// Everything a daemon run needs to know about its home. Shared with the `energy` suite.
pub(crate) struct Env {
    pub(crate) exe: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) paths: Paths,
    /// Scratch space next to the home (stderr capture, the watched folder, the vault).
    pub(crate) work: PathBuf,
    pub(crate) timeout: Duration,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A distinct two-word name; the words lie outside the corpus vocabulary.
fn name(base: usize, i: usize) -> String {
    format!(
        "{} {}",
        capitalize(&word(base + i)),
        capitalize(&word(base + 50_000 + i * 7))
    )
}

/// Key file, config and the seeded store (an empty corpus gives an empty store).
pub(crate) fn seed_home(
    paths: &Paths,
    corpus: &Corpus,
    dim: usize,
    seed: u64,
) -> Result<(), BenchError> {
    std::fs::create_dir_all(&paths.data)?;
    std::fs::create_dir_all(&paths.config)?;
    let mut key = [0u8; 32];
    let mut rng = Rng::new(seed ^ 0xDAE0_0001);
    for c in key.chunks_mut(8) {
        c.copy_from_slice(&rng.next_u64().to_le_bytes());
    }
    // The daemon reads this file when `NUGGET_KEYCHAIN=off` (and tightens it to 0600).
    std::fs::write(paths.data.join("store.key"), hex(&key))?;

    let n = corpus.docs.len();
    let projects = (n / 100).max(3);
    let people = (n / 50).max(5);
    let embedder = HashEmbedder::new(dim, &[]);
    let store = Store::open(paths.db_file(), &StoreKey::from_bytes(key))?;
    for doc in &corpus.docs {
        let text = doc_text(doc, 0);
        let r = item_ref(doc.id);
        let project = format!("Project {}", capitalize(&word(300_000 + doc.id % projects)));
        let a = name(400_000, doc.id % people);
        let b = name(400_000, (doc.id * 7 + 3) % people);
        let item = NewItem::new(r.source.as_str(), r.key.as_str(), content_hash(&text))
            .with_title(doc.title.clone())
            .with_version("v0")
            .with_mime("text/markdown")
            .with_metadata(json!({
                "extracted": {"frontmatter": {"project": project, "people": [a, b]}},
                "tags": [format!("topic{}", doc.topic)],
            }));
        let chunk = NewChunk::embedded(text.clone(), embedder.embed_text(&text));
        let outcome = store.upsert_item_with_chunks(&item, &[chunk])?;
        if outcome != UpsertOutcome::Inserted {
            return Err(BenchError::Failed(format!(
                "seeding {} returned {outcome:?}",
                r.key
            )));
        }
    }
    debug_assert_eq!(SYNTHETIC_SOURCE, item_ref(0).source.as_str());
    Ok(())
}

/// Write `config.toml`: hermetic (no providers, runtime or MCP listener), the vault at `vault`
/// or off.
pub(crate) fn write_config(paths: &Paths, vault: Option<&Path>) -> Result<(), BenchError> {
    let vault = match vault {
        // A TOML literal string: no escapes, so Windows paths are taken as written.
        Some(v) => format!("enabled = true\npath = '{}'", v.display()),
        None => "enabled = false".to_owned(),
    };
    std::fs::write(
        paths.config_file(),
        format!("[runtime]\nenabled = false\n\n[mcp]\nenabled = false\n\n[vault]\n{vault}\n"),
    )?;
    Ok(())
}

/// A running daemon. Killed on drop if [`stop`](Self::stop) was not reached.
pub(crate) struct Daemon {
    child: Option<Child>,
    pub(crate) pid: u32,
    stderr: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Daemon {
    pub(crate) fn spawn(env: &Env, run: &str) -> Result<Self, BenchError> {
        let stderr = env.work.join(format!("nuggetd-{run}.stderr"));
        let child = Command::new(&env.exe)
            .env("NUGGET_HOME", &env.home)
            .env("NUGGET_KEYCHAIN", "off")
            .env_remove("NUGGET_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr)?)
            .spawn()
            .map_err(|e| BenchError::Failed(format!("cannot start {}: {e}", env.exe.display())))?;
        let pid = child.id();
        Ok(Self {
            child: Some(child),
            pid,
            stderr,
        })
    }

    /// Fails if the process has exited, with the tail of its stderr.
    pub(crate) fn check_alive(&mut self) -> Result<(), BenchError> {
        let Some(child) = self.child.as_mut() else {
            return Err(BenchError::Failed("daemon already stopped".into()));
        };
        if let Some(status) = child.try_wait()? {
            let err = std::fs::read_to_string(&self.stderr).unwrap_or_default();
            let tail: String = err
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(BenchError::Failed(format!(
                "nuggetd exited early with {status}: {tail}"
            )));
        }
        Ok(())
    }

    /// Spawn-to-first-`status` time and a connected client.
    pub(crate) async fn ready(
        &mut self,
        env: &Env,
        t0: Instant,
    ) -> Result<(Duration, Client), BenchError> {
        loop {
            self.check_alive()?;
            if let Ok(client) = Client::connect(&env.paths).await {
                let left = env.timeout.saturating_sub(t0.elapsed());
                match tokio::time::timeout(
                    left,
                    client.call::<_, StatusResponse>(method::STATUS, &()),
                )
                .await
                {
                    Ok(Ok(_)) => return Ok((t0.elapsed(), client)),
                    Ok(Err(_)) => {}
                    Err(_) => {
                        return Err(BenchError::Failed(
                            "nuggetd accepted a connection but never answered `status`".into(),
                        ));
                    }
                }
            }
            if t0.elapsed() >= env.timeout {
                return Err(BenchError::Failed(format!(
                    "nuggetd did not answer within {:?}",
                    env.timeout
                )));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Graceful stop (SIGTERM on Unix), bounded; returns how long it took.
    pub(crate) async fn stop(&mut self) -> Result<Duration, BenchError> {
        let Some(mut child) = self.child.take() else {
            return Ok(Duration::ZERO);
        };
        let t = Instant::now();
        #[cfg(unix)]
        let graceful = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status()
            .is_ok_and(|s| s.success());
        #[cfg(not(unix))]
        let graceful = false;
        if graceful {
            while t.elapsed() < Duration::from_secs(60) {
                if child.try_wait()?.is_some() {
                    return Ok(t.elapsed());
                }
                tokio::time::sleep(POLL).await;
            }
        }
        child.kill()?;
        child.wait()?;
        if graceful {
            return Err(BenchError::Failed(
                "nuggetd did not exit within 60 s of SIGTERM".into(),
            ));
        }
        Ok(t.elapsed())
    }
}

pub(crate) async fn call<R: DeserializeOwned>(
    client: &Client,
    m: &str,
    params: Value,
    timeout: Duration,
) -> Result<R, BenchError> {
    match tokio::time::timeout(timeout, client.call(m, &params)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(BenchError::Failed(format!("{m}: {e}"))),
        Err(_) => Err(BenchError::Failed(format!(
            "{m} timed out after {timeout:?}"
        ))),
    }
}

pub(crate) async fn search(
    client: &Client,
    q: &str,
    timeout: Duration,
) -> Result<Vec<SearchHit>, BenchError> {
    call(
        client,
        method::SEARCH,
        json!({"query": q, "limit": 10}),
        timeout,
    )
    .await
}

/// Poll `f` until it returns `Some`, failing after `timeout` with `what`.
async fn wait_for<T, F, Fut>(
    daemon: &mut Daemon,
    timeout: Duration,
    what: &str,
    f: F,
) -> Result<T, BenchError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, BenchError>>,
{
    wait_every(daemon, timeout, POLL, what, f).await
}

/// [`wait_for`] polling every `poll` (the `energy` suite polls slowly, so its own IPC traffic
/// stays a small part of what it meters).
pub(crate) async fn wait_every<T, F, Fut>(
    daemon: &mut Daemon,
    timeout: Duration,
    poll: Duration,
    what: &str,
    mut f: F,
) -> Result<T, BenchError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, BenchError>>,
{
    let t = Instant::now();
    loop {
        daemon.check_alive()?;
        if let Some(v) = f().await? {
            return Ok(v);
        }
        if t.elapsed() >= timeout {
            return Err(BenchError::Failed(format!(
                "{what} not reached within {timeout:?}"
            )));
        }
        tokio::time::sleep(poll).await;
    }
}

const MIB: f64 = 1024.0 * 1024.0;

fn rss_metric(out: &mut Vec<Metric>, s: &ProcSample, when: &str, what: &str) {
    if let Some(rss) = s.rss {
        out.push(Metric::new(
            format!("daemon.rss_mib.{when}"),
            rss as f64 / MIB,
            "MiB",
            Direction::LowerIsBetter,
            format!("Resident set size of nuggetd {what}"),
        ));
    }
}

fn peak_metric(out: &mut Vec<Metric>, s: &ProcSample, name: &str, what: &str) {
    if let Some(hwm) = s.hwm {
        out.push(Metric::new(
            format!("daemon.rss_mib.{name}"),
            hwm as f64 / MIB,
            "MiB",
            Direction::LowerIsBetter,
            format!("Peak resident set size (VmHWM) of nuggetd {what}"),
        ));
    }
}

async fn profile(
    env: &Env,
    p: &DaemonParams,
    corpus: &Corpus,
    seed: u64,
    out: &mut Vec<Metric>,
) -> Result<(), BenchError> {
    let call_timeout = Duration::from_secs(60).min(env.timeout);
    let queries = corpus.queries(p.searches + 1, 4, 0.25, seed);

    // ---- First start: vault off. Startup, first search, extraction, warm searches. ----
    write_config(&env.paths, None)?;
    let t0 = Instant::now();
    let mut d = Daemon::spawn(env, "first")?;
    let (startup, client) = d.ready(env, t0).await?;
    let ready = Instant::now();
    out.push(Metric::new(
        "daemon.startup_ms",
        startup.as_secs_f64() * 1e3,
        "ms",
        Direction::LowerIsBetter,
        "Process spawn to the first answered `status` call, first start on the seeded store",
    ));
    rss_metric(out, &sample(d.pid), "after_startup", "right after startup");
    let status: StatusResponse = call(&client, method::STATUS, json!({}), call_timeout).await?;
    if status.items != p.items as u64 {
        return Err(BenchError::Failed(format!(
            "nuggetd reports {} items, {} were seeded",
            status.items, p.items
        )));
    }

    let t = Instant::now();
    let first = search(&client, &queries[0].text, call_timeout).await?;
    out.push(Metric::new(
        "daemon.first_search_ms",
        t.elapsed().as_secs_f64() * 1e3,
        "ms",
        Direction::LowerIsBetter,
        "The first `search` IPC call after startup (round trip, top 10; the startup knowledge \
         run is usually in progress)",
    ));
    rss_metric(
        out,
        &sample(d.pid),
        "after_first_search",
        "after the first search",
    );

    let c = &client;
    let k: KnowledgeStatus = wait_for(&mut d, env.timeout, "knowledge extraction", || async move {
        let k: KnowledgeStatus = call(c, method::KNOWLEDGE_STATUS, json!({}), call_timeout).await?;
        Ok((k.last_run.is_some() && k.pending_items == 0).then_some(k))
    })
    .await?;
    let extract = ready.elapsed().as_secs_f64().max(1e-9);
    out.extend([
        Metric::new(
            "daemon.knowledge.extract_s",
            extract,
            "s",
            Direction::LowerIsBetter,
            "Startup knowledge extraction over the whole seeded store (deterministic extractors; \
             no LLM provider configured): from the first `status` answer until \
             `knowledge.status` shows a finished run with nothing pending",
        ),
        Metric::new(
            "daemon.knowledge.items_per_s",
            p.items as f64 / extract,
            "items/s",
            Direction::HigherIsBetter,
            "Seeded items per second of startup knowledge extraction",
        ),
        Metric::new(
            "daemon.knowledge.entities",
            k.entities as f64,
            "count",
            Direction::Info,
            "Entities in the graph after extraction",
        ),
        Metric::new(
            "daemon.knowledge.claims",
            k.claims as f64,
            "count",
            Direction::Info,
            "Claims in the graph after extraction",
        ),
    ]);
    if k.entities == 0 {
        return Err(BenchError::Failed(
            "knowledge extraction produced no entities".into(),
        ));
    }
    rss_metric(
        out,
        &sample(d.pid),
        "after_extract",
        "after the knowledge extraction",
    );

    let mut lat = Vec::with_capacity(p.searches);
    let mut found = 0usize;
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
    let recall = (found
        + usize::from(
            first
                .iter()
                .any(|h| h.item == item_ref(queries[0].relevant)),
        )) as f64
        / queries.len() as f64;
    out.push(Metric::new(
        "daemon.search.recall@10",
        recall,
        "ratio",
        Direction::Info,
        "Known-item queries whose target is in the daemon's top 10 (sanity check of the seeded \
         store; text-only)",
    ));
    if recall == 0.0 {
        return Err(BenchError::Failed(
            "no search through the daemon found its target".into(),
        ));
    }
    peak_metric(
        out,
        &sample(d.pid),
        "peak_first_start",
        "over the first start (startup, extraction and searches)",
    );
    drop(client);
    let stop = d.stop().await?;
    out.push(Metric::new(
        "daemon.shutdown_ms",
        stop.as_secs_f64() * 1e3,
        "ms",
        Direction::LowerIsBetter,
        "Graceful shutdown (SIGTERM to exit) after the first start; on Windows a kill",
    ));

    // ---- Second start: vault on. Startup, full vault render, idle, ingest. ----
    let vault = env.work.join("vault");
    write_config(&env.paths, Some(&vault))?;
    let t0 = Instant::now();
    let mut d = Daemon::spawn(env, "second")?;
    let (startup, client) = d.ready(env, t0).await?;
    let ready = Instant::now();
    out.push(Metric::new(
        "daemon.restart.startup_ms",
        startup.as_secs_f64() * 1e3,
        "ms",
        Direction::LowerIsBetter,
        "Process spawn to the first answered `status` call, second start (graph already built)",
    ));
    let c = &client;
    let k: KnowledgeStatus = wait_for(&mut d, env.timeout, "the vault render", || async move {
        let k: KnowledgeStatus = call(c, method::KNOWLEDGE_STATUS, json!({}), call_timeout).await?;
        Ok(
            (k.vault.last_render.is_some() && k.vault.pages > 0 && k.pending_items == 0)
                .then_some(k),
        )
    })
    .await?;
    let render = ready.elapsed().as_secs_f64();
    out.extend([
        Metric::new(
            "daemon.vault.render_s",
            render,
            "s",
            Direction::LowerIsBetter,
            "First full vault render of every entity page: from the first `status` answer of a \
             start with the vault enabled (never rendered before) until `knowledge.status` \
             reports a render",
        ),
        Metric::new(
            "daemon.vault.pages",
            k.vault.pages as f64,
            "count",
            Direction::Info,
            "Vault pages after the render",
        ),
    ]);

    // Idle: no IPC traffic from here until the window ends.
    tokio::time::sleep(Duration::from_secs_f64(p.settle_s)).await;
    d.check_alive()?;
    let s0 = sample(d.pid);
    let w0 = Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(p.idle_s)).await;
    let s1 = sample(d.pid);
    let wall = w0.elapsed().as_secs_f64();
    if let (Some(a), Some(b)) = (s0.cpu_s, s1.cpu_s) {
        out.push(Metric::new(
            "daemon.idle.cpu_percent",
            ((b - a).max(0.0) / wall * 100.0).max(0.0),
            "%",
            Direction::LowerIsBetter,
            "CPU time used by nuggetd over the idle window, as a share of one core (nothing \
             changes, no clients calling, after extraction and the vault render settled)",
        ));
    }
    if let (Some(a), Some(b)) = (s0.switches, s1.switches) {
        out.push(Metric::new(
            "daemon.idle.wakeups_per_s",
            b.saturating_sub(a) as f64 / wall,
            "1/s",
            Direction::LowerIsBetter,
            "Context switches (voluntary + involuntary, all threads) per second over the idle \
             window: how often the idle daemon wakes up",
        ));
    }
    if let Some(n) = s1.threads {
        out.push(Metric::new(
            "daemon.idle.threads",
            n as f64,
            "count",
            Direction::Info,
            "Threads of the idle daemon",
        ));
    }
    rss_metric(out, &s1, "after_idle", "after the idle window");

    // Ingest through the daemon: a watched folder, then files dropped into it.
    let folder = env.work.join("inbox");
    let src_id = &watch_folder(&mut d, &client, &folder, env.timeout, POLL).await?;
    let texts = ingest_texts(p.ingest_files, p.words_per_doc, seed);
    let probe = &marker(p.ingest_files - 1, INGEST_VERSION);
    let t = Instant::now();
    for (i, text) in texts.iter().enumerate() {
        std::fs::write(folder.join(format!("note-{i:05}.md")), text)?;
    }
    let want = p.ingest_files as u64;
    wait_for(
        &mut d,
        env.timeout,
        "ingest of the dropped files",
        || async move {
            let st: IngestStatus = call(c, method::INGEST_STATUS, json!({}), call_timeout).await?;
            let indexed = st
                .sources
                .iter()
                .any(|s| s.id == *src_id && s.items >= want);
            if !(indexed && st.queue_depth == 0) {
                return Ok(None);
            }
            let hits = search(c, probe, call_timeout).await?;
            Ok(hits.iter().any(|h| h.item.source == *src_id).then_some(()))
        },
    )
    .await?;
    let ingest = t.elapsed().as_secs_f64().max(1e-9);
    out.extend([
        Metric::new(
            "daemon.ingest.files_per_s",
            p.ingest_files as f64 / ingest,
            "files/s",
            Direction::HigherIsBetter,
            "Markdown files dropped into a watched folder, per second, from the first write until \
             all are indexed (ingest.status) and the last one is found by `search` (text-only: \
             no embedding provider)",
        ),
        Metric::new(
            "daemon.ingest.wall_s",
            ingest,
            "s",
            Direction::LowerIsBetter,
            "Wall time from the first dropped file until all are indexed and searchable",
        ),
    ]);
    let end = sample(d.pid);
    rss_metric(
        out,
        &end,
        "after_ingest",
        "after ingesting the dropped files",
    );
    peak_metric(
        out,
        &end,
        "peak",
        "over the second start (vault render, idle and ingest)",
    );
    drop(client);
    d.stop().await?;
    Ok(())
}

/// Add a files source on `folder` (created if missing) and wait until it is watched. Returns
/// the source id.
pub(crate) async fn watch_folder(
    d: &mut Daemon,
    client: &Client,
    folder: &Path,
    timeout: Duration,
    poll: Duration,
) -> Result<SourceId, BenchError> {
    let call_timeout = Duration::from_secs(60).min(timeout);
    std::fs::create_dir_all(folder)?;
    let src: SourceInfo = call(
        client,
        method::SOURCES_ADD,
        json!({"kind": "files", "name": "bench-inbox", "settings": {"roots": [folder], "exclude": []}}),
        call_timeout,
    )
    .await?;
    let src_id = &src.id;
    wait_every(d, timeout, poll, "the watched folder", || async move {
        let st: IngestStatus = call(client, method::INGEST_STATUS, json!({}), call_timeout).await?;
        Ok(st
            .sources
            .iter()
            .any(|s| s.id == *src_id && s.state == SourceState::Watching)
            .then_some(()))
    })
    .await?;
    Ok(src.id)
}

/// `n` Markdown files to drop into a watched folder, each ending in its unique
/// [`INGEST_VERSION`] marker (`marker(i, INGEST_VERSION)` finds file `i`).
pub(crate) fn ingest_texts(n: usize, words_per_doc: usize, seed: u64) -> Vec<String> {
    let files = Corpus::generate(CorpusConfig {
        docs: n,
        words_per_doc,
        seed: seed ^ 0x1A6E_5700,
        ..CorpusConfig::default()
    });
    files
        .docs
        .iter()
        .map(|doc| {
            format!(
                "# {}\n\n{}\n\n{}\n",
                doc.title,
                doc.body,
                marker(doc.id, INGEST_VERSION)
            )
        })
        .collect()
}

/// What the OS cheaply reports about a process. `None` where unavailable.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct ProcSample {
    rss: Option<u64>,
    hwm: Option<u64>,
    /// User + system CPU time of all threads, in seconds.
    cpu_s: Option<f64>,
    /// Voluntary + involuntary context switches of all live threads.
    switches: Option<u64>,
    threads: Option<u64>,
}

#[cfg(target_os = "linux")]
fn sample(pid: u32) -> ProcSample {
    let base = PathBuf::from(format!("/proc/{pid}"));
    let field = |status: &str, key: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    let mut s = ProcSample::default();
    if let Ok(status) = std::fs::read_to_string(base.join("status")) {
        s.rss = field(&status, "VmRSS:").map(|kb| kb * 1024);
        s.hwm = field(&status, "VmHWM:").map(|kb| kb * 1024);
        s.threads = field(&status, "Threads:");
    }
    let Ok(tasks) = std::fs::read_dir(base.join("task")) else {
        return s;
    };
    let (mut cpu_ns, mut switches, mut any) = (0u64, 0u64, false);
    for task in tasks.flatten() {
        let dir = task.path();
        // `schedstat`: nanoseconds on CPU, first field. Threads can exit between list and read.
        let Some(ns) = std::fs::read_to_string(dir.join("schedstat"))
            .ok()
            .and_then(|t| t.split_whitespace().next()?.parse::<u64>().ok())
        else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(dir.join("status")) else {
            continue;
        };
        cpu_ns += ns;
        switches += field(&status, "voluntary_ctxt_switches:").unwrap_or(0)
            + field(&status, "nonvoluntary_ctxt_switches:").unwrap_or(0);
        any = true;
    }
    if any {
        s.cpu_s = Some(cpu_ns as f64 / 1e9);
        s.switches = Some(switches);
    }
    s
}

#[cfg(target_os = "macos")]
fn sample(pid: u32) -> ProcSample {
    let Ok(o) = Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
    else {
        return ProcSample::default();
    };
    let text = String::from_utf8_lossy(&o.stdout);
    let mut parts = text.split_whitespace();
    let rss = parts
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024);
    // `[[dd-]hh:]mm:ss.cc`
    let cpu_s = parts.next().and_then(|t| {
        let t = t.rsplit_once('-').map_or(t, |(_, rest)| rest);
        t.split(':')
            .try_fold(0.0, |acc, p| p.parse::<f64>().ok().map(|v| acc * 60.0 + v))
    });
    ProcSample {
        rss,
        cpu_s,
        ..ProcSample::default()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sample(_pid: u32) -> ProcSample {
    ProcSample::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Backend, RunConfig, run_benchmark};
    use crate::{Report, Status};

    #[test]
    fn names_and_hex() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_ne!(name(400_000, 1), name(400_000, 2));
        assert!(name(400_000, 0).chars().next().unwrap().is_uppercase());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn samples_this_process() {
        let s = sample(std::process::id());
        assert!(s.rss.unwrap() > 0 && s.hwm.unwrap() >= s.rss.unwrap());
        assert!(s.cpu_s.unwrap() > 0.0);
        assert!(s.threads.unwrap() >= 1);
    }

    /// The daemon profile at a tiny size against the `nuggetd` built next to this test binary
    /// (CI's workspace build has one); skipped when there is none.
    #[test]
    fn daemon_profile_smoke() {
        if let Err(why) = find_nuggetd() {
            eprintln!("skipping: {why}");
            return;
        }
        let report: Report = run_benchmark(
            &DaemonOnly(DaemonParams {
                items: 200,
                searches: 10,
                settle_s: 0.5,
                idle_s: 1.0,
                ingest_files: 20,
                ..DaemonParams::for_scale(Scale::Smoke)
            }),
            &RunConfig {
                scale: Scale::Smoke,
                seed: 7,
                backend: Backend::Real,
                ..RunConfig::default()
            },
        );
        // A `nuggetd` left over from an older build of a changed store cannot open the seeded
        // one; that says nothing about the profile (a workspace build has a matching binary).
        if let Some(msg) = &report.message
            && msg.contains("newer than this build supports")
        {
            eprintln!("skipping: stale nuggetd binary: {msg}");
            return;
        }
        assert_eq!(report.status, Status::Ok, "{:?}", report.message);
        let v = |n: &str| {
            report
                .metric(n)
                .unwrap_or_else(|| panic!("missing {n}"))
                .value
        };
        for name in [
            "daemon.startup_ms",
            "daemon.restart.startup_ms",
            "daemon.first_search_ms",
            "daemon.search.latency.p50_us",
            "daemon.search.latency.p95_us",
            "daemon.knowledge.extract_s",
            "daemon.knowledge.entities",
            "daemon.vault.render_s",
            "daemon.vault.pages",
            "daemon.ingest.files_per_s",
        ] {
            assert!(v(name) > 0.0, "{name} = {}", v(name));
        }
        assert!(v("daemon.search.recall@10") > 0.5);
        if cfg!(target_os = "linux") {
            assert!(v("daemon.idle.cpu_percent") >= 0.0);
            assert!(v("daemon.idle.wakeups_per_s") >= 0.0);
            assert!(v("daemon.rss_mib.peak") > 0.0);
        }
        assert_eq!(report.config.params["daemon"], "measured");
    }

    struct DaemonOnly(DaemonParams);

    impl crate::runner::Benchmark for DaemonOnly {
        fn name(&self) -> &'static str {
            "perf"
        }
        fn description(&self) -> &'static str {
            "daemon profile only"
        }
        fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            let exe = find_nuggetd().map_err(BenchError::Failed)?;
            run_with(ctx, &self.0, &exe)
        }
    }
}
