//! W12 scale test: 100k synthetic items in a real encrypted `nugget_store::Store`.
//!
//! Ignored by default (minutes on a release build). Run it with:
//!
//! ```sh
//! CARGO_INCREMENTAL=0 cargo test --release -p nugget-bench --test scale -- --ignored --nocapture
//! ```
//!
//! Items are written directly with `Store::upsert_item_with_chunks` (one 384-dimensional chunk
//! each, deterministic hash embeddings), not through the ingest pipeline; see
//! `nugget_bench::suites::scale` for the phases. The report is printed and written to
//! `bench/results/scale-<timestamp>.json` (or `NUGGET_BENCH_RESULTS_DIR`).
//! `NUGGET_BENCH_SCALE_DOCS` overrides the item count, for a quicker trial.

use std::path::PathBuf;

use nugget_bench::suites::scale::{ScaleBench, StoreParams};
use nugget_bench::{Backend, RunConfig, Scale, Status, run_benchmark};

#[test]
#[ignore = "100k-item scale test, minutes on a release build: \
            cargo test --release -p nugget-bench --test scale -- --ignored --nocapture"]
fn store_at_100k_items() {
    let mut params = StoreParams::scale_100k();
    if let Some(docs) = std::env::var("NUGGET_BENCH_SCALE_DOCS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        params.docs = docs;
    }
    let report = run_benchmark(
        &ScaleBench { params },
        &RunConfig {
            scale: Scale::Full,
            seed: 42,
            backend: Backend::Real,
            ..RunConfig::default()
        },
    );
    if cfg!(debug_assertions) {
        eprintln!("note: debug build; timings are not representative (use --release)");
    }
    eprintln!(
        "== scale [{:?}] {} ms, docs={}",
        report.status, report.duration_ms, report.config.params["docs"]
    );
    for m in &report.metrics {
        eprintln!("  {:<48} {:>14.3} {}", m.name, m.value, m.unit);
    }
    let dir = std::env::var_os("NUGGET_BENCH_RESULTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("results"));
    match report.write_to_dir(&dir) {
        Ok(path) => eprintln!("  wrote {}", path.display()),
        Err(e) => eprintln!("  cannot write report to {}: {e}", dir.display()),
    }
    assert_eq!(report.status, Status::Ok, "{:?}", report.message);
    let v = |n: &str| report.metric(n).map(|m| m.value);
    assert_eq!(
        v("propagation.edit_rate"),
        Some(1.0),
        "every edit searchable"
    );
    assert!(
        v("retrieval.store.vector.recall_vs_exact@10").is_some_and(|r| r >= 0.9),
        "vector recall against exact search fell below 0.9"
    );
}
