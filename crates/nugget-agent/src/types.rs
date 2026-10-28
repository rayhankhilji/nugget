//! Run outcomes and the small value types shared by the loop, the log and the event stream.

use std::fmt;

use serde::{Deserialize, Serialize};

/// How a run ended. Success is only ever [`Outcome::Succeeded`], which requires every mutating
/// action to have passed verification; the model's own claim is never enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    /// `finish(success)` and every change was verified (or there were no changes).
    Succeeded,
    /// `finish(success)`, but some changes could not be verified. The string says which.
    Unverified(String),
    /// `finish(partial)` with no failed verification.
    Partial,
    /// `finish(failure)`, a failed verification the model did not fix, or too many failures.
    Failed(String),
    /// Suspended by `ask_user`; resume with the answer.
    NeedsInput(String),
    /// The global kill switch was engaged.
    Halted,
    /// The run's cancellation token fired.
    Cancelled,
    BudgetExhausted(Budget),
    /// The model or provider declined.
    Refused(String),
    /// A provider or action-log failure.
    Error(String),
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::Unverified(_) => "unverified",
            Outcome::Partial => "partial",
            Outcome::Failed(_) => "failed",
            Outcome::NeedsInput(_) => "needs_input",
            Outcome::Halted => "halted",
            Outcome::Cancelled => "cancelled",
            Outcome::BudgetExhausted(_) => "budget_exhausted",
            Outcome::Refused(_) => "refused",
            Outcome::Error(_) => "error",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Unverified(d)
            | Outcome::Failed(d)
            | Outcome::NeedsInput(d)
            | Outcome::Refused(d)
            | Outcome::Error(d) => write!(f, "{}: {d}", self.label()),
            Outcome::BudgetExhausted(b) => write!(f, "{}: {b}", self.label()),
            _ => f.write_str(self.label()),
        }
    }
}

/// The budget a run ran out of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "budget", rename_all = "snake_case")]
pub enum Budget {
    Steps {
        limit: u32,
    },
    Deadline {
        limit_ms: u64,
    },
    Tokens {
        limit: u64,
    },
    ToolRate {
        tool: String,
        max_calls: u32,
        window_ms: u64,
    },
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Budget::Steps { limit } => write!(f, "step limit ({limit})"),
            Budget::Deadline { limit_ms } => write!(f, "deadline ({limit_ms} ms)"),
            Budget::Tokens { limit } => write!(f, "token limit ({limit})"),
            Budget::ToolRate {
                tool,
                max_calls,
                window_ms,
            } => write!(f, "rate limit for {tool} ({max_calls} per {window_ms} ms)"),
        }
    }
}

/// Model tokens spent. Estimated (4 characters per token) when a provider reports none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// What happened at the policy gate (and confirmation) for one proposed call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateOutcome {
    /// The policy allowed it.
    Allowed,
    /// The policy (or the injection guard) asked; the user approved.
    Approved {
        reason: String,
    },
    /// The user declined.
    Rejected {
        reason: String,
    },
    /// Nobody answered in time; treated as a denial.
    TimedOut {
        reason: String,
    },
    Denied {
        reason: String,
    },
    /// Arguments failed the tool's schema; not executed.
    InvalidArgs {
        reason: String,
    },
    UnknownTool,
    /// The action id was already in the ledger: the recorded result was returned, nothing ran.
    Replayed,
}

impl GateOutcome {
    /// Whether the action runs.
    pub fn executes(&self) -> bool {
        matches!(self, GateOutcome::Allowed | GateOutcome::Approved { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            GateOutcome::Allowed => "allow",
            GateOutcome::Approved { .. } => "confirmed",
            GateOutcome::Rejected { .. } => "rejected by user",
            GateOutcome::TimedOut { .. } => "confirmation timed out",
            GateOutcome::Denied { .. } => "DENY",
            GateOutcome::InvalidArgs { .. } => "invalid args",
            GateOutcome::UnknownTool => "unknown tool",
            GateOutcome::Replayed => "replayed",
        }
    }
}

/// The user's answer to a confirmation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmAnswer {
    Approved,
    Rejected,
    TimedOut,
}
