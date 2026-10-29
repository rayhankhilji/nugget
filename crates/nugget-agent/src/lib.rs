//! Nugget's own agent loop for computer use and multi-step tasks (M4).
//!
//! - [`Tool`] / [`ToolRegistry`]: pluggable tools with a JSON schema, a declared
//!   [`RiskClass`](nugget_policy::RiskClass), and optional before/after observation plus
//!   [`Tool::verify`]. Built-ins: `finish`, `ask_user`, `note` ([`ToolRegistry::without`] drops
//!   one, e.g. `note` for a small model). Computer-use and browser tools come from other crates.
//! - [`Agent::run`]: system prompt and goal, then model turns with tools. The system prompt is
//!   short, written for small models, and identical across runs with the same config (the
//!   per-run fence tag is named in the first user message), so a local server can reuse the
//!   system-plus-tools prefix from its prompt cache. Calls with an empty or reused id get a fresh,
//!   deterministic one before they enter the history. Each call is validated,
//!   given an idempotent action id ([`ledger`]), checked against the
//!   [`PolicyGate`](nugget_policy::PolicyGate) (Allow / Confirm through [`Confirmations`] with a
//!   timeout that counts as a denial / Deny back to the model), executed, and verified when it
//!   mutates (`Edit` and above). Budgets on steps, wall clock, tokens and per-tool rate; the kill
//!   switch and a cancellation token are checked before every step and action and while waiting.
//! - "Done" means verified: `finish` takes `done`, `partial`, `cannot` or `refused` (older
//!   `success` / `failure` are accepted as aliases), and `done` is reported as
//!   [`Outcome::Succeeded`] only when every mutation passed verification; otherwise
//!   [`Outcome::Unverified`] or [`Outcome::Failed`]. `refused` is [`Outcome::Refused`].
//! - Prompt-injection hardening ([`injection`]): untrusted tool output is fenced as data, the
//!   prompt says so, and a detector forces a confirmation on the next risky action.
//! - [`ActionLog`] (append-only; [`MemoryLog`], [`JsonlLog`], and [`ChainedLog`] on the policy
//!   crate's hash-chained log) with [`explain`] for a readable
//!   report, and an [`AgentEvent`] stream for the UI.

mod agent;
pub mod chained;
pub mod events;
pub mod injection;
pub mod ledger;
pub mod log;
pub mod tool;
mod types;

pub use agent::{
    ActionRecord, Agent, AgentConfig, AgentRun, Budgets, FEATURE_AGENT, Model, MutationRecord,
    PendingAsk, RunState, Task, ToolRate, VerificationSummary,
};
pub use chained::{AGENT_SOURCE, ChainedLog, agent_entries};
pub use events::{AgentEvent, ConfirmationRequest, Confirmations, EventSink, NoEvents};
pub use injection::{HeuristicDetector, InjectionDetector, InjectionFinding};
pub use ledger::{ActionLedger, LedgerEntry, MemoryLedger, action_id};
pub use log::{ActionLog, JsonlLog, LogEntry, MemoryLog, Record, check_complete, explain};
pub use tool::{
    ActionTarget, Claim, Control, Snapshot, Tool, ToolCtx, ToolRegistry, ToolResult, Verification,
};
pub use types::{Budget, ConfirmAnswer, GateOutcome, Outcome, Usage};
