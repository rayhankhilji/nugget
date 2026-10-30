//! The `Benchmark` trait, the suite registry and the runner that turns a run into a [`Report`].

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use time::OffsetDateTime;

use crate::report::{
    Environment, Metric, Report, ReportConfig, SCHEMA_VERSION, Status, TargetCheck,
    detect_git_commit,
};
use crate::suites;

/// How much work a run does. `smoke` exists for CI and tests; publishable numbers use
/// `standard` or `full` on a release build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum Scale {
    Smoke,
    Quick,
    #[default]
    Standard,
    Full,
}

impl Scale {
    pub fn as_str(self) -> &'static str {
        match self {
            Scale::Smoke => "smoke",
            Scale::Quick => "quick",
            Scale::Standard => "standard",
            Scale::Full => "full",
        }
    }

    /// Pick one of four values by scale.
    pub fn pick<T>(self, smoke: T, quick: T, standard: T, full: T) -> T {
        match self {
            Scale::Smoke => smoke,
            Scale::Quick => quick,
            Scale::Standard => standard,
            Scale::Full => full,
        }
    }
}

/// What a suite runs against. `memory` and `proactive` have a real backend; others ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum Backend {
    /// The in-crate [`FakeIndex`](crate::index::FakeIndex) with a simulated watcher (fast; CI).
    #[default]
    Fake,
    /// The real stack in process: an encrypted `nugget-store`, `nugget-ingest` with the `files`
    /// connector watching a temporary folder, and `nugget-retrieve` for queries.
    Real,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Fake => "fake",
            Backend::Real => "real",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    pub scale: Scale,
    pub seed: u64,
    pub backend: Backend,
    /// Real backend only: base URL of an OpenAI-compatible embeddings server (e.g.
    /// `http://127.0.0.1:8080/v1`) to use instead of the in-process deterministic embedder.
    pub embed_url: Option<String>,
    /// Model name sent to `embed_url`.
    pub embed_model: Option<String>,
    /// `draft` suite options (provider, fake-model latency, locality).
    pub draft: suites::draft::DraftOptions,
    /// `--local-models` options (real local models through `nugget-runtime`).
    pub local: crate::local::LocalOptions,
    /// `doubletap` suite options (read-ahead).
    pub doubletap: suites::doubletap::DoubletapOptions,
    /// `energy` suite options (which scenarios run).
    pub energy: suites::energy::EnergyOptions,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            scale: Scale::default(),
            seed: 42,
            backend: Backend::default(),
            embed_url: None,
            embed_model: None,
            draft: suites::draft::DraftOptions::default(),
            local: crate::local::LocalOptions::default(),
            doubletap: suites::doubletap::DoubletapOptions::default(),
            energy: suites::energy::EnergyOptions::default(),
        }
    }
}

/// Handed to a benchmark while it runs. Suites record the parameters they used so a report is
/// reproducible on its own.
#[derive(Debug)]
pub struct RunContext {
    pub config: RunConfig,
    params: BTreeMap<String, serde_json::Value>,
}

impl RunContext {
    pub fn new(config: RunConfig) -> Self {
        Self {
            config,
            params: BTreeMap::new(),
        }
    }

    pub fn scale(&self) -> Scale {
        self.config.scale
    }

    pub fn seed(&self) -> u64 {
        self.config.seed
    }

    pub fn backend(&self) -> Backend {
        self.config.backend
    }

    /// Record a parameter in the report's `config.params`.
    pub fn param(&mut self, name: &str, value: impl Into<serde_json::Value>) {
        self.params.insert(name.to_owned(), value.into());
    }

    pub fn params(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.params
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    /// The suite is registered but not built yet (or a crate it measures is still a stub). The
    /// string is a one-paragraph plan, or says what is missing.
    #[error("not yet implemented: {0}")]
    NotImplemented(Cow<'static, str>),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Core(#[from] nugget_core::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// A benchmark suite.
pub trait Benchmark: Send + Sync {
    /// Registry name, e.g. `perf`. Lowercase; used in report file names.
    fn name(&self) -> &'static str;
    /// One-line description for `--list`.
    fn description(&self) -> &'static str;
    /// Whether the suite produces metrics yet.
    fn implemented(&self) -> bool {
        true
    }
    /// Run and return metrics. Record parameters with [`RunContext::param`].
    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError>;
}

/// Every registered suite, in display order.
pub fn registry() -> Vec<Box<dyn Benchmark>> {
    vec![
        Box::new(suites::memory::MemoryBench),
        Box::new(suites::perf::PerfBench),
        Box::new(suites::proactive::ProactiveBench),
        Box::new(suites::draft::DraftBench),
        Box::new(suites::doubletap::DoubleTapBench),
        Box::new(suites::cua::CuaBench::default()),
        Box::new(suites::knowledge::KnowledgeBench),
        Box::new(suites::energy::EnergyBench),
    ]
}

/// Look up a suite by name.
pub fn find(name: &str) -> Option<Box<dyn Benchmark>> {
    registry().into_iter().find(|b| b.name() == name)
}

/// Check the report invariants the schema can't express: finite values and unique names.
fn validate_metrics(metrics: &[Metric]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for m in metrics {
        if !m.value.is_finite() {
            return Err(format!("metric {} has a non-finite value", m.name));
        }
        if m.target.is_some_and(|t| !t.is_finite()) {
            return Err(format!("metric {} has a non-finite target", m.name));
        }
        if !valid_metric_name(&m.name) {
            return Err(format!(
                "metric name {:?} is not lowercase dotted form",
                m.name
            ));
        }
        if !seen.insert(m.name.as_str()) {
            return Err(format!("duplicate metric {}", m.name));
        }
    }
    Ok(())
}

/// Mirrors the `name` pattern in the JSON schema: `^[a-z][a-z0-9_@]*(\.[a-z0-9_@]+)*$`.
pub fn valid_metric_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let ok_part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'@'))
    };
    match parts.next() {
        Some(first)
            if first.as_bytes().first().is_some_and(u8::is_ascii_lowercase) && ok_part(first) =>
        {
            parts.all(ok_part)
        }
        _ => false,
    }
}

/// Run one benchmark and build its report. Never panics on suite errors: they become a
/// `failed` report.
pub fn run_benchmark(bench: &dyn Benchmark, config: &RunConfig) -> Report {
    let mut ctx = RunContext::new(config.clone());
    let started_at = OffsetDateTime::now_utc();
    let clock = Instant::now();
    let result = bench.run(&mut ctx);
    let duration_ms = clock.elapsed().as_millis() as u64;
    let finished_at = OffsetDateTime::now_utc();

    let (status, metrics, message) = match result {
        Ok(metrics) => match validate_metrics(&metrics) {
            Ok(()) => (Status::Ok, metrics, None),
            Err(e) => (Status::Failed, Vec::new(), Some(e)),
        },
        Err(BenchError::NotImplemented(plan)) => {
            (Status::NotImplemented, Vec::new(), Some(plan.into_owned()))
        }
        Err(e) => (Status::Failed, Vec::new(), Some(e.to_string())),
    };

    let targets = metrics
        .iter()
        .filter_map(TargetCheck::from_metric)
        .collect();
    Report {
        schema_version: SCHEMA_VERSION,
        suite: bench.name().to_owned(),
        status,
        started_at,
        finished_at,
        duration_ms,
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_commit: detect_git_commit(),
        environment: Environment::detect(),
        config: ReportConfig {
            scale: config.scale.as_str().to_owned(),
            seed: config.seed,
            params: ctx.params,
        },
        metrics,
        targets,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Direction;

    struct Fixed(Vec<Metric>);
    impl Benchmark for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn description(&self) -> &'static str {
            "test"
        }
        fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            ctx.param("items", 3);
            Ok(self.0.clone())
        }
    }

    struct Planned;
    impl Benchmark for Planned {
        fn name(&self) -> &'static str {
            "planned"
        }
        fn description(&self) -> &'static str {
            "test"
        }
        fn implemented(&self) -> bool {
            false
        }
        fn run(&self, _ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            Err(BenchError::NotImplemented(
                "planned for a later milestone; see bench/README.md".into(),
            ))
        }
    }

    struct Boom;
    impl Benchmark for Boom {
        fn name(&self) -> &'static str {
            "boom"
        }
        fn description(&self) -> &'static str {
            "test"
        }
        fn run(&self, _ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            Err(BenchError::Failed("disk on fire".into()))
        }
    }

    fn m(name: &str, value: f64) -> Metric {
        Metric::new(name, value, "x", Direction::Info, "")
    }

    #[test]
    fn registry_has_the_registered_suites() {
        let names: Vec<&str> = registry().iter().map(|b| b.name()).collect();
        assert_eq!(
            names,
            [
                "memory",
                "perf",
                "proactive",
                "draft",
                "doubletap",
                "cua",
                "knowledge",
                "energy"
            ]
        );
        assert!(find("energy").is_some_and(|b| b.implemented()));
        assert!(find("proactive").is_some_and(|b| b.implemented()));
        assert!(find("knowledge").is_some_and(|b| b.implemented()));
        assert!(find("perf").is_some_and(|b| b.implemented()));
        assert!(find("memory").is_some_and(|b| b.implemented()));
        assert!(find("draft").is_some_and(|b| b.implemented()));
        assert!(find("doubletap").is_some_and(|b| b.implemented()));
        assert!(find("cua").is_some_and(|b| b.implemented()));
        assert!(find("nope").is_none());
        for b in registry() {
            assert!(valid_metric_name(b.name()));
            assert!(!b.description().is_empty());
        }
    }

    #[test]
    fn ok_run_records_params_and_metrics() {
        let r = run_benchmark(
            &Fixed(vec![m("a.b", 1.0), m("c", 2.0)]),
            &RunConfig::default(),
        );
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.suite, "fixed");
        assert_eq!(r.schema_version, SCHEMA_VERSION);
        assert_eq!(r.metrics.len(), 2);
        assert_eq!(r.config.params["items"], 3);
        assert_eq!(r.config.scale, "standard");
        assert_eq!(r.config.seed, 42);
        assert!(r.finished_at >= r.started_at);
        assert!(r.message.is_none());
    }

    #[test]
    fn targets_are_derived_from_metrics_with_targets() {
        let r = run_benchmark(
            &Fixed(vec![
                Metric::new("lat", 70.0, "s", Direction::LowerIsBetter, "").with_target(60.0),
                Metric::new("rec", 0.9, "ratio", Direction::HigherIsBetter, "").with_target(0.8),
                m("info", 1.0),
            ]),
            &RunConfig::default(),
        );
        assert_eq!(r.targets.len(), 2);
        assert_eq!(r.targets[0].metric, "lat");
        assert!(!r.targets[0].pass);
        assert!(r.targets[1].pass);
        assert!(r.targets_missed());
        let ok = run_benchmark(&Fixed(vec![m("a", 1.0)]), &RunConfig::default());
        assert!(ok.targets.is_empty() && !ok.targets_missed());
    }

    #[test]
    fn errors_become_failed_reports() {
        let r = run_benchmark(&Boom, &RunConfig::default());
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.message.as_deref(), Some("disk on fire"));
        assert!(r.metrics.is_empty());
    }

    #[test]
    fn invalid_metrics_fail_the_run() {
        for bad in [
            vec![m("a", f64::NAN)],
            vec![m("a", f64::INFINITY)],
            vec![m("a", 1.0), m("a", 2.0)],
            vec![m("Upper", 1.0)],
            vec![m("a..b", 1.0)],
            vec![m("", 1.0)],
            vec![m("a", 1.0).with_target(f64::NAN)],
        ] {
            let r = run_benchmark(&Fixed(bad.clone()), &RunConfig::default());
            assert_eq!(r.status, Status::Failed, "{bad:?}");
            assert!(r.metrics.is_empty());
        }
    }

    #[test]
    fn planned_suite_reports_not_implemented() {
        let r = run_benchmark(&Planned, &RunConfig::default());
        assert_eq!(r.status, Status::NotImplemented);
        assert!(r.message.as_deref().is_some_and(|m| m.len() > 20));
        assert!(r.metrics.is_empty());
    }

    #[test]
    fn metric_names() {
        for ok in [
            "a",
            "ingest.items_per_s",
            "retrieval.hybrid.recall@10",
            "x1.y_2",
        ] {
            assert!(valid_metric_name(ok), "{ok}");
        }
        for bad in ["", "1a", "_a", "a.", ".a", "a-b", "A", "a b"] {
            assert!(!valid_metric_name(bad), "{bad}");
        }
    }

    #[test]
    fn scale_pick() {
        assert_eq!(Scale::Smoke.pick(1, 2, 3, 4), 1);
        assert_eq!(Scale::Full.pick(1, 2, 3, 4), 4);
        assert_eq!(Scale::default(), Scale::Standard);
    }
}
