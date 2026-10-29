//! Retrieval quality on the labelled fixture corpus (`tests/fixtures/eval/`).
//!
//! Run with `--nocapture` to see the report table.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nugget_core::capability::{Capabilities, Tier};
use nugget_infer::{Provider, Router};
use nugget_retrieve::eval::{self, EvalMode, EvalSet, HashEmbedder};
use nugget_retrieve::{RetrieveConfig, Retriever, SearchOptions, Strategy};
use nugget_store::Store;

const DIM: usize = 384;

async fn setup(with_embedder: bool) -> (Store, Retriever, EvalSet) {
    setup_with(with_embedder, RetrieveConfig::default()).await
}

async fn setup_with(with_embedder: bool, config: RetrieveConfig) -> (Store, Retriever, EvalSet) {
    let set = EvalSet::builtin().unwrap();
    let store = Store::open_in_memory().unwrap();
    let emb = Arc::new(HashEmbedder::new(DIM, &set.synonyms));
    let router = with_embedder.then(|| {
        let r = Router::new(
            vec![emb.clone() as Arc<dyn Provider>],
            HashMap::from([(Tier::Embedding, vec![emb.id().clone()])]),
        );
        r.set_capabilities(
            emb.id(),
            Capabilities {
                embeddings: true,
                ..Default::default()
            },
        );
        Arc::new(r)
    });
    eval::index(
        &store,
        &set,
        with_embedder.then_some(emb.as_ref() as &dyn Provider),
    )
    .await
    .unwrap();
    let retriever = Retriever::new(store.clone(), router, config);
    (store, retriever, set)
}

#[tokio::test]
async fn hybrid_is_at_least_as_good_as_text_only() {
    let (store, retriever, set) = setup(true).await;
    let report = eval::run(&store, &retriever, &set).await.unwrap();
    println!("{report}");

    assert_eq!(report.docs, set.docs.len());
    assert_eq!(report.queries, set.queries.len());
    for m in &report.modes {
        assert!(m.ran, "{:?} did not run: {:?}", m.mode, m.note);
        assert_eq!(m.note, None, "{:?}: {:?}", m.mode, m.note);
        assert_eq!(m.queries.len(), set.queries.len());
        let x = m.metrics;
        for v in [
            x.recall_at_1,
            x.recall_at_5,
            x.recall_at_10,
            x.mrr_at_10,
            x.ndcg_at_10,
        ] {
            assert!((0.0..=1.0).contains(&v), "{:?}: {v}", m.mode);
        }
        assert!(x.recall_at_1 <= x.recall_at_5 && x.recall_at_5 <= x.recall_at_10);
    }

    let get = |m| report.mode(m).unwrap().metrics;
    let text = get(EvalMode::TextOnly);
    let hybrid = get(EvalMode::Hybrid);
    let hybrid_mmr = get(EvalMode::HybridMmr);
    // The M1 exit gate, on fixtures: fusion must not lose what full-text finds.
    assert!(
        hybrid.recall_at_10 >= text.recall_at_10,
        "hybrid recall@10 {} < text-only {}",
        hybrid.recall_at_10,
        text.recall_at_10
    );
    assert!(
        hybrid_mmr.recall_at_10 >= text.recall_at_10,
        "hybrid+MMR recall@10 {} < text-only {}",
        hybrid_mmr.recall_at_10,
        text.recall_at_10
    );
    // Floors so a regression in any stage shows up (measured with title search in the store:
    // text-only R@10 0.881, hybrid R@10 0.976, hybrid MRR@10 0.780).
    assert!(
        text.recall_at_10 >= 0.85,
        "text-only recall@10 {}",
        text.recall_at_10
    );
    assert!(
        hybrid.recall_at_10 >= 0.9,
        "hybrid recall@10 {}",
        hybrid.recall_at_10
    );
    assert!(
        hybrid.mrr_at_10 >= 0.75,
        "hybrid MRR@10 {}",
        hybrid.mrr_at_10
    );

    let pairs = report.metric_pairs();
    assert_eq!(pairs.len(), 4 * 5);
    assert!(pairs.iter().any(|(k, _)| k == "retrieval.hybrid.recall@10"));

    // Deterministic: a second run gives the same report.
    assert_eq!(eval::run(&store, &retriever, &set).await.unwrap(), report);
}

#[tokio::test]
async fn without_an_embedder_vector_modes_do_not_run() {
    let (store, retriever, set) = setup(false).await;
    let report = eval::run(&store, &retriever, &set).await.unwrap();
    println!("{report}");
    let vector = report.mode(EvalMode::VectorOnly).unwrap();
    assert!(!vector.ran);
    let hybrid = report.mode(EvalMode::Hybrid).unwrap();
    assert!(hybrid.ran);
    assert!(hybrid.note.as_deref().unwrap().contains("text-only"));
    assert_eq!(
        hybrid.metrics,
        report.mode(EvalMode::TextOnly).unwrap().metrics
    );
}

#[tokio::test]
async fn run_requires_an_indexed_set() {
    let set = EvalSet::builtin().unwrap();
    let store = Store::open_in_memory().unwrap();
    let retriever = Retriever::new(store.clone(), None, RetrieveConfig::default());
    assert!(eval::run(&store, &retriever, &set).await.is_err());
}

#[test]
fn fixture_directory_loads() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval");
    let set = EvalSet::load_dir(&dir).unwrap();
    assert_eq!(set.docs, EvalSet::builtin().unwrap().docs);
}

/// Intra-list similarity: mean pairwise token-set Jaccard of the top `k` documents' texts
/// (lower is more diverse).
fn intra_list_similarity(set: &EvalSet, ranked: &[String], k: usize) -> f64 {
    use nugget_retrieve::rank::{jaccard, token_set};
    let texts: HashMap<&str, &str> = set
        .docs
        .iter()
        .map(|d| (d.id.as_str(), d.text.as_str()))
        .collect();
    let sets: Vec<_> = ranked
        .iter()
        .take(k)
        .filter_map(|id| texts.get(id.as_str()))
        .map(|t| token_set(t))
        .collect();
    let mut sum = 0.0;
    let mut n = 0usize;
    for i in 0..sets.len() {
        for j in i + 1..sets.len() {
            sum += f64::from(jaccard(&sets[i], &sets[j]));
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// Mean (R@1, R@5, R@10, MRR@10, nDCG@10, ILS@5) over the set's queries, MMR on.
async fn sweep_point(retriever: &Retriever, set: &EvalSet, strategy: Strategy) -> [f64; 6] {
    let mut acc = [0.0f64; 6];
    for q in &set.queries {
        let opts = SearchOptions {
            strategy,
            mmr: Some(true),
        };
        let res = retriever
            .search_with(&q.query, eval::EVAL_DEPTH, opts)
            .await
            .unwrap();
        let ranked: Vec<String> = res
            .items
            .iter()
            .map(|r| r.item.key.as_str().to_owned())
            .collect();
        let rel: HashSet<&str> = q.relevant.iter().map(String::as_str).collect();
        acc[0] += eval::recall_at(&ranked, &rel, 1);
        acc[1] += eval::recall_at(&ranked, &rel, 5);
        acc[2] += eval::recall_at(&ranked, &rel, 10);
        acc[3] += eval::mrr_at(&ranked, &rel, 10);
        acc[4] += eval::ndcg_at(&ranked, &rel, 10);
        acc[5] += intra_list_similarity(set, &ranked, 5);
    }
    acc.map(|x| x / set.queries.len() as f64)
}

/// Sweep `mmr_lambda` × `mmr_pool_factor` on the fixture set, with vector MMR (hybrid) and
/// lexical MMR (text-only), and print a table; `RetrieveConfig`'s MMR defaults come from it.
/// `cargo test -p nugget-retrieve --test eval -- --ignored --nocapture mmr_sweep`
#[tokio::test]
#[ignore = "parameter sweep; run by hand when tuning MMR"]
async fn mmr_sweep() {
    let now = time::OffsetDateTime::parse(
        "2026-10-28T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    for (label, strategy) in [
        ("hybrid (vector MMR)", Strategy::Auto),
        ("text-only (lexical MMR)", Strategy::TextOnly),
    ] {
        println!("\n{label}");
        println!(
            "{:>6} {:>4} {:>6} {:>6} {:>6} {:>7} {:>8} {:>6}",
            "lambda", "pool", "R@1", "R@5", "R@10", "MRR@10", "nDCG@10", "ILS@5"
        );
        for lambda in [0.5f32, 0.6, 0.7, 0.8, 0.9, 1.0] {
            for pool in 2..=5usize {
                let cfg = RetrieveConfig {
                    mmr_lambda: lambda,
                    mmr_pool_factor: pool,
                    ..RetrieveConfig::default()
                };
                let (_store, r, set) = setup_with(true, cfg).await;
                let r = r.with_reference_time(now);
                let m = sweep_point(&r, &set, strategy).await;
                println!(
                    "{lambda:>6.1} {pool:>4} {:>6.3} {:>6.3} {:>6.3} {:>7.3} {:>8.3} {:>6.3}",
                    m[0], m[1], m[2], m[3], m[4], m[5]
                );
            }
        }
    }
}
