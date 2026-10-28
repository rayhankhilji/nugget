//! Store-level speed and quality at scale: `perf --backend real`, and the 100k-item scale test.
//!
//! Unlike `memory --backend real`, nothing goes through the ingest pipeline or a watcher: the
//! synthetic corpus is written **directly** with [`Store::upsert_item_with_chunks`] (item, chunks,
//! full-text entries and vectors in one transaction per item) into an encrypted
//! [`nugget_store::Store`] in a temporary directory, with deterministic embeddings from
//! [`HashEmbedder`] computed outside the timed region. That isolates the store and retriever,
//! which is what W12 changes, and keeps a 100k-item run to minutes.
//!
//! Phases:
//! 1. **Write.** Every item with its chunks (`ingest.*`), then a re-upsert of unchanged content
//!    (`reingest_unchanged.*`), and the database size (`resources.*`).
//! 2. **Queries.** Known-item queries timed on each path: `Store::search_text`,
//!    `Store::search_vector` (query embedding excluded), `Store::search_hybrid`,
//!    `Retriever::search` and `Retriever::build_context` (query embedding included)
//!    (`query.*`), with known-item recall@10 and MRR@10 for each search (`retrieval.*`).
//! 3. **Vector recall against exact search.** For a sample of queries, `Store::search_vector`'s
//!    top 10 is compared with a brute-force cosine top 10 computed here over the generated
//!    vectors (`retrieval.store.vector.recall_vs_exact@10`). A hit counts if its exact score
//!    reaches the 10th exact score, so ties at the boundary never count as misses. Exact while
//!    the store searches by brute force; this is what an approximate index must keep high.
//! 4. **Edit freshness.** An item is rewritten with a new marker and the clock runs from the
//!    write until full-text search finds the new marker, vector search with the new embedding
//!    returns the item, the old marker no longer finds it, and `Retriever::search` finds it
//!    (`freshness.edit.*`, `update.latency.*`, `propagation.edit_rate`).
//! 5. **Deletes** (`delete.latency.*`) and peak RSS.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::ItemRef;
use nugget_infer::{Provider, Router};
use nugget_retrieve::eval::HashEmbedder;
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_store::{Hit, NewChunk, NewItem, Store, StoreKey, UpsertOutcome, content_hash};

use crate::corpus::{Corpus, CorpusConfig, Doc, Rng, marker};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext, Scale};
use crate::stats::summarize;
use crate::suites::perf::{PerfParams, latency_metrics, us};
use crate::suites::{SYNTHETIC_SOURCE, item_ref, secs};

/// Results per query, as in the other suites.
const DEPTH: usize = 10;

/// Workload of the store-level run.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreParams {
    pub docs: usize,
    pub words_per_doc: usize,
    /// Chunks each item is split into (each embedded).
    pub chunks_per_item: usize,
    pub dim: usize,
    /// Known-item queries timed on every path.
    pub queries: usize,
    pub query_terms: usize,
    pub query_noise: f64,
    /// Queries whose vector top 10 is checked against exact search.
    pub recall_sample: usize,
    /// Items rewritten in the freshness phase.
    pub edits: usize,
    pub deletes: usize,
    /// Token budget of the timed `Retriever::build_context` calls.
    pub context_budget_tokens: u32,
    /// An edit not searchable after this long counts as failed (and at this lag).
    pub edit_timeout_s: f64,
}

impl StoreParams {
    /// `perf --backend real`: the corpus size of the fake `perf` run at the same scale.
    pub fn for_scale(scale: Scale) -> Self {
        let p = PerfParams::for_scale(scale);
        Self {
            docs: p.docs,
            words_per_doc: p.words_per_doc,
            chunks_per_item: 2,
            dim: p.dim,
            queries: scale.pick(30, 200, 300, 500),
            query_terms: 4,
            query_noise: 0.25,
            recall_sample: scale.pick(20, 100, 100, 200),
            edits: scale.pick(10, 50, 100, 200),
            deletes: scale.pick(20, 200, 500, 1_000),
            context_budget_tokens: 2_000,
            edit_timeout_s: 30.0,
        }
    }

    /// The W12 scale test: 100k items, one 384-dimensional chunk each.
    pub fn scale_100k() -> Self {
        Self {
            docs: 100_000,
            words_per_doc: 120,
            chunks_per_item: 1,
            dim: 384,
            queries: 100,
            query_terms: 4,
            query_noise: 0.25,
            recall_sample: 50,
            edits: 30,
            deletes: 200,
            context_budget_tokens: 2_000,
            edit_timeout_s: 60.0,
        }
    }
}

/// The store-level run as a suite of its own (`scale`). Not registered: the 100k-item test in
/// `bench/tests/scale.rs` runs it; `perf --backend real` runs the same code at `--scale` sizes.
pub struct ScaleBench {
    pub params: StoreParams,
}

impl Benchmark for ScaleBench {
    fn name(&self) -> &'static str {
        "scale"
    }

    fn description(&self) -> &'static str {
        "Real-store write, query, exact-recall and edit-freshness at a fixed item count"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        run_with(ctx, &self.params)
    }
}

/// Entry point for `perf --backend real`.
pub fn run(ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
    run_with(ctx, &StoreParams::for_scale(ctx.scale()))
}

/// A router with `embedder` as the (probed) `Embedding` tier, as the real memory backend uses.
fn router(embedder: Arc<HashEmbedder>) -> Arc<Router> {
    let id = embedder.id().clone();
    let router = Router::new(
        vec![embedder as Arc<dyn Provider>],
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

/// Document id of a synthetic item key (`doc-<id>`).
fn doc_id(item: &ItemRef) -> Option<usize> {
    if item.source.as_str() != SYNTHETIC_SOURCE {
        return None;
    }
    item.key.as_str().strip_prefix("doc-")?.parse().ok()
}

/// A document's chunks: its body split into `n` runs of words, the version marker on the first.
fn chunk_texts(doc: &Doc, version: u32, n: usize) -> Vec<String> {
    let words: Vec<&str> = doc.body.split(' ').collect();
    let n = n.clamp(1, words.len().max(1));
    let per = words.len().div_ceil(n).max(1);
    let mut out: Vec<String> = words.chunks(per).map(|w| w.join(" ")).collect();
    if out.is_empty() {
        out.push(String::new());
    }
    out[0] = format!("{}\n{}", out[0], marker(doc.id, version));
    out
}

fn new_item(doc: &Doc, version: u32, chunks: &[String]) -> NewItem {
    let mut whole = doc.title.clone();
    for c in chunks {
        whole.push('\n');
        whole.push_str(c);
    }
    let r = item_ref(doc.id);
    NewItem::new(r.source.as_str(), r.key.as_str(), content_hash(&whole))
        .with_title(doc.title.clone())
        .with_version(format!("v{version}"))
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Rank (0-based) of `target`'s first hit in a list of chunk hits.
fn rank_of(hits: &[Hit], target: usize) -> Option<usize> {
    let mut seen = Vec::new();
    for h in hits {
        let Some(d) = doc_id(&h.item) else { continue };
        if d == target {
            return Some(seen.len());
        }
        if !seen.contains(&d) {
            seen.push(d);
        }
    }
    None
}

/// Accumulates known-item recall@10 and MRR@10 for one search path.
#[derive(Default)]
struct KnownItem {
    hits: usize,
    rr: f64,
    n: usize,
}

impl KnownItem {
    fn add(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(r) = rank.filter(|r| *r < DEPTH) {
            self.hits += 1;
            self.rr += 1.0 / (r + 1) as f64;
        }
    }

    fn metrics(&self, out: &mut Vec<Metric>, path: &str, what: &str) {
        let n = self.n.max(1) as f64;
        out.push(Metric::new(
            format!("retrieval.{path}.recall@10"),
            self.hits as f64 / n,
            "ratio",
            Direction::HigherIsBetter,
            format!("Known-item queries: share whose target item is in {what}'s top 10"),
        ));
        out.push(Metric::new(
            format!("retrieval.{path}.mrr@10"),
            self.rr / n,
            "ratio",
            Direction::HigherIsBetter,
            format!("Mean reciprocal rank of the target item in {what}'s top 10"),
        ));
    }
}

pub fn run_with(ctx: &mut RunContext, p: &StoreParams) -> Result<Vec<Metric>, BenchError> {
    if p.docs == 0 || p.queries == 0 || p.query_terms == 0 || p.chunks_per_item == 0 {
        return Err(BenchError::Failed(
            "the store run needs documents, chunks, queries and query terms".into(),
        ));
    }
    ctx.param("backend", "real_store");
    ctx.param(
        "stack",
        "nugget-store (SQLCipher, temp dir), written directly with upsert_item_with_chunks (no \
         ingest pipeline) + nugget-retrieve (RetrieveConfig::default())",
    );
    ctx.param(
        "embedder",
        format!(
            "hash-bow (nugget_retrieve::eval::HashEmbedder, in-process, deterministic), dim {}",
            p.dim
        ),
    );
    ctx.param("docs", p.docs);
    ctx.param("words_per_doc", p.words_per_doc);
    ctx.param("chunks_per_item", p.chunks_per_item);
    ctx.param("embedding_dim", p.dim);
    ctx.param("queries", p.queries);
    ctx.param("query_terms", p.query_terms);
    ctx.param("query_noise", p.query_noise);
    ctx.param("recall_sample", p.recall_sample);
    ctx.param("edits", p.edits);
    ctx.param("deletes", p.deletes);
    ctx.param("context_budget_tokens", p.context_budget_tokens);
    ctx.param("edit_timeout_s", p.edit_timeout_s);

    let seed = ctx.seed();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut out = rt.block_on(run_async(p, seed))?;
    if let Some(rss) = crate::real::peak_rss_bytes() {
        out.push(Metric::new(
            "resources.peak_rss_mib",
            rss as f64 / (1024.0 * 1024.0),
            "MiB",
            Direction::LowerIsBetter,
            "Peak resident set size of the whole benchmark process (store, retriever, the \
             generated corpus and its vectors held for exact search), from /proc/self/status VmHWM",
        ));
    }
    Ok(out)
}

async fn run_async(p: &StoreParams, seed: u64) -> Result<Vec<Metric>, BenchError> {
    let mut out = Vec::new();

    // Corpus, chunks and embeddings: outside every timed region.
    let corpus = Corpus::generate(CorpusConfig {
        docs: p.docs,
        words_per_doc: p.words_per_doc,
        seed,
        ..CorpusConfig::default()
    });
    let embedder = Arc::new(HashEmbedder::new(p.dim, &[]));
    let chunks: Vec<Vec<String>> = corpus
        .docs
        .iter()
        .map(|d| chunk_texts(d, 0, p.chunks_per_item))
        .collect();
    let t = Instant::now();
    // Row-major: chunk `ord` of doc `id` is row `offsets[id] + ord`.
    let mut offsets = Vec::with_capacity(p.docs);
    let mut vectors: Vec<f32> = Vec::new();
    let mut n_chunks = 0usize;
    for cs in &chunks {
        offsets.push(n_chunks);
        for c in cs {
            vectors.extend(embedder.embed_text(c));
            n_chunks += 1;
        }
    }
    out.push(Metric::new(
        "ingest.embed_ms_per_1k_chunks",
        secs(t.elapsed()) * 1e6 / n_chunks as f64,
        "ms",
        Direction::Info,
        "Deterministic hash embedder, per 1k chunks (outside the timed write; for reference)",
    ));
    let row = |i: usize| &vectors[i * p.dim..(i + 1) * p.dim];

    // Phase 1: write.
    let home = tempfile::Builder::new()
        .prefix("nugget-bench-store-")
        .tempdir()?;
    let db_path = home.path().join("nugget.db");
    let mut key = [0u8; 32];
    let mut krng = Rng::new(seed ^ 0x6B65_7901);
    for c in key.chunks_mut(8) {
        c.copy_from_slice(&krng.next_u64().to_le_bytes());
    }
    let store = Store::open(&db_path, &StoreKey::from_bytes(key))?;
    if !store.is_encrypted() {
        return Err(BenchError::Failed("store is not encrypted".into()));
    }
    let items: Vec<NewItem> = corpus
        .docs
        .iter()
        .zip(&chunks)
        .map(|(d, cs)| new_item(d, 0, cs))
        .collect();
    let new_chunks = |id: usize| -> Vec<NewChunk> {
        chunks[id]
            .iter()
            .enumerate()
            .map(|(ord, c)| NewChunk::embedded(c.clone(), row(offsets[id] + ord).to_vec()))
            .collect()
    };
    let mut write_s = 0.0;
    for (id, item) in items.iter().enumerate() {
        let cs = new_chunks(id);
        let t = Instant::now();
        let outcome = store.upsert_item_with_chunks(item, &cs)?;
        write_s += t.elapsed().as_secs_f64();
        if outcome != UpsertOutcome::Inserted {
            return Err(BenchError::Failed(format!(
                "{} was {outcome:?}, not inserted",
                item.item.key
            )));
        }
    }
    let write_s = write_s.max(1e-9);
    out.extend([
        Metric::new(
            "ingest.items_per_s",
            p.docs as f64 / write_s,
            "items/s",
            Direction::HigherIsBetter,
            "Store write throughput: upsert_item_with_chunks per item (item, chunks, full-text and \
             vectors in one transaction; embeddings precomputed)",
        ),
        Metric::new(
            "ingest.chunks_per_s",
            n_chunks as f64 / write_s,
            "chunks/s",
            Direction::HigherIsBetter,
            "Store write throughput in embedded chunks",
        ),
        Metric::new(
            "ingest.ms_per_1k_items",
            write_s * 1e6 / p.docs as f64,
            "ms",
            Direction::LowerIsBetter,
            "Store write time per 1k items",
        ),
    ]);
    let t = Instant::now();
    for (id, item) in items.iter().enumerate() {
        if store.upsert_item_with_chunks(item, &new_chunks(id))? != UpsertOutcome::Unchanged {
            return Err(BenchError::Failed(format!(
                "{} was not deduplicated",
                item.item.key
            )));
        }
    }
    out.push(Metric::new(
        "reingest_unchanged.items_per_s",
        p.docs as f64 / secs(t.elapsed()),
        "items/s",
        Direction::HigherIsBetter,
        "Re-upsert of unchanged items (content-hash dedup; includes cloning the chunks)",
    ));
    out.push(Metric::new(
        "resources.db_bytes_per_1k_items",
        crate::real::db_bytes(&db_path) as f64 * 1000.0 / p.docs as f64,
        "bytes",
        Direction::LowerIsBetter,
        "Encrypted database on disk (main file + WAL + SHM) after the write, per 1k items",
    ));

    // Phase 2: queries on every path.
    let retriever = Retriever::new(
        store.clone(),
        Some(router(embedder.clone())),
        RetrieveConfig::default(),
    );
    let queries = corpus.queries(p.queries, p.query_terms, p.query_noise, seed);
    let mut lat: [Vec<f64>; 5] = Default::default();
    let mut ki: [KnownItem; 4] = Default::default();
    let mut query_vecs = Vec::with_capacity(queries.len());
    let mut fallbacks = 0usize;
    for q in &queries {
        let e = embedder.embed_text(&q.text);

        let t = Instant::now();
        let text = store.search_text(&q.text, DEPTH)?;
        lat[0].push(us(t.elapsed()));
        ki[0].add(rank_of(&text, q.relevant));

        let t = Instant::now();
        let vector = store.search_vector(&e, DEPTH)?;
        lat[1].push(us(t.elapsed()));
        ki[1].add(rank_of(&vector, q.relevant));

        let t = Instant::now();
        let hybrid = store.search_hybrid(&q.text, Some(&e), DEPTH)?;
        lat[2].push(us(t.elapsed()));
        ki[2].add(rank_of(&hybrid, q.relevant));

        let t = Instant::now();
        let res = retriever.search(&q.text, DEPTH).await?;
        lat[3].push(us(t.elapsed()));
        if res.mode.is_text_only() {
            fallbacks += 1;
        }
        ki[3].add(
            res.items
                .iter()
                .position(|i| doc_id(&i.item) == Some(q.relevant)),
        );

        let t = Instant::now();
        std::hint::black_box(
            retriever
                .build_context(&q.text, p.context_budget_tokens)
                .await?,
        );
        lat[4].push(us(t.elapsed()));
        query_vecs.push(e);
    }
    for (samples, (prefix, what)) in lat.iter().zip([
        ("query.store.text.latency", "Store::search_text top 10"),
        (
            "query.store.vector.latency",
            "Store::search_vector top 10 (query embedding excluded)",
        ),
        (
            "query.store.hybrid.latency",
            "Store::search_hybrid top 10 (query embedding excluded)",
        ),
        (
            "query.retriever.search.latency",
            "Retriever::search top 10 (hybrid, grouped, MMR; query embedding included)",
        ),
        (
            "query.retriever.build_context.latency",
            "Retriever::build_context (query embedding included)",
        ),
    ]) {
        latency_metrics(&mut out, prefix, what, samples)?;
    }
    for (k, (path, what)) in ki.iter().zip([
        ("store.text", "Store::search_text"),
        ("store.vector", "Store::search_vector"),
        ("store.hybrid", "Store::search_hybrid"),
        ("retriever", "Retriever::search"),
    ]) {
        k.metrics(&mut out, path, what);
    }
    out.push(Metric::new(
        "retrieval.retriever.text_only_fallbacks",
        fallbacks as f64,
        "count",
        Direction::Info,
        "Queries the retriever answered text-only (embedding unavailable or failed)",
    ));

    // Phase 3: vector recall against exact search.
    let sample = p.recall_sample.min(query_vecs.len());
    let mut recall = 0.0;
    let mut exact_us = Vec::with_capacity(sample);
    let mut scores = vec![0.0f32; n_chunks];
    for e in query_vecs.iter().take(sample) {
        let t = Instant::now();
        for (i, s) in scores.iter_mut().enumerate() {
            *s = dot(e, row(i));
        }
        let k = DEPTH.min(n_chunks);
        let mut top = scores.clone();
        top.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
        let kth = top[k - 1];
        exact_us.push(us(t.elapsed()));
        let hits = store.search_vector(e, DEPTH)?;
        let ids: Vec<_> = hits.iter().map(|h| h.chunk_id).collect();
        let found = store.get_chunks(&ids)?;
        let good = found
            .iter()
            .filter_map(|c| Some(offsets[doc_id(&c.item)?] + c.ord as usize))
            .filter(|&i| dot(e, row(i)) >= kth - 1e-5)
            .count();
        recall += good.min(k) as f64 / k as f64;
    }
    if sample > 0 {
        out.push(Metric::new(
            "retrieval.store.vector.recall_vs_exact@10",
            recall / sample as f64,
            "ratio",
            Direction::HigherIsBetter,
            "Share of Store::search_vector's top 10 that belongs to the exact (brute-force cosine) \
             top 10, ties at the boundary counted as correct",
        ));
        latency_metrics(
            &mut out,
            "query.exact.vector.latency",
            "an exact brute-force cosine top 10 in Rust over the in-memory vectors (reference)",
            &exact_us,
        )?;
    }
    drop(scores);

    // Phase 4: edit freshness.
    let mut rng = Rng::new(seed ^ 0xED17_0000);
    let mut versions: HashMap<usize, u32> = HashMap::new();
    let timeout = Duration::from_secs_f64(p.edit_timeout_s);
    let mut write_us = Vec::with_capacity(p.edits);
    let mut fresh_ms = Vec::with_capacity(p.edits);
    let mut ok = 0usize;
    for _ in 0..p.edits {
        let id = rng.below(p.docs);
        let v = versions.entry(id).or_insert(0);
        *v += 1;
        let version = *v;
        let doc = corpus.make_doc(&mut rng, id, corpus.docs[id].topic);
        let texts = chunk_texts(&doc, version, p.chunks_per_item);
        let embs: Vec<Vec<f32>> = texts.iter().map(|c| embedder.embed_text(c)).collect();
        let cs: Vec<NewChunk> = texts
            .iter()
            .zip(&embs)
            .map(|(c, e)| NewChunk::embedded(c.clone(), e.clone()))
            .collect();
        let item = new_item(&doc, version, &texts);
        let (new_marker, old_marker) = (marker(id, version), marker(id, version - 1));

        let t0 = Instant::now();
        let outcome = store.upsert_item_with_chunks(&item, &cs)?;
        write_us.push(us(t0.elapsed()));
        if outcome != UpsertOutcome::Updated {
            return Err(BenchError::Failed(format!(
                "edit of doc-{id} returned {outcome:?}"
            )));
        }
        let done = loop {
            let text_new = rank_of(&store.search_text(&new_marker, DEPTH)?, id).is_some();
            let text_old = rank_of(&store.search_text(&old_marker, DEPTH)?, id).is_some();
            let vec_new = rank_of(&store.search_vector(&embs[0], DEPTH)?, id).is_some();
            let retr = retriever
                .search(&new_marker, DEPTH)
                .await?
                .items
                .iter()
                .any(|i| doc_id(&i.item) == Some(id));
            if text_new && !text_old && vec_new && retr {
                break true;
            }
            if t0.elapsed() >= timeout {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        if done {
            ok += 1;
            fresh_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        } else {
            fresh_ms.push(timeout.as_secs_f64() * 1e3);
        }
    }
    if p.edits > 0 {
        latency_metrics(
            &mut out,
            "update.latency",
            "rewriting one item with its chunks and vectors (upsert_item_with_chunks)",
            &write_us,
        )?;
        let s = summarize(&fresh_ms)
            .ok_or_else(|| BenchError::Failed("no freshness samples".into()))?;
        for (name, v, what) in [
            ("p50_ms", s.p50, "Median"),
            ("p95_ms", s.p95, "95th-percentile"),
            ("max_ms", s.max, "Worst"),
        ] {
            out.push(Metric::new(
                format!("freshness.edit.{name}"),
                v,
                "ms",
                Direction::LowerIsBetter,
                format!(
                    "{what} time from starting an item rewrite to it being fully searchable: new \
                     marker found by full-text and Retriever::search, new embedding found by \
                     vector search, old marker gone (write included; timeouts count at the \
                     timeout)"
                ),
            ));
        }
        out.push(
            Metric::new(
                "propagation.edit_rate",
                ok as f64 / p.edits as f64,
                "ratio",
                Direction::HigherIsBetter,
                "Share of item rewrites fully searchable within the timeout",
            )
            .with_target(1.0),
        );
    }

    // Phase 5: deletes.
    let mut del_us = Vec::with_capacity(p.deletes);
    for doc in corpus.docs.iter().take(p.deletes) {
        let r = item_ref(doc.id);
        let t = Instant::now();
        let existed = store.delete_item(&r)?;
        del_us.push(us(t.elapsed()));
        if !existed {
            return Err(BenchError::Failed(format!("{} missing on delete", r.key)));
        }
    }
    if !del_us.is_empty() {
        latency_metrics(
            &mut out,
            "delete.latency",
            "deleting one item with its chunks, full-text entries and vectors",
            &del_us,
        )?;
    }
    out.push(Metric::new(
        "resources.db_bytes",
        crate::real::db_bytes(&db_path) as f64,
        "bytes",
        Direction::Info,
        "Encrypted database on disk (main file + WAL + SHM) at the end of the run",
    ));
    drop(retriever);
    drop(store);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Backend, RunConfig, run_benchmark};
    use crate::{Report, Status};

    fn tiny() -> StoreParams {
        StoreParams {
            docs: 150,
            queries: 12,
            recall_sample: 8,
            edits: 4,
            deletes: 5,
            ..StoreParams::for_scale(Scale::Smoke)
        }
    }

    #[test]
    fn chunking_splits_bodies_and_marks_the_first_chunk() {
        let corpus = Corpus::generate(CorpusConfig {
            docs: 2,
            words_per_doc: 10,
            seed: 1,
            ..CorpusConfig::default()
        });
        let cs = chunk_texts(&corpus.docs[1], 3, 2);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].ends_with("mk1v3"));
        assert_eq!(
            cs.iter()
                .map(|c| c.split_whitespace().count())
                .sum::<usize>(),
            11
        );
        assert_eq!(doc_id(&item_ref(42)), Some(42));
    }

    #[test]
    fn tiny_store_run_produces_expected_metrics() {
        let report: Report = run_benchmark(
            &ScaleBench { params: tiny() },
            &RunConfig {
                scale: Scale::Smoke,
                seed: 7,
                backend: Backend::Real,
                ..RunConfig::default()
            },
        );
        assert_eq!(report.status, Status::Ok, "{:?}", report.message);
        for name in [
            "ingest.items_per_s",
            "ingest.chunks_per_s",
            "reingest_unchanged.items_per_s",
            "resources.db_bytes_per_1k_items",
            "query.store.text.latency.p50_us",
            "query.store.vector.latency.p95_us",
            "query.store.hybrid.latency.p95_us",
            "query.retriever.search.latency.p95_us",
            "query.retriever.build_context.latency.p95_us",
            "query.exact.vector.latency.p50_us",
            "update.latency.p95_us",
            "freshness.edit.p95_ms",
            "delete.latency.p50_us",
        ] {
            let m = report
                .metric(name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert!(m.value > 0.0, "{name} = {}", m.value);
        }
        let v = |n: &str| report.metric(n).map(|m| m.value).unwrap_or(-1.0);
        // Brute-force vector search at this size: exactly the exact top 10.
        assert_eq!(v("retrieval.store.vector.recall_vs_exact@10"), 1.0);
        assert_eq!(v("propagation.edit_rate"), 1.0);
        assert!(v("retrieval.store.text.recall@10") > 0.5);
        assert!(v("retrieval.retriever.recall@10") > 0.5);
        assert_eq!(v("retrieval.retriever.text_only_fallbacks"), 0.0);
        assert_eq!(report.config.params["backend"], "real_store");
    }
}
