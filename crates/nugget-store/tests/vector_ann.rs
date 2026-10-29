//! Approximate vector search (binary-quantized side index + exact rescoring) against the exact
//! scan.
//!
//! The recall/latency measurement is ignored by default (it writes 100k vectors); run it with
//!
//! ```text
//! cargo test --release -p nugget-store --test vector_ann -- --ignored --nocapture
//! ```
//!
//! `NUGGET_ANN_VECTORS=<n>` changes the corpus size; `NUGGET_ANN_MEMORY=1` uses an unencrypted
//! in-memory store instead of an encrypted file.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nugget_core::ids::ItemRef;
use nugget_store::{ANN_MIN_VECTORS, NewChunk, NewItem, Store, StoreKey};

const DIM: usize = 384;

/// Small deterministic PRNG (xorshift64*), so runs are comparable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn unit_f32(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Standard normal sample (Box–Muller).
    fn gauss(&mut self) -> f32 {
        let u1 = self.unit_f32().max(f32::MIN_POSITIVE);
        let u2 = self.unit_f32();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter_mut().for_each(|x| *x /= n);
    v
}

/// Clustered embeddings, closer to real text embeddings than isotropic noise: `clusters` random
/// topic centroids, each vector a centroid plus gaussian noise (`noise` per dimension, relative
/// to the centroid's unit-variance coordinates), normalised.
struct Corpus {
    rng: Rng,
    centroids: Vec<Vec<f32>>,
    noise: f32,
}

impl Corpus {
    fn new(seed: u64, clusters: usize, noise: f32) -> Self {
        let mut rng = Rng(seed);
        let centroids = (0..clusters)
            .map(|_| (0..DIM).map(|_| rng.gauss()).collect())
            .collect();
        Self {
            rng,
            centroids,
            noise,
        }
    }

    fn sample(&mut self) -> Vec<f32> {
        let c = (self.rng.next() % self.centroids.len() as u64) as usize;
        let v = (0..DIM)
            .map(|i| self.centroids[c][i] + self.noise * self.rng.gauss())
            .collect();
        normalize(v)
    }
}

/// Write `n` vectors as items of `per_item` chunks each.
fn fill(s: &Store, corpus: &mut Corpus, n: usize, per_item: usize) {
    for item in 0..n.div_ceil(per_item) {
        let chunks: Vec<NewChunk> = (0..per_item.min(n - item * per_item))
            .map(|j| NewChunk::embedded(format!("item {item} chunk {j}"), corpus.sample()))
            .collect();
        let new = NewItem::new("bench", format!("i{item}"), format!("h{item}"));
        s.upsert_item_with_chunks(&new, &chunks).unwrap();
    }
}

fn ids(hits: &[nugget_store::Hit]) -> Vec<i64> {
    hits.iter().map(|h| h.chunk_id).collect()
}

#[test]
fn approximate_search_finds_the_exact_top_hit_and_tracks_updates() {
    let s = Store::open_in_memory().unwrap();
    let mut corpus = Corpus::new(0x5eed, 40, 0.6);
    fill(&s, &mut corpus, 3_000, 5);

    let mut queries = Corpus::new(0xfeed, 1, 0.0);
    queries.centroids = corpus.centroids.clone();
    queries.noise = 0.6;
    let mut overlap = 0;
    for _ in 0..20 {
        let q = queries.sample();
        let exact = s.search_vector_exact(&q, 10).unwrap();
        let approx = s.search_vector_approx(&q, 10).unwrap();
        assert_eq!(approx.len(), 10);
        assert_eq!(approx[0].chunk_id, exact[0].chunk_id);
        // Scores are exact cosine similarities on both paths.
        assert_eq!(approx[0].score, exact[0].score);
        assert!(approx.windows(2).all(|w| w[0].score >= w[1].score));
        let e = ids(&exact);
        overlap += ids(&approx).iter().filter(|id| e.contains(id)).count();
    }
    assert!(overlap >= 180, "recall@10 {}", overlap as f64 / 200.0);

    // Updating an item's chunks: its old vectors disappear from both indexes, the new one is
    // found.
    let target = queries.sample();
    let top = s.search_vector_approx(&target, 1).unwrap().remove(0);
    let item = NewItem::new("bench", top.item.key.as_str(), "changed");
    let replacement = normalize(target.iter().map(|x| -x).collect());
    s.upsert_item_with_chunks(
        &item,
        &[NewChunk::embedded("replaced", replacement.clone())],
    )
    .unwrap();
    let after = s.search_vector_approx(&target, 50).unwrap();
    assert!(after.iter().all(|h| h.chunk_id != top.chunk_id));
    let found = s.search_vector_approx(&replacement, 1).unwrap();
    assert_eq!(found[0].snippet, "replaced");

    // Deleting it removes it from both.
    let iref: ItemRef = top.item.clone();
    assert!(s.delete_item(&iref).unwrap());
    let gone = s.search_vector_approx(&replacement, 10).unwrap();
    assert!(gone.iter().all(|h| h.item != iref));
    assert_eq!(gone.len(), 10);

    // Below the threshold the default path is exact (and, far below it, drops the index).
    let q = queries.sample();
    assert_eq!(
        ids(&s.search_vector(&q, 10).unwrap()),
        ids(&s.search_vector_exact(&q, 10).unwrap())
    );
    assert!(!s.ann_index_ready().unwrap());
}

#[test]
#[ignore = "slow: writes 100k vectors; run in release with --ignored --nocapture"]
fn ann_recall_and_latency_at_scale() {
    let n: usize = std::env::var("NUGGET_ANN_VECTORS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    assert!(n >= ANN_MIN_VECTORS, "corpus below the ANN threshold");
    let dir = tempfile::tempdir().unwrap();
    let s = if std::env::var_os("NUGGET_ANN_MEMORY").is_some() {
        Store::open_in_memory().unwrap()
    } else {
        Store::open(dir.path().join("ann.db"), &StoreKey::from_bytes([9u8; 32])).unwrap()
    };
    // ~1 cluster per 100 chunks; noise 0.75 keeps within-cluster cosine around 0.6, roughly what
    // sentence embeddings of related passages show.
    let clusters = (n / 100).max(10);
    let noise = 0.75;
    let mut corpus = Corpus::new(42, clusters, noise);
    let t = Instant::now();
    fill(&s, &mut corpus, n, 10);
    println!("wrote {n} vectors (dim {DIM}) in {:.1?}", t.elapsed());

    let mut queries = Corpus::new(7, 1, 0.0);
    queries.centroids = corpus.centroids.clone();
    queries.noise = noise;
    let queries: Vec<Vec<f32>> = (0..100).map(|_| queries.sample()).collect();

    // The first search past the thresholds starts building the approximate index; meanwhile a
    // writer keeps upserting, and its latencies show how long the build holds the writer.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (s, stop) = (s.clone(), stop.clone());
        let mut corpus = Corpus::new(99, 10, noise);
        std::thread::spawn(move || {
            let mut latencies = Vec::new();
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let item = NewItem::new("w", format!("w{}", i % 100), format!("wh{i}"));
                let chunk = NewChunk::embedded("w", corpus.sample());
                let t = Instant::now();
                s.upsert_item_with_chunks(&item, &[chunk]).unwrap();
                latencies.push(t.elapsed());
                i += 1;
            }
            latencies
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    s.search_vector(&queries[0], 10).unwrap();
    println!("first search_vector: {:.1?}", t.elapsed());
    let deadline = Instant::now() + Duration::from_secs(600);
    while !s.ann_index_ready().unwrap() {
        assert!(Instant::now() < deadline, "index build did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("index complete {:.1?} after the first search", t.elapsed());
    stop.store(true, Ordering::Relaxed);
    let mut latencies = writer.join().unwrap();
    latencies.sort();
    println!(
        "upserts during the build: {}, p50 {:.1?}, p99 {:.1?}, max {:.1?}",
        latencies.len(),
        latencies[latencies.len() / 2],
        latencies[latencies.len() * 99 / 100],
        latencies.last().unwrap()
    );

    for k in [10usize, 50] {
        // Separate loops: the exact scan reads every vector and would evict the approximate
        // path's pages from the cache if interleaved with it.
        let t = Instant::now();
        let exact: Vec<Vec<i64>> = queries
            .iter()
            .map(|q| ids(&s.search_vector_exact(q, k).unwrap()))
            .collect();
        let exact_time = t.elapsed();
        let t = Instant::now();
        let approx: Vec<Vec<i64>> = queries
            .iter()
            .map(|q| ids(&s.search_vector(q, k).unwrap()))
            .collect();
        let approx_time = t.elapsed();
        let mut found = 0usize;
        let mut top1 = 0usize;
        for (e, a) in exact.iter().zip(&approx) {
            found += a.iter().filter(|id| e.contains(id)).count();
            top1 += usize::from(a[0] == e[0]);
        }
        let recall = found as f64 / (k * queries.len()) as f64;
        println!(
            "k={k}: recall@{k} {recall:.3}, top-1 agreement {top1}/{}, exact {:.2?}/query, \
             approximate {:.2?}/query",
            queries.len(),
            exact_time / queries.len() as u32,
            approx_time / queries.len() as u32,
        );
        if k == 10 {
            assert!(recall >= 0.95, "recall@10 {recall:.3}");
        }
    }
}
