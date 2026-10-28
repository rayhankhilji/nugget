//! `nugget-bench`: the benchmark harness behind every performance and quality claim Nugget makes.
//!
//! "If we can't measure it, we don't claim it" (see `docs/PLAN.md` §2). Each suite implements
//! [`Benchmark`], is registered in [`registry`], and produces a [`Report`] that conforms to the
//! stable JSON schema in `bench/schema/report.schema.json`.
//!
//! In M0 the `memory` and `perf` suites run real micro-benchmarks against [`index::FakeIndex`], an
//! in-crate stand-in for `nugget-store` with the same shape (content-hash dedup, delete, BM25,
//! vector kNN and hybrid fusion). They will be pointed at the real store and ingest pipeline in M1.
//! `draft` and `cua` are registered but not yet implemented; their methodology is in
//! `bench/README.md`.

pub mod corpus;
pub mod index;
pub mod report;
pub mod runner;
pub mod stats;
pub mod suites;

pub use report::{Direction, Metric, Report, SCHEMA_VERSION, Status};
pub use runner::{
    BenchError, Benchmark, RunConfig, RunContext, Scale, find, registry, run_benchmark,
};
