//! GG3 explore-layer probes: a misbehaving (scripted) model against the computer-use agent loop.
//!
//! `gg3-probes [FILTER] [--repeat N]` runs every probe whose name contains FILTER, N times
//! (default 1), and prints each check as `OK`, `FINDING` or `INFO`, then whether every repeat gave
//! the same verdicts (reproduction rate). See README.md.

mod probes;
mod rig;

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Ok,
    Finding,
    Info,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    pub verdict: Verdict,
    pub detail: String,
}

impl Check {
    pub fn new(id: impl Into<String>, verdict: Verdict, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            verdict,
            detail: detail.into(),
        }
    }
    pub fn ok(id: impl Into<String>, d: impl Into<String>) -> Self {
        Self::new(id, Verdict::Ok, d)
    }
    pub fn finding(id: impl Into<String>, d: impl Into<String>) -> Self {
        Self::new(id, Verdict::Finding, d)
    }
    pub fn info(id: impl Into<String>, d: impl Into<String>) -> Self {
        Self::new(id, Verdict::Info, d)
    }
}

type ProbeFn = fn() -> Vec<Check>;

fn main() {
    let mut filter = String::new();
    let mut repeat = 1usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--repeat" {
            repeat = args.next().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
        } else {
            filter = a;
        }
    }
    let all: Vec<(&str, ProbeFn)> = vec![
        ("p1_bypass_sweep", probes::p1_bypass_sweep),
        ("p1_mail_keyboard_paths", probes::p1_mail_keyboard_paths),
        ("p1_stale_focus_cache", probes::p1_stale_focus_cache),
        ("p1_external_relabel", probes::p1_external_relabel),
        ("p2_scope", probes::p2_scope),
        ("p3_preview", probes::p3_preview),
        ("p4_detector_variants", probes::p4_detector_variants),
        ("p4_injection_e2e", probes::p4_injection_e2e),
        ("p4_fence", probes::p4_fence),
        ("p4_suspicion_cleared", probes::p4_suspicion_cleared),
        ("p5_password", probes::p5_password),
        ("p6_verification", probes::p6_verification),
        ("p7_triage", probes::p7_triage),
    ];
    let mut findings = 0;
    let mut unstable = Vec::new();
    for (name, f) in all.iter().filter(|(n, _)| n.contains(&filter)) {
        println!("\n=== {name} ===");
        let mut runs: Vec<BTreeMap<String, Verdict>> = Vec::new();
        for i in 0..repeat {
            let checks = f();
            if i == 0 {
                for c in &checks {
                    let tag = match c.verdict {
                        Verdict::Ok => "OK     ",
                        Verdict::Finding => "FINDING",
                        Verdict::Info => "INFO   ",
                    };
                    println!("[{tag}] {}: {}", c.id, c.detail);
                    if c.verdict == Verdict::Finding {
                        findings += 1;
                    }
                }
            }
            runs.push(checks.into_iter().map(|c| (c.id, c.verdict)).collect());
        }
        let same = runs.windows(2).all(|w| w[0] == w[1]);
        println!(
            "--- {name}: verdicts identical in {}/{repeat} runs",
            if same { repeat } else { 0 }
        );
        if !same {
            unstable.push(*name);
        }
    }
    println!("\n{findings} finding check(s); unstable probes: {unstable:?}");
}
