//! `memory`: retrieval quality and knowledge freshness.
//!
//! * **Retrieval.** Known-item queries over a synthetic corpus; recall@{1,5,10} and MRR@10
//!   for BM25, vector and hybrid search.
//! * **Freshness.** A stream of edits, inserts and deletes arrives at random times. A
//!   simulated watcher batches them (debounce plus a maximum batching delay) and the batch is
//!   applied to the index for real. Freshness per change is the simulated wait plus the
//!   measured apply time; propagation checks that edits replace old content and deletes remove
//!   the item everywhere. The plan's target is about 60 s from change to searchable.
//!
//! `--backend fake` (the default) runs against the in-crate [`FakeIndex`] with a simulated
//! watcher. `--backend real` runs the real stack in process ([`crate::real`]) and keeps the
//! metric names stable where they mean the same thing.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::corpus::{Corpus, CorpusConfig, Query, Rng, marker};
use crate::index::FakeIndex;
use crate::report::{Direction, Metric};
use crate::runner::{Backend, BenchError, Benchmark, RunContext, Scale};
use crate::stats::summarize;
use crate::suites::{doc_text, item_ref};

/// Freshness target from `docs/PLAN.md` §2: about 60 s from change to searchable.
pub const FRESHNESS_TARGET_S: f64 = 60.0;

/// `memory --backend real` options (flattened into the CLI). With `--local-models` the real
/// backend embeds with the manifest's embedding model served by `nugget-runtime`
/// (`--local-embed-model`), as the app does, instead of the in-process hash embedder.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct MemoryOptions {
    /// `memory --backend real --local-models`: also start the manifest's reranker
    /// (`--local-rerank-model`) and rerank search results with it (`local`), or not (`off`, the
    /// default, as the app runs without `rerank_model`).
    #[arg(long, value_enum, default_value_t = MemoryRerank::Off)]
    pub memory_rerank: MemoryRerank,
    /// `memory --backend real`: the reranker's request timeout in milliseconds (default: the
    /// app's, `RetrieveConfig::rerank_timeout_ms`). A rerank that times out keeps the first-stage
    /// order, as in the app.
    #[arg(long, value_name = "MS")]
    pub memory_rerank_timeout_ms: Option<u64>,
    /// `memory --backend real`: candidates the reranker scores per query (default: the app's,
    /// `RetrieveConfig::rerank_pool`).
    #[arg(long, value_name = "N")]
    pub memory_rerank_pool: Option<usize>,
    /// `memory --backend real`: the embedding model's task prefixes (`auto`: the model's own, e.g.
    /// `search_query: ` / `search_document: ` for nomic-embed-text, as the app sends them; `off`:
    /// none).
    #[arg(long, value_enum, default_value_t = EmbedPrefixes::Auto)]
    pub memory_embed_prefixes: EmbedPrefixes,
    /// `memory`: known-item queries (default: the scale's). Corpus and query generation are
    /// unchanged; the first N queries of the scale's sequence are asked.
    #[arg(long, value_name = "N")]
    pub memory_queries: Option<usize>,
    /// `memory --backend real`: skip the freshness phase (retrieval quality only).
    #[arg(long)]
    pub memory_no_freshness: bool,
}

impl Default for MemoryOptions {
    fn default() -> Self {
        Self {
            memory_rerank: MemoryRerank::Off,
            memory_rerank_timeout_ms: None,
            memory_rerank_pool: None,
            memory_embed_prefixes: EmbedPrefixes::Auto,
            memory_queries: None,
            memory_no_freshness: false,
        }
    }
}

/// Whether `memory --backend real` reranks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum MemoryRerank {
    #[default]
    Off,
    /// The manifest's reranker on the local runtime (needs `--local-models`).
    Local,
}

/// Task prefixes the embedding model is called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum EmbedPrefixes {
    /// The model's own (none for models without task prefixes).
    #[default]
    Auto,
    /// None.
    Off,
}

impl EmbedPrefixes {
    pub fn as_str(self) -> &'static str {
        match self {
            EmbedPrefixes::Auto => "auto",
            EmbedPrefixes::Off => "off",
        }
    }
}

pub struct MemoryBench;

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryParams {
    pub docs: usize,
    pub queries: usize,
    pub query_terms: usize,
    pub query_noise: f64,
    pub changes: usize,
    pub dim: usize,
    /// Mean simulated time between changes.
    pub arrival_mean_s: f64,
    /// Watcher flushes after this much quiet time...
    pub debounce_s: f64,
    /// ...or once the oldest pending change has waited this long.
    pub max_batch_delay_s: f64,
}

impl MemoryParams {
    pub fn for_scale(scale: Scale) -> Self {
        Self {
            docs: scale.pick(300, 2_000, 5_000, 20_000),
            queries: scale.pick(50, 200, 500, 1_000),
            query_terms: 4,
            query_noise: 0.25,
            changes: scale.pick(60, 300, 1_000, 5_000),
            dim: 256,
            arrival_mean_s: 0.5,
            debounce_s: 2.0,
            max_batch_delay_s: 10.0,
        }
    }
}

impl Benchmark for MemoryBench {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn description(&self) -> &'static str {
        "Retrieval recall@k/MRR and change-to-searchable freshness with edit/delete propagation (--backend fake|real)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        match ctx.backend() {
            Backend::Fake => run_with(ctx, &MemoryParams::for_scale(ctx.scale())),
            Backend::Real => crate::real::run(ctx),
        }
    }
}

pub fn run_with(ctx: &mut RunContext, p: &MemoryParams) -> Result<Vec<Metric>, BenchError> {
    let p = &MemoryParams {
        queries: ctx.config.memory.memory_queries.unwrap_or(p.queries),
        ..p.clone()
    };
    if p.docs == 0 || p.queries == 0 || p.query_terms == 0 {
        return Err(BenchError::Failed(
            "memory needs documents, queries and query terms".into(),
        ));
    }
    ctx.param("backend", "fake_index");
    ctx.param("docs", p.docs);
    ctx.param("queries", p.queries);
    ctx.param("query_terms", p.query_terms);
    ctx.param("query_noise", p.query_noise);
    ctx.param("changes", p.changes);
    ctx.param("embedding_dim", p.dim);
    ctx.param("arrival_mean_s", p.arrival_mean_s);
    ctx.param("debounce_s", p.debounce_s);
    ctx.param("max_batch_delay_s", p.max_batch_delay_s);

    let corpus = Corpus::generate(CorpusConfig {
        docs: p.docs,
        seed: ctx.seed(),
        ..CorpusConfig::default()
    });
    let mut ix = FakeIndex::new(p.dim);
    for doc in &corpus.docs {
        ix.upsert(&item_ref(doc.id), &doc_text(doc, 0));
    }

    let mut out = retrieval(&corpus, &ix, p, ctx.seed());
    if let Some(path) = rank_dump_path() {
        let queries = corpus.queries(p.queries, p.query_terms, p.query_noise, ctx.seed());
        let mut dump = RankDump::default();
        dump.add_fake_arms(&ix, &queries);
        dump.write(&path, &queries)?;
    }
    out.extend(freshness(&corpus, &mut ix, p, ctx.seed())?);
    Ok(out)
}

/// Known-item retrieval metrics of `ix` (also the real backend's baseline).
pub(crate) fn retrieval(
    corpus: &Corpus,
    ix: &FakeIndex,
    p: &MemoryParams,
    seed: u64,
) -> Vec<Metric> {
    const KS: [usize; 3] = [1, 5, 10];
    let queries = corpus.queries(p.queries, p.query_terms, p.query_noise, seed);
    let mut out = Vec::new();
    for mode in ["bm25", "vector", "hybrid"] {
        let mut hits_at = [0usize; KS.len()];
        let mut rr = 0.0;
        for q in &queries {
            let hits = match mode {
                "bm25" => ix.search_text(&q.text, 10),
                "vector" => ix.search_vector(&ix.embed(&q.text), 10),
                _ => ix.search_hybrid(&q.text, 10),
            };
            let target = item_ref(q.relevant);
            if let Some(rank) = hits.iter().position(|h| h.item == target) {
                rr += 1.0 / (rank + 1) as f64;
                for (i, k) in KS.iter().enumerate() {
                    if rank < *k {
                        hits_at[i] += 1;
                    }
                }
            }
        }
        let n = queries.len() as f64;
        for (i, k) in KS.iter().enumerate() {
            out.push(Metric::new(
                format!("retrieval.{mode}.recall@{k}"),
                hits_at[i] as f64 / n,
                "ratio",
                Direction::HigherIsBetter,
                format!("Share of known-item queries whose target is in the {mode} top {k}"),
            ));
        }
        out.push(Metric::new(
            format!("retrieval.{mode}.mrr@10"),
            rr / n,
            "ratio",
            Direction::HigherIsBetter,
            format!("Mean reciprocal rank of the target in the {mode} top 10"),
        ));
    }
    out
}

/// Diagnostic switch: when this environment variable names a file, the known-item phase writes
/// the target's rank in every retrieval arm, per query, to it as TSV. Off by default; the report
/// is the same either way. It exists to explain recall gaps between the real stack and the fake
/// baseline query by query instead of guessing from the aggregate.
pub const RANK_DUMP_ENV: &str = "NUGGET_BENCH_DUMP_MISSES";

/// Depth of the extra single-leg searches in a rank dump, to see how far a missed target fell.
pub(crate) const RANK_DUMP_DEEP: usize = 100;

pub(crate) fn rank_dump_path() -> Option<PathBuf> {
    std::env::var_os(RANK_DUMP_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Per-query results (document ids, best first) of named retrieval arms.
#[derive(Debug, Default)]
pub(crate) struct RankDump {
    arms: Vec<(String, Vec<Vec<usize>>)>,
}

impl RankDump {
    pub(crate) fn push(&mut self, arm: impl Into<String>, results: Vec<Vec<usize>>) {
        self.arms.push((arm.into(), results));
    }

    fn results(&self, arm: &str, q: usize) -> Option<&[usize]> {
        self.arms
            .iter()
            .find(|(a, _)| a == arm)
            .and_then(|(_, r)| r.get(q))
            .map(Vec::as_slice)
    }

    /// The fake index's arms: the metric's hybrid top 10, and each leg alone at depth.
    pub(crate) fn add_fake_arms(&mut self, ix: &FakeIndex, queries: &[Query]) {
        let ids = |hits: Vec<crate::index::Hit>| -> Vec<usize> {
            hits.iter()
                .filter_map(|h| h.item.key.0.strip_prefix("doc-")?.parse().ok())
                .collect()
        };
        let arm = |f: &dyn Fn(&str) -> Vec<crate::index::Hit>| -> Vec<Vec<usize>> {
            queries.iter().map(|q| ids(f(&q.text))).collect()
        };
        self.push("fake.hybrid@10", arm(&|t| ix.search_hybrid(t, 10)));
        self.push("fake.bm25", arm(&|t| ix.search_text(t, RANK_DUMP_DEEP)));
        self.push(
            "fake.vector",
            arm(&|t| ix.search_vector(&ix.embed(t), RANK_DUMP_DEEP)),
        );
    }

    /// TSV: one row per query with the target's 1-based rank in each arm (`-` when absent).
    /// Then, when both hybrid arms are present, `#` lines for every query whose hybrid top-10
    /// outcome differs between the real stack and the fake index: each side's top 10 as
    /// `doc(bm25 rank/vector rank)` from that side's single-leg arms, which shows what pushed
    /// the target out.
    pub(crate) fn write(&self, path: &Path, queries: &[Query]) -> Result<(), BenchError> {
        let rank = |arm: &str, i: usize, id: usize| {
            self.results(arm, i)
                .and_then(|r| r.iter().position(|&d| d == id))
        };
        let mut s = String::from("query\ttarget\ttext");
        for (arm, _) in &self.arms {
            let _ = write!(s, "\t{arm}");
        }
        s.push('\n');
        for (i, q) in queries.iter().enumerate() {
            let _ = write!(s, "{i}\t{}\t{}", q.relevant, q.text);
            for (arm, _) in &self.arms {
                match rank(arm, i, q.relevant) {
                    Some(r) => {
                        let _ = write!(s, "\t{}", r + 1);
                    }
                    None => s.push_str("\t-"),
                }
            }
            s.push('\n');
        }
        let hit = |side: &str, i: usize| {
            self.results(&format!("{side}.hybrid@10"), i)
                .map(|r| r.iter().take(10).any(|&d| d == queries[i].relevant))
        };
        let fmt_rank =
            |r: Option<usize>| r.map_or_else(|| "-".to_string(), |r| (r + 1).to_string());
        for (label, real_hit) in [("real_only", true), ("fake_only", false)] {
            let diff: Vec<usize> = (0..queries.len())
                .filter(|&i| hit("real", i) == Some(real_hit) && hit("fake", i) == Some(!real_hit))
                .collect();
            let ids: Vec<String> = diff.iter().map(usize::to_string).collect();
            let _ = writeln!(s, "# hybrid@10 {label}: {}", ids.join(" "));
            for &i in &diff {
                let q = &queries[i];
                let _ = writeln!(s, "#   q{i} target {} \"{}\"", q.relevant, q.text);
                for side in ["fake", "real"] {
                    let top = self
                        .results(&format!("{side}.hybrid@10"), i)
                        .unwrap_or_default();
                    let cells: Vec<String> = top
                        .iter()
                        .take(10)
                        .map(|&d| {
                            format!(
                                "{d}{}({}/{})",
                                if d == q.relevant { "*" } else { "" },
                                fmt_rank(rank(&format!("{side}.bm25"), i, d)),
                                fmt_rank(rank(&format!("{side}.vector"), i, d)),
                            )
                        })
                        .collect();
                    let _ = writeln!(
                        s,
                        "#     {side}: target {}/{} | {}",
                        fmt_rank(rank(&format!("{side}.bm25"), i, q.relevant)),
                        fmt_rank(rank(&format!("{side}.vector"), i, q.relevant)),
                        cells.join(" ")
                    );
                }
            }
        }
        std::fs::write(path, s)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Edit,
    Insert,
    Delete,
}

#[derive(Debug)]
struct Change {
    at: f64,
    kind: Kind,
    id: usize,
    /// Version after the change (edit/insert) or the version deleted (delete).
    version: u32,
    /// Text to index for edits and inserts, rendered ahead of the timed region.
    text: Option<String>,
}

fn generate_changes(corpus: &Corpus, p: &MemoryParams, seed: u64) -> Vec<Change> {
    let mut rng = Rng::new(seed ^ 0xF4E5_4E55);
    let mut live: Vec<usize> = (0..corpus.docs.len()).collect();
    let mut versions: HashMap<usize, u32> = HashMap::new();
    let mut next_id = corpus.docs.len();
    let mut at = 0.0;
    let mut out = Vec::with_capacity(p.changes);
    for _ in 0..p.changes {
        at += rng.exp(p.arrival_mean_s);
        let r = rng.unit();
        let kind = if live.is_empty() || (0.6..0.8).contains(&r) {
            Kind::Insert
        } else if r < 0.6 {
            Kind::Edit
        } else {
            Kind::Delete
        };
        let change = match kind {
            Kind::Insert => {
                let id = next_id;
                next_id += 1;
                live.push(id);
                versions.insert(id, 0);
                let topic = rng.below(corpus.config.topics);
                let doc = corpus.make_doc(&mut rng, id, topic);
                Change {
                    at,
                    kind,
                    id,
                    version: 0,
                    text: Some(doc_text(&doc, 0)),
                }
            }
            Kind::Edit => {
                let id = live[rng.below(live.len())];
                let v = versions.entry(id).or_insert(0);
                *v += 1;
                let version = *v;
                let topic = corpus.docs.get(id).map_or(0, |d| d.topic);
                let doc = corpus.make_doc(&mut rng, id, topic);
                Change {
                    at,
                    kind,
                    id,
                    version,
                    text: Some(doc_text(&doc, version)),
                }
            }
            Kind::Delete => {
                let id = live.swap_remove(rng.below(live.len()));
                let version = versions.remove(&id).unwrap_or(0);
                Change {
                    at,
                    kind,
                    id,
                    version,
                    text: None,
                }
            }
        };
        out.push(change);
    }
    out
}

/// Whether the index reflects `final_state` for `id`: the current version's marker finds it
/// first, or (if deleted) nothing finds it.
fn state_matches(ix: &FakeIndex, id: usize, final_state: Option<u32>, last_version: u32) -> bool {
    let item = item_ref(id);
    match final_state {
        Some(v) => ix
            .search_text(&marker(id, v), 1)
            .first()
            .is_some_and(|h| h.item == item),
        None => {
            !ix.contains(&item)
                && ix
                    .search_text(&marker(id, last_version), 10)
                    .iter()
                    .all(|h| h.item != item)
        }
    }
}

fn freshness(
    corpus: &Corpus,
    ix: &mut FakeIndex,
    p: &MemoryParams,
    seed: u64,
) -> Result<Vec<Metric>, BenchError> {
    let changes = generate_changes(corpus, p, seed);
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let mut lag = Vec::with_capacity(changes.len());
    let mut apply_ms = Vec::new();
    let mut ok: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    let mut indexer_free = 0.0f64;
    let mut i = 0;
    while i < changes.len() {
        // Grow the batch while changes keep arriving within the debounce window.
        let first_at = changes[i].at;
        let mut j = i;
        while j + 1 < changes.len()
            && changes[j + 1].at - changes[j].at <= p.debounce_s
            && changes[j + 1].at <= first_at + p.max_batch_delay_s
        {
            j += 1;
        }
        let flush_at = (changes[j].at + p.debounce_s).min(first_at + p.max_batch_delay_s);
        let start = flush_at.max(indexer_free);
        let batch = &changes[i..=j];

        let t = Instant::now();
        for c in batch {
            let item = item_ref(c.id);
            match (&c.kind, &c.text) {
                (Kind::Delete, _) => {
                    ix.delete(&item);
                }
                (_, Some(text)) => {
                    ix.upsert(&item, text);
                }
                (_, None) => return Err(BenchError::Failed("edit without text".into())),
            }
        }
        let wall = t.elapsed().as_secs_f64();
        apply_ms.push(wall * 1000.0);
        let done = start + wall;
        indexer_free = done;
        lag.extend(batch.iter().map(|c| done - c.at));

        // Final state per touched id within this batch, then verify each change against it.
        let mut final_state: HashMap<usize, (Option<u32>, u32)> = HashMap::new();
        for c in batch {
            let state = if c.kind == Kind::Delete {
                None
            } else {
                Some(c.version)
            };
            final_state.insert(c.id, (state, c.version));
        }
        let verified: HashMap<usize, bool> = final_state
            .iter()
            .map(|(&id, &(state, last))| (id, state_matches(ix, id, state, last)))
            .collect();
        for c in batch {
            let mut good = verified[&c.id];
            if c.kind == Kind::Edit && good {
                // The previous version's content must be gone.
                let prev = marker(c.id, c.version - 1);
                good = ix
                    .search_text(&prev, 10)
                    .iter()
                    .all(|h| h.item != item_ref(c.id));
            }
            let key = match c.kind {
                Kind::Edit => "edit",
                Kind::Insert => "insert",
                Kind::Delete => "delete",
            };
            let e = ok.entry(key).or_insert((0, 0));
            e.1 += 1;
            if good {
                e.0 += 1;
            }
        }
        i = j + 1;
    }

    let lag = summarize(&lag).ok_or_else(|| BenchError::Failed("no freshness samples".into()))?;
    let apply =
        summarize(&apply_ms).ok_or_else(|| BenchError::Failed("no apply samples".into()))?;
    let mut out = vec![
        Metric::new(
            "freshness.p50_s",
            lag.p50,
            "s",
            Direction::LowerIsBetter,
            "Median change-to-searchable time (simulated watcher batching + measured apply)",
        ),
        Metric::new(
            "freshness.p95_s",
            lag.p95,
            "s",
            Direction::LowerIsBetter,
            "95th-percentile change-to-searchable time",
        )
        .with_target(FRESHNESS_TARGET_S),
        Metric::new(
            "freshness.max_s",
            lag.max,
            "s",
            Direction::LowerIsBetter,
            "Worst change-to-searchable time",
        ),
        Metric::new(
            "freshness.batch_apply.p95_ms",
            apply.p95,
            "ms",
            Direction::LowerIsBetter,
            "95th-percentile wall time to apply one watcher batch to the index",
        ),
        Metric::new(
            "freshness.batches",
            apply.count as f64,
            "count",
            Direction::Info,
            "Number of watcher batches",
        ),
    ];
    for (key, (good, total)) in ok {
        out.push(
            Metric::new(
                format!("propagation.{key}_rate"),
                good as f64 / total as f64,
                "ratio",
                Direction::HigherIsBetter,
                format!("Share of {key}s whose effect is visible (and old content gone) after their batch"),
            )
            .with_target(1.0),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;
    use crate::runner::{RunConfig, run_benchmark};

    fn smoke() -> crate::Report {
        run_benchmark(
            &MemoryBench,
            &RunConfig {
                scale: Scale::Smoke,
                seed: 11,
                ..RunConfig::default()
            },
        )
    }

    #[test]
    fn smoke_run_produces_sane_metrics() {
        let r = smoke();
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        for mode in ["bm25", "vector", "hybrid"] {
            let mut prev = 0.0;
            for k in [1, 5, 10] {
                let m = r.metric(&format!("retrieval.{mode}.recall@{k}")).unwrap();
                assert!((0.0..=1.0).contains(&m.value));
                assert!(m.value >= prev, "recall must be monotone in k");
                prev = m.value;
            }
            let mrr = r.metric(&format!("retrieval.{mode}.mrr@10")).unwrap().value;
            assert!(mrr <= prev + 1e-12);
        }
        // Lexical known-item search over distinctive words should mostly succeed.
        assert!(r.metric("retrieval.bm25.recall@10").unwrap().value > 0.5);

        let p95 = r.metric("freshness.p95_s").unwrap();
        assert!(p95.value > 0.0);
        assert_eq!(p95.target, Some(FRESHNESS_TARGET_S));
        // Batching bounds the simulated wait by the max batching delay plus apply time.
        assert!(r.metric("freshness.max_s").unwrap().value < 10.0 + 5.0);
        for kind in ["edit", "insert", "delete"] {
            let m = r.metric(&format!("propagation.{kind}_rate")).unwrap();
            assert_eq!(m.value, 1.0, "{kind} propagation");
            assert_eq!(m.target_met, Some(true));
        }
    }

    #[test]
    fn deterministic_quality_for_a_seed() {
        let a = smoke();
        let b = smoke();
        for name in [
            "retrieval.hybrid.recall@10",
            "retrieval.bm25.mrr@10",
            "freshness.batches",
        ] {
            assert_eq!(
                a.metric(name).unwrap().value,
                b.metric(name).unwrap().value,
                "{name}"
            );
        }
    }

    #[test]
    fn rank_dump_lists_ranks_and_hybrid_differences() {
        let queries = vec![
            Query {
                text: "alpha".into(),
                relevant: 7,
            },
            Query {
                text: "beta".into(),
                relevant: 8,
            },
        ];
        let mut d = RankDump::default();
        d.push("fake.hybrid@10", vec![vec![1, 7], vec![1, 2]]);
        d.push("fake.bm25", vec![vec![7, 1], vec![8]]);
        d.push("fake.vector", vec![vec![1], vec![]]);
        d.push("real.hybrid@10", vec![vec![1, 2], vec![8]]);
        d.push("real.bm25", vec![vec![2, 7], vec![8]]);
        d.push("real.vector", vec![vec![1, 2], vec![]]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ranks.tsv");
        d.write(&path, &queries).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("query\ttarget\ttext\tfake.hybrid@10"));
        assert_eq!(lines[1], "0\t7\talpha\t2\t1\t-\t-\t2\t-");
        assert_eq!(lines[2], "1\t8\tbeta\t-\t1\t-\t1\t1\t-");
        assert!(text.contains("# hybrid@10 real_only: 1\n"));
        assert!(text.contains("# hybrid@10 fake_only: 0\n"));
        assert!(text.contains("#     fake: target 1/- | 1(2/1) 7*(1/-)"));
        assert!(text.contains("#     real: target 2/- | 1(-/1) 2(1/2)"));
    }

    #[test]
    fn change_stream_is_consistent() {
        let corpus = Corpus::generate(CorpusConfig {
            docs: 20,
            ..CorpusConfig::default()
        });
        let p = MemoryParams {
            changes: 200,
            ..MemoryParams::for_scale(Scale::Smoke)
        };
        let changes = generate_changes(&corpus, &p, 3);
        assert_eq!(changes.len(), 200);
        assert!(changes.windows(2).all(|w| w[0].at <= w[1].at));
        let mut deleted = std::collections::HashSet::new();
        for c in &changes {
            assert!(!deleted.contains(&c.id), "change to a deleted doc");
            match c.kind {
                Kind::Delete => {
                    deleted.insert(c.id);
                    assert!(c.text.is_none());
                }
                _ => assert!(
                    c.text
                        .as_deref()
                        .is_some_and(|t| t.contains(&marker(c.id, c.version)))
                ),
            }
        }
    }
}
