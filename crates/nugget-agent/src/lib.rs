//! Heed's own agent loop for computer use and multi-step tasks.
//!
//! [`Agent::run`] drives model turns with [`Tool`]s: each call is validated, given an idempotent
//! action id ([`ledger`]), checked against the [`PolicyGate`](nugget_policy::PolicyGate) (with
//! user [`Confirmations`]), executed and, when it mutates, verified. "Done" means verified
//! ([`Outcome`]). Budgets, the kill switch and cancellation are checked before every step and
//! action. Optional triage judges the goal before the first turn; [`clarify`] asks the user
//! instead of guessing an ambiguous target; [`injection`] fences untrusted output and forces a
//! confirmation after a suspected prompt injection. Every step is written to an [`ActionLog`]
//! ([`explain`] renders it) and emitted as an [`AgentEvent`].

mod agent;
pub mod chained;
pub mod clarify;
pub mod events;
pub mod injection;
pub mod ledger;
pub mod log;
pub mod tool;
mod types;

pub use agent::{
    ActionRecord, Agent, AgentConfig, AgentRun, Budgets, FEATURE_AGENT, Model, MutationRecord,
    PendingAsk, Progress, RunState, TRIAGE_CANCEL, TRIAGE_GO_AHEAD, Task, ToolRate, TriageReport,
    TriageVerdict, VerificationSummary,
};
pub use chained::{AGENT_SOURCE, ChainedLog, agent_entries};
pub use clarify::{Ambiguity, ambiguity};
pub use events::{AgentEvent, Answered, ConfirmationRequest, Confirmations, EventSink, NoEvents};
pub use injection::{HeuristicDetector, InjectionDetector, InjectionFinding};
pub use ledger::{ActionLedger, LedgerEntry, MemoryLedger, action_id};
pub use log::{ActionLog, JsonlLog, LogEntry, MemoryLog, Record, check_complete, explain};
pub use tool::{
    ActionTarget, Claim, Control, Snapshot, Tool, ToolCtx, ToolRegistry, ToolResult, Verification,
};
pub use types::{Budget, ConfirmAnswer, GateOutcome, Outcome, Usage};
