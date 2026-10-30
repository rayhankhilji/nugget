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
//!   Too many failed verifications in a row stop the run; a mutation whose tool returned an error
//!   (it refused before acting) is noted to the model but does not count towards that streak.
//! - Opt-in triage ([`AgentConfig::triage`]): before the first turn of a fresh run (not on
//!   resume), one short tool-less call with a fixed system text judges the goal and trusted
//!   context alone against a `proceed` / `refuse` JSON schema. `refuse` ends the run as
//!   [`Outcome::Refused`] with nothing done; an error or unparsable reply falls through to the
//!   normal loop. Its tokens count towards the budget; it is not a step.
//! - "Done" means verified: `finish` takes `done`, `partial`, `cannot` or `refused` (older
//!   `success` / `failure` are accepted as aliases), and `done` is reported as
//!   [`Outcome::Succeeded`] only when every mutation passed verification; otherwise
//!   [`Outcome::Unverified`] or [`Outcome::Failed`]. `refused` is [`Outcome::Refused`].
//! - Ask, don't guess ([`AgentConfig::clarify`], on by default; [`clarify`]): when a tool's
//!   [`ToolResult::targets`] show more than one item the goal could mean (two people named Sam),
//!   the next action of `Edit` risk or higher is held back, never run, and the run suspends with
//!   [`Outcome::NeedsInput`] and the candidates as options; the answer goes back with a note that
//!   the action was not performed. An `ask_user` the model makes meanwhile gets the candidates too.
//! - Prompt-injection hardening ([`injection`]): untrusted tool output is fenced as data, the
//!   prompt says so, and a detector forces a confirmation on the next risky action.
//! - [`ActionLog`] (append-only; [`MemoryLog`], [`JsonlLog`], and [`ChainedLog`] on the policy
//!   crate's hash-chained log) with [`explain`] for a readable
//!   report, and an [`AgentEvent`] stream for the UI.

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
    PendingAsk, RunState, Task, ToolRate, TriageReport, TriageVerdict, VerificationSummary,
};
pub use chained::{AGENT_SOURCE, ChainedLog, agent_entries};
pub use clarify::{Ambiguity, ambiguity};
pub use events::{AgentEvent, ConfirmationRequest, Confirmations, EventSink, NoEvents};
pub use injection::{HeuristicDetector, InjectionDetector, InjectionFinding};
pub use ledger::{ActionLedger, LedgerEntry, MemoryLedger, action_id};
pub use log::{ActionLog, JsonlLog, LogEntry, MemoryLog, Record, check_complete, explain};
pub use tool::{
    ActionTarget, Claim, Control, Snapshot, Tool, ToolCtx, ToolRegistry, ToolResult, Verification,
};
pub use types::{Budget, ConfirmAnswer, GateOutcome, Outcome, Usage};
