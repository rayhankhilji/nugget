//! Suites that are registered but not built yet. Running them yields a `not_implemented`
//! report whose message is the plan; `bench/README.md` has the full methodology.

use crate::report::Metric;
use crate::runner::{BenchError, Benchmark, RunContext};

/// `bench/draft`: reply drafting quality and latency (Double Tap, milestone M3).
pub struct DraftBench;

pub const DRAFT_PLAN: &str = "Planned for M3 (Double Tap). Reply drafting against synthetic personas and consented \
held-out threads, scored by pairwise LLM judging against a screenshot-only baseline (voice match, factuality, \
helpfulness) plus human spot checks, with p50/p95 time to first inserted token for cloud (target < 1.2 s) and \
local (target < 3 s on a 16 GB machine) tiers. See bench/README.md.";

impl Benchmark for DraftBench {
    fn name(&self) -> &'static str {
        "draft"
    }
    fn description(&self) -> &'static str {
        "Reply drafting: judged quality and time to first inserted token (planned, M3)"
    }
    fn implemented(&self) -> bool {
        false
    }
    fn run(&self, _ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        Err(BenchError::NotImplemented(DRAFT_PLAN.into()))
    }
}

/// `bench/cua`: computer-use task success (agent and `nugget-a11y`, milestone M4).
pub struct CuaBench;

pub const CUA_PLAN: &str = "Planned for M4 (agent and computer use). Scripted tasks in macOS, Windows and Linux \
VMs (form fill, email triage, file organising, calendar booking, web checkout up to the confirmation step), \
each with a programmatic outcome check. Metrics: verified success rate, steps, wall time, and safety violations \
(risky actions without confirmation; target 0), plus an OSWorld-style subset where licences allow. See \
bench/README.md.";

impl Benchmark for CuaBench {
    fn name(&self) -> &'static str {
        "cua"
    }
    fn description(&self) -> &'static str {
        "Computer use: verified task success, steps, time and safety violations (planned, M4)"
    }
    fn implemented(&self) -> bool {
        false
    }
    fn run(&self, _ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        Err(BenchError::NotImplemented(CUA_PLAN.into()))
    }
}
