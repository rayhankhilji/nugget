//! `perf`: cost of the indexing and query paths.
//!
//! `--backend fake` (the default) measures the in-crate [`FakeIndex`] so the harness, schema and
//! CI plumbing are exercised end to end. `--backend real` measures the real encrypted
//! `nugget-store` and `nugget-retrieve` (written directly, without the ingest pipeline): write
//! throughput, text, vector and hybrid search, `Retriever::search` and `build_context` latency,
//! vector recall against exact search and edit freshness (see [`crate::suites::scale`]); then the
//! `nuggetd` binary itself on a seeded store, over IPC: startup, search, knowledge extraction,
//! the vault render, idle CPU and wakeups, RSS and ingest (`daemon.*`, see
//! [`crate::suites::perf_daemon`]; skipped when no binary is found).

use std::time::Instant;

use crate::corpus::{Corpus, CorpusConfig, Rng};
use crate::index::{FakeIndex, UpsertOutcome};
use crate::report::{Direction, Metric};
use crate::runner::{Backend, BenchError, Benchmark, RunContext, Scale};
use crate::stats::summarize;
use crate::suites::{doc_text, item_ref, secs};

pub struct PerfBench;

/// Size mix of the synthetic items in `perf --backend real`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PerfSizes {
    /// Every item the same length (120 words), as W12 and X5 measured.
    #[default]
    Fixed,
    /// A mailbox-like mix: log-normal lengths with a median of 80 words and a long tail
    /// (see [`crate::corpus::MAIL_LENGTH_SIGMA`]).
    Mail,
}

impl PerfSizes {
    /// `(median or fixed words per item, σ of ln(words))`.
    pub fn shape(self, fixed_words: usize) -> (usize, f64) {
        match self {
            PerfSizes::Fixed => (fixed_words, 0.0),
            PerfSizes::Mail => (80, crate::corpus::MAIL_LENGTH_SIGMA),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PerfSizes::Fixed => "fixed",
            PerfSizes::Mail => "mail",
        }
    }
}

/// Which parts of `perf --backend real` run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PerfPhases {
    /// The in-process store run and the daemon profile.
    #[default]
    All,
    /// Only the in-process store and retriever run (`crate::suites::scale`).
    Store,
    /// Only the `nuggetd` profile (`crate::suites::perf_daemon`).
    Daemon,
}

/// `perf --backend real` options (flattened into the CLI). The II5 scale runs use
/// `--perf-items 250000 --perf-sizes mail --perf-phases daemon`.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct PerfOptions {
    /// `perf --backend real`: items in the store run and the daemon profile (default: the
    /// scale's, 50k / 100k at `full`). The other workload sizes (queries, searches, timeouts)
    /// stay the scale's, except that waits grow with the item count.
    #[arg(long, value_name = "N")]
    pub perf_items: Option<usize>,
    /// `perf --backend real`: item size mix.
    #[arg(long, value_enum, default_value_t = PerfSizes::Fixed)]
    pub perf_sizes: PerfSizes,
    /// `perf --backend real`: run the store run, the daemon profile, or both.
    #[arg(long, value_enum, default_value_t = PerfPhases::All)]
    pub perf_phases: PerfPhases,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PerfParams {
    pub docs: usize,
    pub words_per_doc: usize,
    pub updates: usize,
    pub deletes: usize,
    pub queries: usize,
    pub dim: usize,
}

impl PerfParams {
    pub fn for_scale(scale: Scale) -> Self {
        Self {
            docs: scale.pick(300, 2_000, 10_000, 50_000),
            words_per_doc: 120,
            updates: scale.pick(50, 500, 2_000, 5_000),
            deletes: scale.pick(20, 200, 1_000, 2_000),
            queries: scale.pick(30, 200, 500, 1_000),
            dim: 256,
        }
    }
}

impl Benchmark for PerfBench {
    fn name(&self) -> &'static str {
        "perf"
    }

    fn description(&self) -> &'static str {
        "Ingest throughput, dedup, update/delete and query latency (--backend fake: in-crate index; real: nugget-store + nugget-retrieve)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        match ctx.backend() {
            Backend::Fake => run_with(ctx, &PerfParams::for_scale(ctx.scale())),
            Backend::Real => {
                let phases = ctx.config.perf.perf_phases;
                ctx.param(
                    "perf_phases",
                    match phases {
                        PerfPhases::All => "all",
                        PerfPhases::Store => "store",
                        PerfPhases::Daemon => "daemon",
                    },
                );
                let mut out = Vec::new();
                if phases != PerfPhases::Daemon {
                    out.extend(crate::suites::scale::run(ctx)?);
                }
                if phases != PerfPhases::Store {
                    out.extend(crate::suites::perf_daemon::run(ctx)?);
                }
                Ok(out)
            }
        }
    }
}

pub(crate) fn us(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

pub(crate) fn latency_metrics(
    out: &mut Vec<Metric>,
    prefix: &str,
    what: &str,
    samples: &[f64],
) -> Result<(), BenchError> {
    let s =
        summarize(samples).ok_or_else(|| BenchError::Failed(format!("no samples for {prefix}")))?;
    out.push(Metric::new(
        format!("{prefix}.p50_us"),
        s.p50,
        "us",
        Direction::LowerIsBetter,
        format!("Median latency of {what}"),
    ));
    out.push(Metric::new(
        format!("{prefix}.p95_us"),
        s.p95,
        "us",
        Direction::LowerIsBetter,
        format!("95th-percentile latency of {what}"),
    ));
    Ok(())
}

pub fn run_with(ctx: &mut RunContext, p: &PerfParams) -> Result<Vec<Metric>, BenchError> {
    if p.docs == 0 || p.queries == 0 {
        return Err(BenchError::Failed(
            "perf needs at least one document and one query".into(),
        ));
    }
    ctx.param("backend", "fake_index");
    ctx.param("docs", p.docs);
    ctx.param("words_per_doc", p.words_per_doc);
    ctx.param("updates", p.updates);
    ctx.param("deletes", p.deletes);
    ctx.param("queries", p.queries);
    ctx.param("embedding_dim", p.dim);

    let corpus = Corpus::generate(CorpusConfig {
        docs: p.docs,
        words_per_doc: p.words_per_doc,
        seed: ctx.seed(),
        ..CorpusConfig::default()
    });
    // Render outside the timed region: we measure indexing, not corpus generation.
    let items: Vec<_> = (0..p.docs).map(item_ref).collect();
    let texts: Vec<String> = corpus.docs.iter().map(|d| doc_text(d, 0)).collect();
    let bytes: usize = texts.iter().map(String::len).sum();

    let mut out = Vec::new();
    let mut ix = FakeIndex::new(p.dim);

    // Fresh ingest.
    let t = Instant::now();
    for (item, text) in items.iter().zip(&texts) {
        if ix.upsert(item, text) != UpsertOutcome::Inserted {
            return Err(BenchError::Failed(format!("{} was not inserted", item.key)));
        }
    }
    let ingest = secs(t.elapsed());
    out.push(Metric::new(
        "ingest.items_per_s",
        p.docs as f64 / ingest,
        "items/s",
        Direction::HigherIsBetter,
        "Fresh ingest throughput: tokenize, embed and index each item (single thread)",
    ));
    out.push(Metric::new(
        "ingest.mib_per_s",
        bytes as f64 / (1024.0 * 1024.0) / ingest,
        "MiB/s",
        Direction::HigherIsBetter,
        "Fresh ingest throughput in text bytes",
    ));
    out.push(Metric::new(
        "ingest.ms_per_1k_items",
        ingest * 1000.0 * 1000.0 / p.docs as f64,
        "ms",
        Direction::LowerIsBetter,
        "Wall time to ingest 1k items on one core (proxy for CPU cost per 1k items)",
    ));
    out.push(Metric::new(
        "index.est_bytes_per_1k_items",
        ix.approx_heap_bytes() as f64 * 1000.0 / p.docs as f64,
        "bytes",
        Direction::LowerIsBetter,
        "Estimated index heap per 1k items (structure estimate, not allocator-measured)",
    ));

    // Re-ingest of unchanged content: the dedup fast path every rescan hits.
    let t = Instant::now();
    for (item, text) in items.iter().zip(&texts) {
        if ix.upsert(item, text) != UpsertOutcome::Unchanged {
            return Err(BenchError::Failed(format!(
                "{} was not deduplicated",
                item.key
            )));
        }
    }
    out.push(Metric::new(
        "reingest_unchanged.items_per_s",
        p.docs as f64 / secs(t.elapsed()),
        "items/s",
        Direction::HigherIsBetter,
        "Throughput when content is unchanged (content-hash dedup)",
    ));

    // Incremental updates to existing items.
    let mut rng = Rng::new(ctx.seed() ^ 0x0BAD_5EED);
    let mut versions = vec![0u32; p.docs];
    let mut samples = Vec::with_capacity(p.updates);
    for _ in 0..p.updates {
        let id = rng.below(p.docs);
        versions[id] += 1;
        let doc = corpus.make_doc(&mut rng, id, corpus.docs[id].topic);
        let text = doc_text(&doc, versions[id]);
        let t = Instant::now();
        let outcome = ix.upsert(&items[id], &text);
        samples.push(us(t.elapsed()));
        if outcome != UpsertOutcome::Updated {
            return Err(BenchError::Failed(format!(
                "update of {} returned {outcome:?}",
                items[id].key
            )));
        }
    }
    if !samples.is_empty() {
        latency_metrics(
            &mut out,
            "update.latency",
            "updating one existing item",
            &samples,
        )?;
    }

    // Queries.
    let queries = corpus.queries(p.queries, 4, 0.2, ctx.seed());
    let mut bm25 = Vec::with_capacity(queries.len());
    let mut vector = Vec::with_capacity(queries.len());
    let mut hybrid = Vec::with_capacity(queries.len());
    for q in &queries {
        let t = Instant::now();
        std::hint::black_box(ix.search_text(&q.text, 10));
        bm25.push(us(t.elapsed()));
        let t = Instant::now();
        let e = ix.embed(&q.text);
        std::hint::black_box(ix.search_vector(&e, 10));
        vector.push(us(t.elapsed()));
        let t = Instant::now();
        std::hint::black_box(ix.search_hybrid(&q.text, 10));
        hybrid.push(us(t.elapsed()));
    }
    latency_metrics(&mut out, "query.bm25.latency", "a BM25 top-10 query", &bm25)?;
    latency_metrics(
        &mut out,
        "query.vector.latency",
        "embedding plus a brute-force vector top-10",
        &vector,
    )?;
    latency_metrics(
        &mut out,
        "query.hybrid.latency",
        "a hybrid (RRF) top-10 query",
        &hybrid,
    )?;

    // Deletes.
    let mut samples = Vec::with_capacity(p.deletes);
    for item in items.iter().take(p.deletes) {
        let t = Instant::now();
        let existed = ix.delete(item);
        samples.push(us(t.elapsed()));
        if !existed {
            return Err(BenchError::Failed(format!(
                "{} missing on delete",
                item.key
            )));
        }
    }
    if !samples.is_empty() {
        latency_metrics(
            &mut out,
            "delete.latency",
            "deleting one item from every index",
            &samples,
        )?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, run_benchmark};
    use crate::{Report, Status};

    #[test]
    fn smoke_run_produces_expected_metrics() {
        let report: Report = run_benchmark(
            &PerfBench,
            &RunConfig {
                scale: Scale::Smoke,
                seed: 7,
                ..RunConfig::default()
            },
        );
        assert_eq!(report.status, Status::Ok, "{:?}", report.message);
        for name in [
            "ingest.items_per_s",
            "ingest.mib_per_s",
            "ingest.ms_per_1k_items",
            "index.est_bytes_per_1k_items",
            "reingest_unchanged.items_per_s",
            "update.latency.p50_us",
            "update.latency.p95_us",
            "query.bm25.latency.p50_us",
            "query.vector.latency.p95_us",
            "query.hybrid.latency.p95_us",
            "delete.latency.p50_us",
        ] {
            let m = report
                .metric(name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert!(m.value > 0.0, "{name} = {}", m.value);
        }
        assert_eq!(report.config.params["docs"], 300);
        assert_eq!(report.config.params["backend"], "fake_index");
    }

    #[test]
    fn rejects_empty_params() {
        let mut ctx = RunContext::new(RunConfig::default());
        let p = PerfParams {
            docs: 0,
            ..PerfParams::for_scale(Scale::Smoke)
        };
        assert!(run_with(&mut ctx, &p).is_err());
    }
}
