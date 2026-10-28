//! `nugget-bench`: the benchmark harness behind every performance and quality claim Nugget makes.
//!
//! "If we can't measure it, we don't claim it" (see `docs/PLAN.md` §2). Each suite implements
//! [`Benchmark`], is registered in [`registry`], and produces a [`Report`] that conforms to the
//! stable JSON schema in `bench/schema/report.schema.json`.
//!
//! The `memory` and `perf` suites run by default against [`index::FakeIndex`], an in-crate
//! stand-in for `nugget-store` with the same shape (content-hash dedup, delete, BM25, vector kNN
//! and hybrid fusion). `memory --backend real` ([`real`]) runs the real stack in process: an
//! encrypted `nugget-store`, `nugget-ingest` with the `files` connector watching a temporary
//! folder, and `nugget-retrieve`.
//! `draft` and `cua` are registered but not yet implemented; their methodology is in
//! `bench/README.md`.

pub mod corpus;
pub mod index;
pub mod real;
pub mod report;
pub mod runner;
pub mod stats;
pub mod suites;

pub use report::{Comparison, Direction, Metric, Report, SCHEMA_VERSION, Status, TargetCheck};
pub use runner::{
    Backend, BenchError, Benchmark, RunConfig, RunContext, Scale, find, registry, run_benchmark,
};
