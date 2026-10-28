//! [`ChainedLog`]: the agent's [`ActionLog`] on top of the policy crate's hash-chained,
//! tamper-evident log, so agent runs, policy decisions, confirmations and halts share one chain.
//!
//! Every agent [`LogEntry`] becomes one `LogEvent::External { source: "agent", kind, detail }`
//! where `kind` is the record kind (`run_started`, `decision`, `executed`, ...) and `detail` is
//! the entry's JSON. Arguments are already redacted by the tool; the action descriptor in a
//! `decision` is redacted on the way in ([`RedactedAction`]: secure text replaced, digit runs
//! masked, previews capped). Each executed action is also recorded with
//! [`nugget_policy::ActionLog::record_execution`], so the policy view of the chain lists it like
//! any other executor's action. The readers ([`ChainedLog::entries`], [`ChainedLog::for_task`])
//! rebuild the agent entries from the chain and ignore everything else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nugget_core::Result;
use nugget_policy::{ActionContext, ActionDescriptor, LogEvent, RedactedAction};
use serde_json::Value;

use crate::log::{ActionLog, LogEntry, Record};

/// `LogEvent::External::source` of agent entries.
pub const AGENT_SOURCE: &str = "agent";

/// [`ActionLog`] adapter over a shared [`nugget_policy::ActionLog`]. Cheap to share.
#[derive(Debug)]
pub struct ChainedLog {
    chain: Arc<nugget_policy::ActionLog>,
    /// Descriptors of allowed actions awaiting their `executed` entry, by action id.
    in_flight: Mutex<HashMap<String, ActionDescriptor>>,
}

impl ChainedLog {
    pub fn new(chain: Arc<nugget_policy::ActionLog>) -> Self {
        Self {
            chain,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// The underlying chain.
    pub fn chain(&self) -> &Arc<nugget_policy::ActionLog> {
        &self.chain
    }

    /// Every agent entry in the chain, in chain order. Entries that do not parse are skipped
    /// (with a warning); the chain itself is verified by the policy crate.
    pub fn entries(&self) -> Vec<LogEntry> {
        agent_entries(&self.chain.entries())
    }

    /// The agent entries of one task, in order.
    pub fn for_task(&self, task_id: &str) -> Vec<LogEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.task_id == task_id)
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ActionDescriptor>> {
        self.in_flight.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The agent entries among policy-log entries (for callers holding an exported chain).
pub fn agent_entries(chain: &[nugget_policy::LogEntry]) -> Vec<LogEntry> {
    chain
        .iter()
        .filter_map(|e| match &e.event {
            LogEvent::External {
                source, detail, ..
            } if source == AGENT_SOURCE => match serde_json::from_value(detail.clone()) {
                Ok(entry) => Some(entry),
                Err(err) => {
                    tracing::warn!(seq = e.seq, error = %err, "unreadable agent entry in the action log");
                    None
                }
            },
            _ => None,
        })
        .collect()
}

fn redact(action: &ActionDescriptor) -> ActionDescriptor {
    let r = RedactedAction::new(action, &ActionContext::default());
    ActionDescriptor {
        target_app: r.target_app,
        verb: r.verb,
        target_label: r.target_label,
        text_preview: r.text_preview,
        declared_risk: r.declared_risk,
        task_id: r.task_id,
    }
}

fn kind_of(detail: &Value) -> String {
    detail
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

impl ActionLog for ChainedLog {
    fn append(&self, entry: &LogEntry) -> Result<()> {
        let mut entry = entry.clone();
        let mut executed = None;
        match &mut entry.record {
            Record::Decision {
                action: Some(action),
                decision,
                action_id,
                ..
            } => {
                let raw = action.clone();
                *action = redact(&raw);
                if decision.executes() {
                    self.lock().insert(action_id.clone(), raw);
                }
            }
            Record::Executed {
                action_id,
                tool,
                is_error,
                interrupted,
                ..
            } => {
                if let Some(action) = self.lock().remove(action_id.as_str()) {
                    let detail = if *interrupted {
                        format!("{tool} {action_id} (interrupted)")
                    } else {
                        format!("{tool} {action_id}")
                    };
                    executed = Some((action, !*is_error && !*interrupted, detail));
                }
            }
            Record::Finished { .. } => {
                let task = entry.task_id.clone();
                self.lock().retain(|_, a| a.task_id != task);
            }
            _ => {}
        }
        let detail = serde_json::to_value(&entry)?;
        self.chain.append(LogEvent::External {
            source: AGENT_SOURCE.into(),
            kind: kind_of(&detail),
            detail,
        })?;
        if let Some((action, ok, detail)) = executed {
            self.chain
                .record_execution(&action, &ActionContext::default(), ok, Some(detail))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Verification;
    use crate::types::{GateOutcome, Outcome, Usage};
    use nugget_policy::RiskClass;
    use serde_json::json;

    fn entry(task: &str, seq: u64, record: Record) -> LogEntry {
        LogEntry {
            task_id: task.into(),
            seq,
            at_ms: 1_000 + seq,
            record,
        }
    }

    fn action(task: &str, preview: &str) -> ActionDescriptor {
        ActionDescriptor {
            target_app: "mail".into(),
            verb: "send".into(),
            target_label: Some("Password".into()),
            text_preview: Some(preview.into()),
            declared_risk: RiskClass::Send,
            task_id: task.into(),
        }
    }

    fn run(task: &str) -> Vec<LogEntry> {
        vec![
            entry(
                task,
                0,
                Record::RunStarted {
                    goal: "send it".into(),
                    tools: vec!["finish".into()],
                    max_steps: 3,
                    deadline_ms: 1000,
                    max_tokens: 100,
                },
            ),
            entry(
                task,
                1,
                Record::Decision {
                    step: 1,
                    action_id: format!("act_{task}"),
                    call_id: "c1".into(),
                    tool: "mail.send".into(),
                    args: json!({"to": "a@example.com"}),
                    risk: RiskClass::Send,
                    action: Some(action(task, "hunter2")),
                    decision: GateOutcome::Approved {
                        reason: "asked".into(),
                    },
                    forced_by_injection: false,
                    duration_ms: 5,
                },
            ),
            entry(
                task,
                2,
                Record::Executed {
                    step: 1,
                    action_id: format!("act_{task}"),
                    tool: "mail.send".into(),
                    is_error: false,
                    result: "sent".into(),
                    verification: Some(Verification::Passed("in sent folder".into())),
                    interrupted: false,
                    duration_ms: 7,
                },
            ),
            entry(
                task,
                3,
                Record::Finished {
                    outcome: Outcome::Succeeded,
                    claim: None,
                    summary: Some("done".into()),
                    steps: 1,
                    usage: Usage::default(),
                    elapsed_ms: 20,
                },
            ),
        ]
    }

    #[test]
    fn entries_round_trip_and_the_chain_verifies() {
        let chain = Arc::new(nugget_policy::ActionLog::in_memory());
        let log = ChainedLog::new(chain.clone());
        chain
            .append(LogEvent::External {
                source: "other".into(),
                kind: "run_started".into(),
                detail: json!({"task_id": "x"}),
            })
            .unwrap();
        let a = run("a");
        let b = run("b");
        for e in a.iter().zip(&b).flat_map(|(x, y)| [x, y]) {
            log.append(e).unwrap();
        }
        chain.append(LogEvent::Halted).unwrap();
        chain.verify_chain().unwrap();

        let back = log.for_task("a");
        assert_eq!(back.len(), a.len());
        // Everything but the decision's descriptor round-trips verbatim.
        for (got, want) in back.iter().zip(&a) {
            if !matches!(want.record, Record::Decision { .. }) {
                assert_eq!(got, want);
            }
        }
        let Record::Decision { action, .. } = &back[1].record else {
            panic!("not a decision: {:?}", back[1]);
        };
        let action = action.as_ref().unwrap();
        assert_eq!(action.verb, "send");
        assert_ne!(action.text_preview.as_deref(), Some("hunter2"));
        assert!(crate::log::check_complete(&back).is_empty());
        assert_eq!(log.entries().len(), a.len() + b.len());

        // Kinds name the record; executed actions are recorded for the policy view too.
        let kinds: Vec<String> = chain
            .entries()
            .iter()
            .filter_map(|e| match &e.event {
                LogEvent::External { source, kind, .. } if source == AGENT_SOURCE => {
                    Some(kind.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(&kinds[..2], ["run_started", "run_started"]);
        let executions = chain
            .entries()
            .iter()
            .filter(|e| matches!(&e.event, LogEvent::Executed { ok: true, .. }))
            .count();
        assert_eq!(executions, 2);
        assert!(!format!("{:?}", chain.entries()).contains("hunter2"));
    }

    #[test]
    fn a_persisted_chain_reads_back_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.jsonl");
        let a = run("a");
        {
            let log = ChainedLog::new(Arc::new(nugget_policy::ActionLog::open(&path).unwrap()));
            for e in &a[..2] {
                log.append(e).unwrap();
            }
        }
        let reopened = Arc::new(nugget_policy::ActionLog::open(&path).unwrap());
        let log = ChainedLog::new(reopened);
        let back = log.for_task("a");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], a[0]);
        assert!(log.for_task("b").is_empty());
    }
}
