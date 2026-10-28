//! The benchmark report: a stable, versioned JSON document.
//!
//! The schema lives in `bench/schema/report.schema.json`. Any incompatible change to these types
//! must bump [`SCHEMA_VERSION`] and update the schema file; the tests enforce that the two stay in
//! sync. Additive, optional fields (like `targets`) keep version 1: every earlier report still
//! validates and deserializes.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Version of the report JSON schema. Bump on any incompatible change.
pub const SCHEMA_VERSION: u32 = 1;

/// The JSON schema, embedded so tools can print it and tests can check it.
pub const SCHEMA_JSON: &str = include_str!("../schema/report.schema.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The suite ran and every metric was recorded.
    Ok,
    /// The suite is registered but has no implementation yet; `message` holds the plan.
    NotImplemented,
    /// The suite errored; `message` holds the error.
    Failed,
}

/// Which way is "better" for a metric. Drives regression checks and `target_met`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    HigherIsBetter,
    LowerIsBetter,
    /// Informational only; no better or worse.
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    /// Dotted, lowercase name, e.g. `ingest.items_per_s` or `retrieval.hybrid.recall@10`.
    pub name: String,
    pub value: f64,
    /// Unit, e.g. `items/s`, `ms`, `us`, `s`, `bytes`, `ratio`.
    pub unit: String,
    pub direction: Direction,
    /// The product target from `docs/PLAN.md` §2, if this metric has one.
    pub target: Option<f64>,
    /// Whether `value` meets `target` given `direction`. `None` without a target.
    pub target_met: Option<bool>,
    pub description: String,
}

impl Metric {
    pub fn new(
        name: impl Into<String>,
        value: f64,
        unit: impl Into<String>,
        direction: Direction,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            value,
            unit: unit.into(),
            direction,
            target: None,
            target_met: None,
            description: description.into(),
        }
    }

    /// Attach a target and compute `target_met`.
    pub fn with_target(mut self, target: f64) -> Self {
        self.target = Some(target);
        self.target_met = match self.direction {
            Direction::HigherIsBetter => Some(self.value >= target),
            Direction::LowerIsBetter => Some(self.value <= target),
            Direction::Info => None,
        };
        self
    }
}

/// How a metric's value is compared with its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// Pass when `value >= target` (a `higher_is_better` metric).
    AtLeast,
    /// Pass when `value <= target` (a `lower_is_better` metric).
    AtMost,
}

/// Pass/fail of one target, one per metric that has a target. Derived from the metrics by the
/// runner, so it always agrees with each metric's `target_met`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetCheck {
    /// Name of the metric the target applies to.
    pub metric: String,
    pub comparison: Comparison,
    pub target: f64,
    pub value: f64,
    pub pass: bool,
}

impl TargetCheck {
    /// The check for `m`, if it has a target and a direction.
    pub fn from_metric(m: &Metric) -> Option<Self> {
        let target = m.target?;
        let comparison = match m.direction {
            Direction::HigherIsBetter => Comparison::AtLeast,
            Direction::LowerIsBetter => Comparison::AtMost,
            Direction::Info => return None,
        };
        Some(Self {
            metric: m.name.clone(),
            comparison,
            target,
            value: m.value,
            pass: m.target_met?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// `std::env::consts::OS`: `linux`, `macos`, `windows`, ...
    pub os: String,
    pub arch: String,
    /// Logical CPUs available to the process.
    pub cpus: u32,
    /// `debug` or `release`. Perf numbers from debug builds are not representative.
    pub build_profile: String,
}

impl Environment {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportConfig {
    /// `smoke`, `quick`, `standard` or `full`.
    pub scale: String,
    pub seed: u64,
    /// Suite-specific parameters (corpus size, query count, ...).
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub suite: String,
    pub status: Status,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub duration_ms: u64,
    /// Version of `nugget-bench` that produced the report.
    pub harness_version: String,
    /// Commit of the code under test, when known.
    pub git_commit: Option<String>,
    pub environment: Environment,
    pub config: ReportConfig,
    pub metrics: Vec<Metric>,
    /// Pass/fail per metric target (added in harness 0.0.1 for M1; optional, omitted when no
    /// metric has a target, so reports written before it still validate).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<TargetCheck>,
    /// Plan (for `not_implemented`), error (for `failed`), or a note.
    pub message: Option<String>,
}

impl Report {
    pub fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|m| m.name == name)
    }

    /// Whether any target was missed.
    pub fn targets_missed(&self) -> bool {
        self.targets.iter().any(|t| !t.pass)
    }

    pub fn to_json_pretty(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// `<suite>-<UTC timestamp>` with no characters that are invalid in Windows file names.
    pub fn file_stem(&self) -> String {
        let t = self.started_at.to_offset(time::UtcOffset::UTC);
        format!(
            "{}-{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            self.suite,
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        )
    }

    /// Write to `<dir>/<suite>-<timestamp>.json`, creating `dir`. Never overwrites: a numeric
    /// suffix is added if the name is taken.
    pub fn write_to_dir(&self, dir: &Path) -> io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let json = self.to_json_pretty().map_err(io::Error::other)?;
        let stem = self.file_stem();
        for n in 0..1000u32 {
            let name = if n == 0 {
                format!("{stem}.json")
            } else {
                format!("{stem}-{n}.json")
            };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(json.as_bytes())?;
                    f.write_all(b"\n")?;
                    return Ok(path);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many reports with the same timestamp",
        ))
    }
}

/// Best-effort commit of the code under test, without running git: `NUGGET_BENCH_COMMIT`,
/// then `GITHUB_SHA`, then reading `.git/HEAD` at the workspace root.
pub fn detect_git_commit() -> Option<String> {
    for var in ["NUGGET_BENCH_COMMIT", "GITHUB_SHA"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_owned();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let git = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.join(".git");
    read_git_head(&git)
}

fn read_git_head(git: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref: ") else {
        return is_sha(head).then(|| head.to_owned());
    };
    if let Ok(sha) = std::fs::read_to_string(git.join(reference)) {
        let sha = sha.trim();
        return is_sha(sha).then(|| sha.to_owned());
    }
    let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (sha, name) = line.split_once(' ')?;
        (name == reference && is_sha(sha)).then(|| sha.to_owned())
    })
}

fn is_sha(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(status: Status) -> Report {
        let at = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let mut params = BTreeMap::new();
        params.insert("docs".to_owned(), serde_json::json!(1000));
        Report {
            schema_version: SCHEMA_VERSION,
            suite: "perf".to_owned(),
            status,
            started_at: at,
            finished_at: at,
            duration_ms: 12,
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: None,
            environment: Environment::detect(),
            config: ReportConfig {
                scale: "quick".to_owned(),
                seed: 42,
                params,
            },
            metrics: vec![
                Metric::new(
                    "ingest.items_per_s",
                    1234.5,
                    "items/s",
                    Direction::HigherIsBetter,
                    "x",
                ),
                Metric::new("freshness.p95_s", 3.0, "s", Direction::LowerIsBetter, "y")
                    .with_target(60.0),
            ],
            targets: vec![TargetCheck {
                metric: "freshness.p95_s".to_owned(),
                comparison: Comparison::AtMost,
                target: 60.0,
                value: 3.0,
                pass: true,
            }],
            message: None,
        }
    }

    #[test]
    fn target_met_follows_direction() {
        let up = Metric::new("a", 5.0, "x", Direction::HigherIsBetter, "");
        assert_eq!(up.clone().with_target(4.0).target_met, Some(true));
        assert_eq!(up.with_target(6.0).target_met, Some(false));
        let down = Metric::new("b", 5.0, "x", Direction::LowerIsBetter, "");
        assert_eq!(down.clone().with_target(6.0).target_met, Some(true));
        assert_eq!(down.with_target(4.0).target_met, Some(false));
        let info = Metric::new("c", 5.0, "x", Direction::Info, "").with_target(1.0);
        assert_eq!(info.target_met, None);
    }

    #[test]
    fn json_round_trip() {
        let r = sample(Status::Ok);
        let json = r.to_json_pretty().unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["metrics"][0]["direction"], "higher_is_better");
        assert_eq!(v["started_at"], "2026-09-21T14:13:20Z");
    }

    #[test]
    fn target_checks_follow_metric_targets() {
        let up = Metric::new("r", 0.9, "ratio", Direction::HigherIsBetter, "").with_target(0.95);
        let c = TargetCheck::from_metric(&up).unwrap();
        assert_eq!(c.comparison, Comparison::AtLeast);
        assert!(!c.pass);
        let down = Metric::new("f", 3.0, "s", Direction::LowerIsBetter, "").with_target(60.0);
        assert!(TargetCheck::from_metric(&down).unwrap().pass);
        let info = Metric::new("i", 1.0, "x", Direction::Info, "").with_target(1.0);
        assert!(TargetCheck::from_metric(&info).is_none());
        assert!(
            TargetCheck::from_metric(&Metric::new("n", 1.0, "x", Direction::Info, "")).is_none()
        );
    }

    #[test]
    fn reports_without_targets_still_deserialize() {
        // A report as written before `targets` existed.
        let mut r = sample(Status::Ok);
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("targets");
        let back: Report = serde_json::from_value(v).unwrap();
        r.targets.clear();
        assert_eq!(back, r);
        // ...and an empty list is not written at all.
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("targets").is_none());
        assert!(!r.targets_missed());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut v = serde_json::to_value(sample(Status::Ok)).unwrap();
        v["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Report>(v).is_err());
    }

    #[test]
    fn file_stem_is_portable() {
        let stem = sample(Status::Ok).file_stem();
        assert_eq!(stem, "perf-20260921T141320Z");
        assert!(!stem.contains(':'));
    }

    #[test]
    fn write_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested").join("results");
        let r = sample(Status::Ok);
        let a = r.write_to_dir(&sub).unwrap();
        let b = r.write_to_dir(&sub).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap(), "perf-20260921T141320Z.json");
        assert_eq!(b.file_name().unwrap(), "perf-20260921T141320Z-1.json");
        let back: Report = serde_json::from_str(&std::fs::read_to_string(&a).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn git_head_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        std::fs::write(git.join("HEAD"), format!("{sha}\n")).unwrap();
        assert_eq!(read_git_head(git).as_deref(), Some(sha));

        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_git_head(git), None);
        std::fs::write(
            git.join("packed-refs"),
            format!("# pack-refs\n{sha} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(read_git_head(git).as_deref(), Some(sha));

        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        let loose = "fedcba9876543210fedcba9876543210fedcba98";
        std::fs::write(git.join("refs/heads/main"), loose).unwrap();
        assert_eq!(read_git_head(git).as_deref(), Some(loose));
    }
}
