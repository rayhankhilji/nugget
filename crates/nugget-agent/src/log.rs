//! The append-only action log and the human-readable run report rendered from it.
//!
//! Every model turn, every policy decision and every executed action is one [`LogEntry`]. The
//! loop stops (`Outcome::Error`) if an entry cannot be written: an action that cannot be audited
//! is not taken. The daemon backs [`ActionLog`] with the store's hash-chained audit log; this
//! crate ships [`MemoryLog`] and [`JsonlLog`].
//!
//! JSONL format: one entry per line, `{"task_id", "seq", "at_ms", "kind", ...fields}`, where
//! `kind` is one of `run_started`, `resumed`, `model_turn`, `decision`, `executed`, `injection`,
//! `suspended`, `finished`. `seq` counts from 0 per task and has no gaps.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nugget_core::{Error, Result};
use nugget_policy::{ActionDescriptor, RiskClass};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::injection::InjectionFinding;
use crate::tool::{Claim, Verification};
use crate::types::{GateOutcome, Outcome, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub task_id: String,
    pub seq: u64,
    /// Unix time in milliseconds.
    pub at_ms: u64,
    #[serde(flatten)]
    pub record: Record,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    RunStarted {
        goal: String,
        tools: Vec<String>,
        max_steps: u32,
        deadline_ms: u64,
        max_tokens: u64,
    },
    Resumed {
        answer: String,
    },
    /// One model turn: a summary of its text and the calls it made.
    ModelTurn {
        step: u32,
        text: String,
        tool_calls: Vec<String>,
        stop_reason: String,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
    },
    /// What the gate (and the user) decided for one proposed call. Written before it executes.
    Decision {
        step: u32,
        action_id: String,
        call_id: String,
        tool: String,
        /// Redacted by the tool.
        args: Value,
        risk: RiskClass,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<ActionDescriptor>,
        #[serde(flatten)]
        decision: GateOutcome,
        #[serde(default)]
        forced_by_injection: bool,
        /// Time spent deciding, including waiting for the user.
        duration_ms: u64,
    },
    /// The action ran (or was interrupted while running).
    Executed {
        step: u32,
        action_id: String,
        tool: String,
        is_error: bool,
        /// Truncated result text.
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verification: Option<Verification>,
        #[serde(default)]
        interrupted: bool,
        duration_ms: u64,
    },
    Injection {
        step: u32,
        action_id: String,
        tool: String,
        finding: InjectionFinding,
    },
    Suspended {
        question: String,
    },
    Finished {
        #[serde(flatten)]
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim: Option<Claim>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        steps: u32,
        usage: Usage,
        elapsed_ms: u64,
    },
}

/// Append-only sink for [`LogEntry`]s. Must be durable once `append` returns `Ok`.
pub trait ActionLog: Send + Sync {
    fn append(&self, entry: &LogEntry) -> Result<()>;
}

/// In-memory log, for tests and for runs whose log is persisted elsewhere.
#[derive(Debug, Default)]
pub struct MemoryLog {
    entries: Mutex<Vec<LogEntry>>,
}

impl MemoryLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn for_task(&self, task_id: &str) -> Vec<LogEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.task_id == task_id)
            .collect()
    }
}

impl ActionLog for MemoryLog {
    fn append(&self, entry: &LogEntry) -> Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry.clone());
        Ok(())
    }
}

/// One JSON object per line, appended and flushed (and `fsync`ed) per entry.
#[derive(Debug)]
pub struct JsonlLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl JsonlLog {
    /// Open (creating if needed) for appending.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read every entry back. A malformed line is an `Integrity` error naming its number.
    pub fn read(path: impl AsRef<Path>) -> Result<Vec<LogEntry>> {
        let reader = BufReader::new(File::open(path)?);
        let mut out = Vec::new();
        for (n, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry = serde_json::from_str(&line)
                .map_err(|e| Error::Integrity(format!("action log line {}: {e}", n + 1)))?;
            out.push(entry);
        }
        Ok(out)
    }
}

impl ActionLog for JsonlLog {
    fn append(&self, entry: &LogEntry) -> Result<()> {
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        f.write_all(line.as_bytes())?;
        f.flush()?;
        f.sync_data()?;
        Ok(())
    }
}

/// Check that entries form complete runs: per task, `seq` starts at 0 with no gaps, every
/// executing decision has an `executed` entry, and each run ends with `finished` or
/// `suspended`. Returns the problems found (empty means complete).
pub fn check_complete(entries: &[LogEntry]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut tasks: Vec<&str> = entries.iter().map(|e| e.task_id.as_str()).collect();
    tasks.dedup();
    tasks.sort();
    tasks.dedup();
    for task in tasks {
        let es: Vec<&LogEntry> = entries.iter().filter(|e| e.task_id == task).collect();
        for (i, e) in es.iter().enumerate() {
            if e.seq != i as u64 {
                problems.push(format!("{task}: entry {i} has seq {}", e.seq));
                break;
            }
        }
        for e in &es {
            if let Record::Decision {
                action_id,
                decision,
                ..
            } = &e.record
                && decision.executes()
                && !es.iter().any(|x| {
                    matches!(&x.record, Record::Executed { action_id: a, .. } if a == action_id)
                })
            {
                problems.push(format!("{task}: {action_id} allowed but never logged as executed"));
            }
        }
        match es.last().map(|e| &e.record) {
            Some(Record::Finished { .. } | Record::Suspended { .. }) => {}
            _ => problems.push(format!("{task}: no finished or suspended entry at the end")),
        }
    }
    problems
}

/// Render a run report from log entries (one or more tasks), for people.
pub fn explain(entries: &[LogEntry]) -> String {
    let mut out = String::new();
    let mut current_task: Option<&str> = None;
    for e in entries {
        if current_task != Some(e.task_id.as_str()) {
            if current_task.is_some() {
                out.push('\n');
            }
            let _ = writeln!(out, "Task {}", e.task_id);
            current_task = Some(&e.task_id);
        }
        match &e.record {
            Record::RunStarted {
                goal,
                tools,
                max_steps,
                deadline_ms,
                max_tokens,
            } => {
                let _ = writeln!(out, "  Goal: {goal}");
                let _ = writeln!(
                    out,
                    "  Budgets: {max_steps} steps, {} s, {max_tokens} tokens. Tools: {}",
                    deadline_ms / 1000,
                    tools.join(", ")
                );
            }
            Record::Resumed { answer } => {
                let _ = writeln!(out, "  Resumed with the user's answer: {answer:?}");
            }
            Record::ModelTurn {
                step,
                text,
                tool_calls,
                input_tokens,
                output_tokens,
                duration_ms,
                ..
            } => {
                let _ = write!(
                    out,
                    "  Step {step} · model {duration_ms} ms · {input_tokens} in / {output_tokens} out"
                );
                if !text.is_empty() {
                    let _ = write!(out, " · says {text:?}");
                }
                if tool_calls.is_empty() {
                    let _ = writeln!(out, " · no tool calls");
                } else {
                    let _ = writeln!(out, " · calls {}", tool_calls.join(", "));
                }
            }
            Record::Decision {
                tool,
                args,
                risk,
                decision,
                forced_by_injection,
                action_id,
                ..
            } => {
                let reason = match decision {
                    GateOutcome::Approved { reason }
                    | GateOutcome::Rejected { reason }
                    | GateOutcome::TimedOut { reason }
                    | GateOutcome::Denied { reason }
                    | GateOutcome::InvalidArgs { reason } => format!(": {reason}"),
                    _ => String::new(),
                };
                let forced = if *forced_by_injection {
                    " (asked because of suspected injection)"
                } else {
                    ""
                };
                let _ = writeln!(
                    out,
                    "    [{}{reason}]{forced} {tool} {args} · risk {} · {}",
                    decision.label(),
                    risk_label(*risk),
                    short_id(action_id),
                );
            }
            Record::Executed {
                is_error,
                result,
                verification,
                interrupted,
                duration_ms,
                ..
            } => {
                let status = if *interrupted {
                    "interrupted"
                } else if *is_error {
                    "error"
                } else {
                    "ok"
                };
                let _ = write!(out, "      → {status} {result:?} · {duration_ms} ms");
                match verification {
                    Some(Verification::Passed(d)) => {
                        let _ = write!(out, " · verified: {d}");
                    }
                    Some(Verification::Failed(d)) => {
                        let _ = write!(out, " · VERIFICATION FAILED: {d}");
                    }
                    Some(Verification::Unverifiable) => {
                        let _ = write!(out, " · not verifiable");
                    }
                    None => {}
                }
                out.push('\n');
            }
            Record::Injection { tool, finding, .. } => {
                let _ = writeln!(
                    out,
                    "      ! possible prompt injection in {tool} output ({}): {:?}",
                    finding.patterns.join(", "),
                    finding.excerpt
                );
            }
            Record::Suspended { question } => {
                let _ = writeln!(out, "  Paused to ask the user: {question:?}");
            }
            Record::Finished {
                outcome,
                claim,
                summary,
                steps,
                usage,
                elapsed_ms,
            } => {
                let _ = write!(out, "  Outcome: {outcome}");
                if let Some(c) = claim {
                    let _ = write!(out, " (model claimed {})", claim_label(*c));
                }
                out.push('\n');
                if let Some(s) = summary {
                    let _ = writeln!(out, "  Summary: {s}");
                }
                let _ = writeln!(
                    out,
                    "  {steps} steps · {} tokens · {:.1} s",
                    usage.total(),
                    *elapsed_ms as f64 / 1000.0
                );
            }
        }
    }
    out
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

pub(crate) fn risk_label(r: RiskClass) -> &'static str {
    match r {
        RiskClass::Observe => "observe",
        RiskClass::Edit => "edit",
        RiskClass::Send => "send",
        RiskClass::Destroy => "destroy",
        RiskClass::Spend => "spend",
        RiskClass::Security => "security",
    }
}

fn claim_label(c: Claim) -> &'static str {
    match c {
        Claim::Success => "success",
        Claim::Partial => "partial",
        Claim::Failure => "failure",
        Claim::Refused => "refused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let log = JsonlLog::open(&path).unwrap();
        let entries = vec![
            LogEntry {
                task_id: "t".into(),
                seq: 0,
                at_ms: 1,
                record: Record::Decision {
                    step: 1,
                    action_id: "act_1".into(),
                    call_id: "c".into(),
                    tool: "x".into(),
                    args: serde_json::json!({"a": 1}),
                    risk: RiskClass::Send,
                    action: None,
                    decision: GateOutcome::Denied {
                        reason: "no".into(),
                    },
                    forced_by_injection: false,
                    duration_ms: 0,
                },
            },
            LogEntry {
                task_id: "t".into(),
                seq: 1,
                at_ms: 2,
                record: Record::Finished {
                    outcome: Outcome::Failed("x".into()),
                    claim: Some(Claim::Failure),
                    summary: None,
                    steps: 1,
                    usage: Usage::default(),
                    elapsed_ms: 3,
                },
            },
        ];
        for e in &entries {
            log.append(e).unwrap();
        }
        let line = std::fs::read_to_string(&path).unwrap();
        let first: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(first["kind"], "decision");
        assert_eq!(first["decision"], "denied");
        assert_eq!(first["reason"], "no");
        assert_eq!(JsonlLog::read(&path).unwrap(), entries);
        assert!(check_complete(&entries).is_empty());

        std::fs::write(&path, "{not json}\n").unwrap();
        assert!(matches!(JsonlLog::read(&path), Err(Error::Integrity(_))));
    }
}
