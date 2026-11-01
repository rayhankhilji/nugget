//! `nugget-bench`: the benchmark harness behind every performance and quality claim Nugget makes.
//!
//! "If we can't measure it, we don't claim it" (see `docs/PLAN.md` §2). Each suite implements
//! [`Benchmark`], is registered in [`registry`], and produces a [`Report`] that conforms to the
//! stable JSON schema in `bench/schema/report.schema.json`.
//!
//! `memory` and `perf` default to [`index::FakeIndex`], an in-crate stand-in for `nugget-store`;
//! `--backend real` ([`real`]) runs the real stack in process. See `bench/README.md` for suites.

pub mod corpus;
pub mod index;
pub mod knowledge;
pub mod local;
pub mod real;
pub mod report;
pub mod runner;
pub mod stats;
pub mod suites;

pub use report::{Comparison, Direction, Metric, Report, SCHEMA_VERSION, Status, TargetCheck};
pub use runner::{
    Backend, BenchError, Benchmark, RunConfig, RunContext, Scale, find, registry, run_benchmark,
};
