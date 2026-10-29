//! `memory --backend real`: the memory benchmark against the real stack, in process.
//!
//! Everything a user's data goes through, minus the daemon and IPC:
//! an encrypted [`nugget_store::Store`] in a temporary `NUGGET_HOME`, [`nugget_ingest::Ingest`]
//! with the `files` connector (registered through [`nugget_connector_files::register`]) watching
//! a temporary folder, and [`nugget_retrieve::Retriever`] for every query. Embeddings come from
//! an in-process deterministic embedder ([`HashEmbedder`], on-device locality) routed through a
//! [`Router`] as the `Embedding` tier, or from an OpenAI-compatible server given with
//! `--embed-url`.
//!
//! Phases:
//! 1. **Eval set.** `nugget-retrieve`'s labelled fixtures are written as Markdown files, ingested
//!    through the pipeline and queried text-only, hybrid and hybrid+MMR (`retrieval.eval.*`).
//!    The same set indexed directly into an in-memory store (`eval::index`, one chunk per
//!    paragraph) is reported alongside (`baseline.eval_direct.*`).
//! 2. **Initial ingest.** The synthetic corpus of the fake backend (same scale and seed, so the
//!    same documents) is written as files, then the source is added and timed until every file
//!    is indexed (`ingest.*`).
//! 3. **Known-item retrieval** on that corpus (`retrieval.{bm25,vector,hybrid,hybrid_mmr}.*`),
//!    with the fake index's numbers on the identical corpus and queries as the baseline
//!    (`baseline.fake.*`): the M1 gate is hybrid recall@10 at or above it.
//! 4. **Freshness.** Edits, creates, deletes and renames hit the watched folder at random
//!    (exponential gaps). For each, the clock starts when the file operation returns and stops
//!    when `Retriever::search` reflects it (new content found, old content and old keys gone),
//!    probed every few milliseconds (`freshness.*`, `propagation.*`). The M1 gate is p95 ≤ 60 s.
//! 5. **Resources**: database size on disk and (Linux) the process's peak RSS.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_connector::Registry;
use nugget_connector_files::{FILES_KIND, item_key};
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::egress::NullEgress;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{SourceSpec, SourceState};
use nugget_core::paths::Paths;
use nugget_infer::{Provider, ProviderConfig, Router, build_provider};
use nugget_ingest::{Ingest, IngestConfig, MemorySecrets, NoPowerPolicy};
use nugget_retrieve::eval::{
    self, EVAL_DEPTH, EvalMode, EvalSet, HashEmbedder, mrr_at, ndcg_at, recall_at,
};
use nugget_retrieve::{RetrieveConfig, Retriever, SearchOptions, Strategy};
use nugget_store::{Store, StoreKey};
use serde_json::json;

use crate::corpus::{Corpus, CorpusConfig, Doc, Query, Rng, marker};
use crate::index::FakeIndex;
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunConfig, RunContext, Scale};
use crate::stats::summarize;
use crate::suites::memory::{
    FRESHNESS_TARGET_S, MemoryParams, RANK_DUMP_DEEP, RankDump, rank_dump_path,
    retrieval as fake_retrieval,
};
use crate::suites::{doc_text, item_ref};

/// Results considered per known-item query (as in the fake backend).
const DEPTH: usize = 10;

/// Workload of the real backend. Corpus and query parameters equal the fake backend's for the
/// same scale, so its baseline is computed on identical documents and queries.
#[derive(Debug, Clone, PartialEq)]
pub struct RealParams {
    pub docs: usize,
    pub queries: usize,
    pub query_terms: usize,
    pub query_noise: f64,
    /// Dimension of the in-process embedder (ignored with `--embed-url`).
    pub embedding_dim: usize,
    /// File operations in the freshness phase.
    pub changes: usize,
    /// Mean gap between file operations (exponentially distributed).
    pub change_gap_mean_s: f64,
    /// A change not visible after this long counts as failed (and at this lag).
    pub change_timeout_s: f64,
    /// How often pending changes are checked through `Retriever::search`.
    pub probe_interval_ms: u64,
    /// Upper bound on the initial ingest.
    pub ingest_timeout_s: f64,
    /// Synthetic files per sub-folder.
    pub files_per_dir: usize,
    /// Run the eval-set phase.
    pub eval_set: bool,
}

impl RealParams {
    pub fn for_scale(scale: Scale) -> Self {
        let m = MemoryParams::for_scale(scale);
        Self {
            docs: m.docs,
            queries: m.queries,
            query_terms: m.query_terms,
            query_noise: m.query_noise,
            embedding_dim: m.dim,
            changes: scale.pick(16, 100, 300, 1_000),
            change_gap_mean_s: scale.pick(0.25, 0.5, 0.5, 0.5),
            change_timeout_s: 2.0 * FRESHNESS_TARGET_S,
            probe_interval_ms: 20,
            ingest_timeout_s: 3_600.0,
            files_per_dir: 250,
            eval_set: true,
        }
    }
}

/// Entry point for `memory --backend real`.
pub fn run(ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
    run_with(ctx, &RealParams::for_scale(ctx.scale()))
}

pub fn run_with(ctx: &mut RunContext, p: &RealParams) -> Result<Vec<Metric>, BenchError> {
    if p.docs == 0 || p.queries == 0 || p.query_terms == 0 {
        return Err(BenchError::Failed(
            "memory needs documents, queries and query terms".into(),
        ));
    }
    let config = ctx.config.clone();
    let set = EvalSet::builtin()?;
    // Built outside the runtime: resolving a remote host is a blocking lookup.
    let embedder = Embedder::build(&config, p.embedding_dim, &set.synonyms)?;

    ctx.param("backend", "real");
    ctx.param(
        "stack",
        "nugget-store (SQLCipher, temp dir) + nugget-ingest + files connector (native watcher) + nugget-retrieve",
    );
    ctx.param("embedder", embedder.description.clone());
    ctx.param("docs", p.docs);
    ctx.param("queries", p.queries);
    ctx.param("query_terms", p.query_terms);
    ctx.param("query_noise", p.query_noise);
    ctx.param("changes", p.changes);
    ctx.param("change_gap_mean_s", p.change_gap_mean_s);
    ctx.param("change_timeout_s", p.change_timeout_s);
    ctx.param("probe_interval_ms", p.probe_interval_ms);
    ctx.param("files_per_dir", p.files_per_dir);
    ctx.param(
        "watcher_debounce_ms",
        nugget_connector_files::DEFAULT_DEBOUNCE_MS,
    );
    ctx.param("retrieve_config", "RetrieveConfig::default()");
    if p.eval_set {
        ctx.param("eval_set", set.name.clone());
        ctx.param("eval_docs", set.docs.len());
        ctx.param("eval_queries", set.queries.len());
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut out = Vec::new();
    rt.block_on(async {
        if p.eval_set {
            out.extend(eval_phase(&embedder, &set, config.seed).await?);
        }
        out.extend(synthetic_phase(&embedder, p, &config).await?);
        Ok::<_, BenchError>(())
    })?;
    rt.shutdown_timeout(Duration::from_secs(10));

    if let Some(rss) = peak_rss_bytes() {
        out.push(Metric::new(
            "resources.peak_rss_mib",
            rss as f64 / (1024.0 * 1024.0),
            "MiB",
            Direction::LowerIsBetter,
            "Peak resident set size of the whole benchmark process (store, ingest, watcher, \
             retriever, embedder and the harness itself), from /proc/self/status VmHWM",
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------- embedder and stack

struct Embedder {
    provider: Arc<dyn Provider>,
    description: String,
}

impl Embedder {
    fn build(config: &RunConfig, dim: usize, synonyms: &[Vec<String>]) -> Result<Self, BenchError> {
        match &config.embed_url {
            None => {
                let e = HashEmbedder::new(dim, synonyms);
                Ok(Self {
                    description: format!(
                        "hash-bow (nugget_retrieve::eval::HashEmbedder, in-process, deterministic, \
                         on-device), dim {dim}"
                    ),
                    provider: Arc::new(e),
                })
            }
            Some(url) => {
                let model = config
                    .embed_model
                    .clone()
                    .unwrap_or_else(|| "default".into());
                let cfg: ProviderConfig = serde_json::from_value(json!({
                    "id": "bench-embed",
                    "kind": "openai_compatible",
                    "base_url": url,
                    "model": model,
                    "tiers": ["embedding"],
                }))
                .map_err(|e| BenchError::Failed(format!("embed provider config: {e}")))?;
                let provider = build_provider(&cfg, Arc::new(NullEgress))?;
                let locality = provider.meta().locality;
                Ok(Self {
                    description: format!(
                        "openai_compatible {url} model {model} (locality {locality:?})"
                    ),
                    provider,
                })
            }
        }
    }

    /// A router with this provider as the (probed) `Embedding` tier.
    fn router(&self) -> Arc<Router> {
        let id = self.provider.id().clone();
        let router = Router::new(
            vec![self.provider.clone()],
            HashMap::from([(Tier::Embedding, vec![id.clone()])]),
        );
        router.set_capabilities(
            &id,
            Capabilities {
                embeddings: true,
                ..Default::default()
            },
        );
        Arc::new(router)
    }
}

/// One isolated stack: temp home, encrypted store, ingest with one `files` source, retriever.
struct Stack {
    root: PathBuf,
    db_path: PathBuf,
    store: Store,
    ingest: Ingest,
    retriever: Retriever,
    source: SourceId,
    // Last: removed after everything above is dropped.
    _home: tempfile::TempDir,
}

impl Stack {
    async fn start(source: &str, router: Arc<Router>, seed: u64) -> Result<Self, BenchError> {
        let home = tempfile::Builder::new().prefix("nugget-bench-").tempdir()?;
        let root = home.path().join("watched");
        std::fs::create_dir_all(&root)?;
        // Watchers report canonical paths (e.g. /private/var on macOS).
        let root = root.canonicalize()?;
        let paths = Paths::rooted(home.path().join("home"));
        let db_path = paths.db_file();
        let mut rng = Rng::new(seed ^ 0x6B65_7900);
        let mut key = [0u8; 32];
        for chunk in key.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        let store = Store::open(&db_path, &StoreKey::from_bytes(key))?;
        if !store.is_encrypted() {
            return Err(BenchError::Failed("store is not encrypted".into()));
        }
        let mut registry = Registry::new();
        nugget_connector_files::register(&mut registry);
        let cfg = IngestConfig {
            // No triage provider is configured; say so explicitly.
            triage: false,
            ..IngestConfig::default()
        };
        let ingest = Ingest::builder(cfg, store.clone(), registry, paths)
            .router(Some(router.clone()))
            .secrets(Arc::new(MemorySecrets::new()))
            .power(Arc::new(NoPowerPolicy))
            .start()
            .await?;
        let retriever = Retriever::new(store.clone(), Some(router), RetrieveConfig::default());
        Ok(Self {
            root,
            db_path,
            store,
            ingest,
            retriever,
            source: SourceId::new(source),
            _home: home,
        })
    }

    async fn add_source(&self) -> Result<(), BenchError> {
        self.ingest
            .add(SourceSpec {
                id: Some(self.source.clone()),
                kind: FILES_KIND.into(),
                name: format!("bench {}", self.source),
                settings: json!({ "roots": [self.root] }),
            })
            .await?;
        Ok(())
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, text: &str) -> Result<(), BenchError> {
        let path = self.path(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    fn key(rel: &str) -> ItemKey {
        ItemKey::new(item_key(0, Path::new(rel)))
    }

    fn iref(&self, key: &ItemKey) -> ItemRef {
        ItemRef {
            source: self.source.clone(),
            key: key.clone(),
        }
    }

    /// Wait until exactly `expected` items of the source carry their version (written last by
    /// the pipeline, so the item and its chunks are complete). Returns the elapsed time.
    async fn wait_indexed(&self, expected: usize, timeout: Duration) -> Result<(), BenchError> {
        let start = Instant::now();
        loop {
            let versions = self.store.item_versions(&self.source)?;
            if versions.len() == expected && versions.values().all(Option::is_some) {
                return Ok(());
            }
            let info = self.ingest.get(&self.source).await?;
            if info.state == SourceState::Error {
                return Err(BenchError::Failed(format!(
                    "source {} failed: {}",
                    self.source,
                    info.last_error.unwrap_or_default()
                )));
            }
            if start.elapsed() > timeout {
                return Err(BenchError::Failed(format!(
                    "initial ingest timed out after {timeout:?}: {} of {expected} items indexed",
                    versions.values().filter(|v| v.is_some()).count()
                )));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_watching(&self, timeout: Duration) -> Result<(), BenchError> {
        let start = Instant::now();
        loop {
            let info = self.ingest.get(&self.source).await?;
            if info.state == SourceState::Watching {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(BenchError::Failed(format!(
                    "source {} is {:?}, not watching, after {timeout:?} ({})",
                    self.source,
                    info.state,
                    info.last_error.unwrap_or_default()
                )));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// `Retriever::search` (the default, hybrid path) returns `key` for `query`.
    async fn visible(&self, query: &str, key: &ItemKey) -> Result<bool, BenchError> {
        let res = self.retriever.search(query, DEPTH).await?;
        Ok(res
            .items
            .iter()
            .any(|i| i.item.source == self.source && i.item.key == *key))
    }

    /// Full-text search (exact; the vector leg would return arbitrary neighbours for a marker)
    /// returns `key` for `query`.
    async fn text_hit(&self, query: &str, key: &ItemKey) -> Result<bool, BenchError> {
        let res = self
            .retriever
            .search_with(query, DEPTH, opts(Strategy::TextOnly, false))
            .await?;
        Ok(res
            .items
            .iter()
            .any(|i| i.item.source == self.source && i.item.key == *key))
    }

    fn exists(&self, key: &ItemKey) -> Result<bool, BenchError> {
        Ok(self.store.get_item(&self.iref(key))?.is_some())
    }

    async fn shutdown(self) {
        self.ingest.shutdown().await;
    }
}

fn opts(strategy: Strategy, mmr: bool) -> SearchOptions {
    SearchOptions {
        strategy,
        mmr: Some(mmr),
    }
}

/// The retrieval modes measured, with their search options.
const MODES: [(&str, Strategy, bool); 4] = [
    ("bm25", Strategy::TextOnly, false),
    ("vector", Strategy::VectorOnly, false),
    ("hybrid", Strategy::Auto, false),
    ("hybrid_mmr", Strategy::Auto, true),
];

// ---------------------------------------------------------------- phase 1: eval set

async fn eval_phase(
    embedder: &Embedder,
    set: &EvalSet,
    seed: u64,
) -> Result<Vec<Metric>, BenchError> {
    let router = embedder.router();
    let stack = Stack::start("bench-eval", router.clone(), seed).await?;
    for d in &set.docs {
        stack.write(
            &format!("{}.md", d.id),
            &format!("# {}\n\n{}\n", d.title, d.text),
        )?;
    }
    stack.add_source().await?;
    stack
        .wait_indexed(set.docs.len(), Duration::from_secs(600))
        .await?;

    let doc_id = |key: &ItemKey| -> Option<String> {
        let k = key.as_str().strip_prefix("0:")?.strip_suffix(".md")?;
        Some(k.to_owned())
    };
    let mut out = Vec::new();
    let modes = [
        ("text_only", Strategy::TextOnly, false),
        ("hybrid", Strategy::Auto, false),
        ("hybrid_mmr", Strategy::Auto, true),
    ];
    let mut recall10 = HashMap::new();
    for (mode, strategy, mmr) in modes {
        let (mut r1, mut r5, mut r10, mut mrr, mut ndcg) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for q in &set.queries {
            let res = stack
                .retriever
                .search_with(&q.query, EVAL_DEPTH, opts(strategy, mmr))
                .await?;
            if strategy == Strategy::Auto && res.mode.is_text_only() {
                return Err(BenchError::Failed(format!(
                    "eval {mode}: query {} answered text-only: {:?}",
                    q.id, res.mode
                )));
            }
            let ranked: Vec<String> = res
                .items
                .iter()
                .filter(|i| i.item.source == stack.source)
                .filter_map(|i| doc_id(&i.item.key))
                .collect();
            let relevant: HashSet<&str> = q.relevant.iter().map(String::as_str).collect();
            r1 += recall_at(&ranked, &relevant, 1);
            r5 += recall_at(&ranked, &relevant, 5);
            r10 += recall_at(&ranked, &relevant, 10);
            mrr += mrr_at(&ranked, &relevant, 10);
            ndcg += ndcg_at(&ranked, &relevant, 10);
        }
        let n = set.queries.len().max(1) as f64;
        recall10.insert(mode, r10 / n);
        for (name, v) in [
            ("recall@1", r1),
            ("recall@5", r5),
            ("recall@10", r10),
            ("mrr@10", mrr),
            ("ndcg@10", ndcg),
        ] {
            let mut m = Metric::new(
                format!("retrieval.eval.{mode}.{name}"),
                v / n,
                "ratio",
                Direction::HigherIsBetter,
                format!(
                    "nugget-retrieve eval set written as Markdown files and ingested through the \
                     real pipeline; {mode} {name} (binary relevance)"
                ),
            );
            if mode != "text_only" && name == "recall@10" {
                // The fixture gate: fusion must not lose what full-text search finds.
                m = m.with_target(recall10["text_only"]);
            }
            out.push(m);
        }
    }
    stack.shutdown().await;

    // The same set indexed straight into a store (one chunk per paragraph), as the
    // nugget-retrieve eval test does: isolates what the pipeline (extraction, chunking) changes.
    let direct = Store::open_in_memory()?;
    eval::index(&direct, set, Some(embedder.provider.as_ref())).await?;
    let retriever = Retriever::new(direct.clone(), Some(router), RetrieveConfig::default());
    let report = eval::run(&direct, &retriever, set).await?;
    for (mode, name) in [
        (EvalMode::TextOnly, "text_only"),
        (EvalMode::Hybrid, "hybrid"),
        (EvalMode::HybridMmr, "hybrid_mmr"),
    ] {
        let Some(m) = report.mode(mode).filter(|m| m.ran) else {
            continue;
        };
        for (metric, v) in [
            ("recall@10", m.metrics.recall_at_10),
            ("mrr@10", m.metrics.mrr_at_10),
        ] {
            out.push(Metric::new(
                format!("baseline.eval_direct.{name}.{metric}"),
                v,
                "ratio",
                Direction::Info,
                format!(
                    "Same eval set indexed directly (eval::index, one chunk per paragraph), \
                     {name} {metric}; reference for the pipeline numbers"
                ),
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- phase 2-5: synthetic corpus

fn rel_path(id: usize, per_dir: usize, rename: u32) -> String {
    let dir = id / per_dir.max(1);
    if rename == 0 {
        format!("d{dir:03}/doc-{id}.md")
    } else {
        format!("d{dir:03}/doc-{id}-r{rename}.md")
    }
}

/// A synthetic document as a Markdown file: the title as the heading, then body and marker.
fn file_text(doc: &Doc, version: u32) -> String {
    format!(
        "# {}\n\n{}\n\n{}\n",
        doc.title,
        doc.body,
        marker(doc.id, version)
    )
}

struct LiveDoc {
    rel: String,
    version: u32,
    topic: usize,
    renames: u32,
}

async fn synthetic_phase(
    embedder: &Embedder,
    p: &RealParams,
    config: &RunConfig,
) -> Result<Vec<Metric>, BenchError> {
    let seed = config.seed;
    let mut out = Vec::new();
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.docs,
        seed,
        ..CorpusConfig::default()
    });
    let queries = corpus.queries(p.queries, p.query_terms, p.query_noise, seed);

    // Baseline: the fake index on the identical corpus and queries (the fake backend's numbers
    // for this scale and seed).
    let mp = MemoryParams {
        docs: p.docs,
        queries: p.queries,
        query_terms: p.query_terms,
        query_noise: p.query_noise,
        ..MemoryParams::for_scale(config.scale)
    };
    let mut ix = FakeIndex::new(mp.dim);
    for doc in &corpus.docs {
        ix.upsert(&item_ref(doc.id), &doc_text(doc, 0));
    }
    let baseline: HashMap<String, f64> = fake_retrieval(&corpus, &ix, &mp, seed)
        .into_iter()
        .map(|m| (m.name, m.value))
        .collect();
    // Diagnostic only (`NUGGET_BENCH_DUMP_MISSES`): the fake arms' per-query ranks.
    let dump = rank_dump_path().map(|path| {
        let mut d = RankDump::default();
        d.add_fake_arms(&ix, &queries);
        (path, d)
    });
    drop(ix);

    // Phase 2: initial ingest.
    let stack = Stack::start("bench-corpus", embedder.router(), seed).await?;
    let mut live: HashMap<usize, LiveDoc> = HashMap::with_capacity(corpus.docs.len());
    let mut bytes = 0usize;
    for doc in &corpus.docs {
        let rel = rel_path(doc.id, p.files_per_dir, 0);
        let text = file_text(doc, 0);
        bytes += text.len();
        stack.write(&rel, &text)?;
        live.insert(
            doc.id,
            LiveDoc {
                rel,
                version: 0,
                topic: doc.topic,
                renames: 0,
            },
        );
    }
    let t = Instant::now();
    stack.add_source().await?;
    stack
        .wait_indexed(
            corpus.docs.len(),
            Duration::from_secs_f64(p.ingest_timeout_s),
        )
        .await?;
    let wall = t.elapsed().as_secs_f64().max(1e-9);
    let counts = stack.store.counts()?;
    out.extend([
        Metric::new(
            "ingest.wall_s",
            wall,
            "s",
            Direction::LowerIsBetter,
            "Initial ingest: from adding the files source to every file indexed (scan, extract, \
             chunk, embed, store)",
        ),
        Metric::new(
            "ingest.files_per_s",
            corpus.docs.len() as f64 / wall,
            "files/s",
            Direction::HigherIsBetter,
            "Initial ingest throughput in files",
        ),
        Metric::new(
            "ingest.chunks_per_s",
            counts.chunks as f64 / wall,
            "chunks/s",
            Direction::HigherIsBetter,
            "Initial ingest throughput in chunks (each embedded)",
        ),
        Metric::new(
            "ingest.mib_per_s",
            bytes as f64 / (1024.0 * 1024.0) / wall,
            "MiB/s",
            Direction::HigherIsBetter,
            "Initial ingest throughput in file bytes",
        ),
        Metric::new(
            "ingest.chunks",
            counts.chunks as f64,
            "count",
            Direction::Info,
            "Chunks produced by the initial ingest",
        ),
    ]);
    let db_initial = db_bytes(&stack.db_path);
    out.push(Metric::new(
        "resources.db_bytes_per_1k_items",
        db_initial as f64 * 1000.0 / corpus.docs.len() as f64,
        "bytes",
        Direction::LowerIsBetter,
        "Encrypted database on disk (main file + WAL + SHM) after the initial ingest, per 1k items. \
         Includes fixed overhead (sqlite-vec allocates vectors in blocks of 1024), so small \
         corpora overstate it",
    ));

    // Phase 3: known-item retrieval through the retriever, against the fake baseline.
    let keys: Vec<ItemKey> = corpus
        .docs
        .iter()
        .map(|d| Stack::key(&rel_path(d.id, p.files_per_dir, 0)))
        .collect();
    out.extend(known_item(&stack, &queries, &keys, &baseline, dump).await?);

    // Phase 4: freshness and propagation.
    stack.wait_watching(Duration::from_secs(120)).await?;
    out.extend(freshness(&stack, &corpus, p, seed, &mut live).await?);
    let status = stack.ingest.status().await?;
    if let Some(ms) = status.freshness_p95_ms {
        out.push(Metric::new(
            "freshness.ingest_reported.p95_s",
            ms as f64 / 1000.0,
            "s",
            Direction::Info,
            "Ingest's own p95 from a change reaching the pipeline to committed (excludes the \
             watcher debounce and the search), from Ingest::status",
        ));
    }

    // Phase 5: resources.
    out.push(Metric::new(
        "resources.db_bytes",
        db_bytes(&stack.db_path) as f64,
        "bytes",
        Direction::Info,
        "Encrypted database on disk (main file + WAL + SHM) at the end of the run",
    ));
    stack.shutdown().await;
    Ok(out)
}

async fn known_item(
    stack: &Stack,
    queries: &[Query],
    keys: &[ItemKey],
    baseline: &HashMap<String, f64>,
    mut dump: Option<(PathBuf, RankDump)>,
) -> Result<Vec<Metric>, BenchError> {
    const KS: [usize; 3] = [1, 5, 10];
    let mut out = Vec::new();
    let mut latencies_ms = Vec::with_capacity(queries.len());
    let fake_hybrid = baseline.get("retrieval.hybrid.recall@10").copied();
    let key_ids: HashMap<&ItemKey, usize> = if dump.is_some() {
        keys.iter().enumerate().map(|(i, k)| (k, i)).collect()
    } else {
        HashMap::new()
    };
    for (mode, strategy, mmr) in MODES {
        let mut hits_at = [0usize; KS.len()];
        let mut rr = 0.0;
        let mut fallbacks = 0usize;
        let mut ranks = Vec::new();
        for q in queries {
            let t = Instant::now();
            let res = stack
                .retriever
                .search_with(&q.text, DEPTH, opts(strategy, mmr))
                .await?;
            if mode == "hybrid_mmr" {
                latencies_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            if strategy == Strategy::Auto && res.mode.is_text_only() {
                fallbacks += 1;
            }
            let target = &keys[q.relevant];
            if dump.is_some() {
                ranks.push(doc_ids(stack, &key_ids, &res.items));
            }
            if let Some(rank) = res
                .items
                .iter()
                .position(|i| i.item.source == stack.source && i.item.key == *target)
            {
                rr += 1.0 / (rank + 1) as f64;
                for (i, k) in KS.iter().enumerate() {
                    if rank < *k {
                        hits_at[i] += 1;
                    }
                }
            }
        }
        if let Some((_, d)) = &mut dump {
            d.push(format!("real.{mode}@{DEPTH}"), ranks);
        }
        let n = queries.len() as f64;
        for (i, k) in KS.iter().enumerate() {
            let mut m = Metric::new(
                format!("retrieval.{mode}.recall@{k}"),
                hits_at[i] as f64 / n,
                "ratio",
                Direction::HigherIsBetter,
                format!(
                    "Known-item queries over the synthetic corpus ingested from files; share whose \
                     target is in the Retriever's {mode} top {k}"
                ),
            );
            if *k == 10
                && mode == "hybrid"
                && let Some(b) = fake_hybrid
            {
                // M1 exit criterion: at or above the M0 fake-index baseline, like for like (the
                // fake hybrid has no MMR; `hybrid_mmr` is reported against the same baseline in
                // `bench/README.md` but not gated).
                m = m.with_target(b);
            }
            out.push(m);
        }
        out.push(Metric::new(
            format!("retrieval.{mode}.mrr@10"),
            rr / n,
            "ratio",
            Direction::HigherIsBetter,
            format!("Mean reciprocal rank of the target in the Retriever's {mode} top 10"),
        ));
        if strategy == Strategy::Auto {
            out.push(Metric::new(
                format!("retrieval.{mode}.text_only_fallbacks"),
                fallbacks as f64,
                "count",
                Direction::Info,
                "Queries the retriever answered text-only (embedding unavailable or failed)",
            ));
        }
    }
    for mode in ["bm25", "vector", "hybrid"] {
        for metric in ["recall@10", "mrr@10"] {
            if let Some(v) = baseline.get(&format!("retrieval.{mode}.{metric}")) {
                out.push(Metric::new(
                    format!("baseline.fake.retrieval.{mode}.{metric}"),
                    *v,
                    "ratio",
                    Direction::Info,
                    format!(
                        "M0 fake index (memory --backend fake) on the identical corpus and queries: \
                         {mode} {metric}"
                    ),
                ));
            }
        }
    }
    if let Some((path, mut d)) = dump {
        // Each single leg again at depth, to see how far a missed target fell.
        for (mode, strategy) in [
            ("bm25", Strategy::TextOnly),
            ("vector", Strategy::VectorOnly),
        ] {
            let mut ranks = Vec::with_capacity(queries.len());
            for q in queries {
                let res = stack
                    .retriever
                    .search_with(&q.text, RANK_DUMP_DEEP, opts(strategy, false))
                    .await?;
                ranks.push(doc_ids(stack, &key_ids, &res.items));
            }
            d.push(format!("real.{mode}"), ranks);
        }
        d.write(&path, queries)?;
    }
    if let Some(s) = summarize(&latencies_ms) {
        out.push(Metric::new(
            "query.latency.p50_ms",
            s.p50,
            "ms",
            Direction::LowerIsBetter,
            "Retriever::search latency (hybrid + MMR, query embedding included), median",
        ));
        out.push(Metric::new(
            "query.latency.p95_ms",
            s.p95,
            "ms",
            Direction::LowerIsBetter,
            "Retriever::search latency (hybrid + MMR, query embedding included), 95th percentile",
        ));
    }
    Ok(out)
}

/// Corpus document ids of `items`, best first (items of other sources are skipped).
fn doc_ids(
    stack: &Stack,
    ids: &HashMap<&ItemKey, usize>,
    items: &[nugget_retrieve::RetrievedItem],
) -> Vec<usize> {
    items
        .iter()
        .filter(|i| i.item.source == stack.source)
        .filter_map(|i| ids.get(&i.item.key).copied())
        .collect()
}

// ---------------------------------------------------------------- freshness

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Edit,
    Insert,
    Delete,
    Rename,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Edit => "edit",
            Kind::Insert => "insert",
            Kind::Delete => "delete",
            Kind::Rename => "rename",
        }
    }
}

/// A file operation waiting to show up in search.
struct Pending {
    kind: Kind,
    id: usize,
    /// The item's key after the change (for deletes: the deleted key).
    key: ItemKey,
    /// Rename: the key before.
    old_key: Option<ItemKey>,
    /// Marker that must become findable (edit, insert, rename).
    new_marker: Option<String>,
    /// Marker that must stop finding the item (edit, delete).
    old_marker: Option<String>,
    t0: Instant,
}

impl Pending {
    /// Whether search reflects the change completely.
    async fn done(&self, stack: &Stack) -> Result<bool, BenchError> {
        Ok(match self.kind {
            Kind::Insert => stack.visible(self.marker_new(), &self.key).await?,
            Kind::Edit => {
                stack.visible(self.marker_new(), &self.key).await?
                    && !stack.text_hit(self.marker_old(), &self.key).await?
            }
            Kind::Delete => {
                !stack.exists(&self.key)? && !stack.text_hit(self.marker_old(), &self.key).await?
            }
            Kind::Rename => {
                let old = self.old_key.as_ref().unwrap_or(&self.key);
                stack.visible(self.marker_new(), &self.key).await? && !stack.exists(old)?
            }
        })
    }

    fn marker_new(&self) -> &str {
        self.new_marker.as_deref().unwrap_or_default()
    }

    fn marker_old(&self) -> &str {
        self.old_marker.as_deref().unwrap_or_default()
    }
}

async fn freshness(
    stack: &Stack,
    corpus: &Corpus,
    p: &RealParams,
    seed: u64,
    live: &mut HashMap<usize, LiveDoc>,
) -> Result<Vec<Metric>, BenchError> {
    if p.changes == 0 {
        return Ok(Vec::new());
    }
    let mut rng = Rng::new(seed ^ 0xF2E5_4E55);
    let mut live_ids: Vec<usize> = {
        let mut v: Vec<usize> = live.keys().copied().collect();
        v.sort_unstable();
        v
    };
    let mut next_id = corpus.docs.len();
    let mut pending: Vec<Pending> = Vec::new();
    let mut lags: Vec<(Kind, f64)> = Vec::with_capacity(p.changes);
    let mut ok: BTreeMap<Kind, (usize, usize)> = BTreeMap::new();
    let mut touched: HashSet<usize> = HashSet::new();
    let mut timeouts = 0usize;
    let timeout = Duration::from_secs_f64(p.change_timeout_s);
    let probe = Duration::from_millis(p.probe_interval_ms.max(1));
    let gap = |rng: &mut Rng| Duration::from_secs_f64(rng.exp(p.change_gap_mean_s).min(60.0));
    let mut issued = 0usize;
    let mut next_at = Instant::now() + gap(&mut rng);

    while issued < p.changes || !pending.is_empty() {
        if issued < p.changes && Instant::now() >= next_at {
            let busy: HashSet<usize> = pending.iter().map(|c| c.id).collect();
            let r = rng.unit();
            let mut kind = if r < 0.5 {
                Kind::Edit
            } else if r < 0.7 {
                Kind::Insert
            } else if r < 0.85 {
                Kind::Delete
            } else {
                Kind::Rename
            };
            // An existing document that no pending change touches (bounded tries).
            let mut target = None;
            if kind != Kind::Insert && !live_ids.is_empty() {
                for _ in 0..32 {
                    let i = rng.below(live_ids.len());
                    if !busy.contains(&live_ids[i]) {
                        target = Some(i);
                        break;
                    }
                }
            }
            if target.is_none() {
                kind = Kind::Insert;
            }
            let change = match (kind, target) {
                (Kind::Insert, _) => {
                    let id = next_id;
                    next_id += 1;
                    let topic = rng.below(corpus.config.topics);
                    let doc = corpus.make_doc(&mut rng, id, topic);
                    let rel = rel_path(id, p.files_per_dir, 0);
                    stack.write(&rel, &file_text(&doc, 0))?;
                    let t0 = Instant::now();
                    live_ids.push(id);
                    let key = Stack::key(&rel);
                    live.insert(
                        id,
                        LiveDoc {
                            rel,
                            version: 0,
                            topic,
                            renames: 0,
                        },
                    );
                    Pending {
                        kind,
                        id,
                        key,
                        old_key: None,
                        new_marker: Some(marker(id, 0)),
                        old_marker: None,
                        t0,
                    }
                }
                (Kind::Edit, Some(i)) => {
                    let id = live_ids[i];
                    let d = live.get_mut(&id).ok_or_else(|| missing(id))?;
                    let doc = corpus.make_doc(&mut rng, id, d.topic);
                    d.version += 1;
                    stack.write(&d.rel, &file_text(&doc, d.version))?;
                    let t0 = Instant::now();
                    Pending {
                        kind,
                        id,
                        key: Stack::key(&d.rel),
                        old_key: None,
                        new_marker: Some(marker(id, d.version)),
                        old_marker: Some(marker(id, d.version - 1)),
                        t0,
                    }
                }
                (Kind::Delete, Some(i)) => {
                    let id = live_ids.swap_remove(i);
                    let d = live.remove(&id).ok_or_else(|| missing(id))?;
                    std::fs::remove_file(stack.path(&d.rel))?;
                    let t0 = Instant::now();
                    Pending {
                        kind,
                        id,
                        key: Stack::key(&d.rel),
                        old_key: None,
                        new_marker: None,
                        old_marker: Some(marker(id, d.version)),
                        t0,
                    }
                }
                (Kind::Rename, Some(i)) => {
                    let id = live_ids[i];
                    let d = live.get_mut(&id).ok_or_else(|| missing(id))?;
                    d.renames += 1;
                    let new_rel = rel_path(id, p.files_per_dir, d.renames);
                    std::fs::rename(stack.path(&d.rel), stack.path(&new_rel))?;
                    let t0 = Instant::now();
                    let old_key = Stack::key(&d.rel);
                    d.rel = new_rel;
                    Pending {
                        kind,
                        id,
                        key: Stack::key(&d.rel),
                        old_key: Some(old_key),
                        new_marker: Some(marker(id, d.version)),
                        old_marker: None,
                        t0,
                    }
                }
                (_, None) => unreachable!("non-insert changes always have a target"),
            };
            touched.insert(change.id);
            pending.push(change);
            issued += 1;
            next_at = Instant::now() + gap(&mut rng);
        }

        // Probe every pending change.
        let mut i = 0;
        while i < pending.len() {
            let c = &pending[i];
            let done = c.done(stack).await?;
            let lag = c.t0.elapsed();
            if done || lag >= timeout {
                let e = ok.entry(c.kind).or_insert((0, 0));
                e.1 += 1;
                if done {
                    e.0 += 1;
                    lags.push((c.kind, lag.as_secs_f64()));
                } else {
                    timeouts += 1;
                    lags.push((c.kind, timeout.as_secs_f64()));
                }
                pending.swap_remove(i);
            } else {
                i += 1;
            }
        }
        let mut wait = probe;
        if issued < p.changes {
            wait = wait.min(next_at.saturating_duration_since(Instant::now()));
        }
        tokio::time::sleep(wait).await;
    }

    let all: Vec<f64> = lags.iter().map(|(_, l)| *l).collect();
    let s = summarize(&all).ok_or_else(|| BenchError::Failed("no freshness samples".into()))?;
    let mut out = vec![
        Metric::new(
            "freshness.p50_s",
            s.p50,
            "s",
            Direction::LowerIsBetter,
            "Median time from a file operation on disk to Retriever::search reflecting it \
             (real watcher, pipeline and store)",
        ),
        Metric::new(
            "freshness.p95_s",
            s.p95,
            "s",
            Direction::LowerIsBetter,
            "95th-percentile file-operation-to-searchable time (M1 exit criterion: at most 60 s)",
        )
        .with_target(FRESHNESS_TARGET_S),
        Metric::new(
            "freshness.max_s",
            s.max,
            "s",
            Direction::LowerIsBetter,
            "Worst file-operation-to-searchable time (timeouts count at the timeout)",
        ),
        Metric::new(
            "freshness.changes",
            s.count as f64,
            "count",
            Direction::Info,
            "File operations measured (edits, creates, deletes, renames)",
        ),
        Metric::new(
            "freshness.timeouts",
            timeouts as f64,
            "count",
            Direction::LowerIsBetter,
            "File operations not reflected in search within the timeout",
        ),
    ];
    for kind in [Kind::Edit, Kind::Insert, Kind::Delete, Kind::Rename] {
        let v: Vec<f64> = lags
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, l)| *l)
            .collect();
        if let Some(s) = summarize(&v) {
            out.push(Metric::new(
                format!("freshness.{}.p95_s", kind.as_str()),
                s.p95,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "95th-percentile time until a file {} is reflected in search",
                    kind.as_str()
                ),
            ));
        }
    }
    for (kind, (good, total)) in &ok {
        out.push(
            Metric::new(
                format!("propagation.{}_rate", kind.as_str()),
                *good as f64 / *total as f64,
                "ratio",
                Direction::HigherIsBetter,
                format!(
                    "Share of file {}s fully reflected in search within the timeout (new content \
                     found; old content, deleted items and old paths gone)",
                    kind.as_str()
                ),
            )
            .with_target(1.0),
        );
    }

    // Final state: the store holds exactly the files on disk, and every touched file's current
    // content is what search finds.
    let expected: HashSet<ItemKey> = live.values().map(|d| Stack::key(&d.rel)).collect();
    let actual: HashSet<ItemKey> = stack
        .store
        .item_versions(&stack.source)?
        .into_keys()
        .collect();
    let union = expected.union(&actual).count().max(1);
    let inter = expected.intersection(&actual).count();
    out.push(
        Metric::new(
            "propagation.final_keys_match",
            inter as f64 / union as f64,
            "ratio",
            Direction::HigherIsBetter,
            "After all changes: Jaccard overlap of indexed item keys and files on disk",
        )
        .with_target(1.0),
    );
    let mut checked = 0usize;
    let mut matched = 0usize;
    for id in &touched {
        if let Some(d) = live.get(id) {
            checked += 1;
            if stack
                .text_hit(&marker(*id, d.version), &Stack::key(&d.rel))
                .await?
            {
                matched += 1;
            }
        }
    }
    if checked > 0 {
        out.push(
            Metric::new(
                "propagation.final_content_match",
                matched as f64 / checked as f64,
                "ratio",
                Direction::HigherIsBetter,
                "After all changes: share of changed files still on disk whose current content \
                 finds them",
            )
            .with_target(1.0),
        );
    }
    Ok(out)
}

fn missing(id: usize) -> BenchError {
    BenchError::Failed(format!("internal: live document {id} not tracked"))
}

// ---------------------------------------------------------------- resources

/// Main database file plus its WAL and shared-memory files.
pub(crate) fn db_bytes(path: &Path) -> u64 {
    let mut total = 0;
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        total += std::fs::metadata(&p).map_or(0, |m| m.len());
    }
    total
}

/// Peak resident set size of this process, where cheaply available (Linux `VmHWM`).
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_vm_hwm(&status)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_vm_hwm(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line
        .trim_start_matches("VmHWM:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_and_keys() {
        assert_eq!(rel_path(7, 250, 0), "d000/doc-7.md");
        assert_eq!(rel_path(512, 250, 2), "d002/doc-512-r2.md");
        assert_eq!(Stack::key("d000/doc-7.md").as_str(), "0:d000/doc-7.md");
    }

    #[test]
    fn vm_hwm_parsing() {
        let status = "Name:\tx\nVmPeak:\t  2000 kB\nVmHWM:\t    1536 kB\nVmRSS:\t 1000 kB\n";
        assert_eq!(parse_vm_hwm(status), Some(1536 * 1024));
        assert_eq!(parse_vm_hwm("nothing"), None);
    }

    #[test]
    fn params_match_the_fake_corpus() {
        for scale in [Scale::Smoke, Scale::Quick, Scale::Standard, Scale::Full] {
            let r = RealParams::for_scale(scale);
            let f = MemoryParams::for_scale(scale);
            assert_eq!(
                (r.docs, r.queries, r.query_terms),
                (f.docs, f.queries, f.query_terms)
            );
            assert!(r.change_timeout_s > FRESHNESS_TARGET_S);
        }
    }
}
