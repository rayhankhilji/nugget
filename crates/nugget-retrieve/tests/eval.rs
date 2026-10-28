//! Retrieval quality on the labelled fixture corpus (`tests/fixtures/eval/`).
//!
//! Run with `--nocapture` to see the report table.

use std::collections::HashMap;
use std::sync::Arc;

use nugget_core::capability::{Capabilities, Tier};
use nugget_infer::{Provider, Router};
use nugget_retrieve::eval::{self, EvalMode, EvalSet, HashEmbedder};
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_store::Store;

const DIM: usize = 384;

async fn setup(with_embedder: bool) -> (Store, Retriever, EvalSet) {
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
    let retriever = Retriever::new(store.clone(), router, RetrieveConfig::default());
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
