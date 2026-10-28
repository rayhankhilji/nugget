//! The adapter between the benchmark and a proactive engine.
//!
//! The benchmark drives any [`ProactiveEngine`]: it feeds the world's items in day order, asks
//! for each morning's brief, reports the simulated user's feedback, and asks the engine to execute
//! an accepted card's action with the [`Approval`] the user gave. It never reaches into engine
//! internals, so the same loop scores the reference engine, the recency baseline and the adapter
//! over the real `nugget-proactive` crate ([`super::real`]).

use super::world::{CardKind, Item};

/// What the user did with a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    DraftReply,
    CreateReminder,
    CreatePrepNote,
    CreateEvent,
    ArchiveThread,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::DraftReply => "draft_reply",
            ActionKind::CreateReminder => "create_reminder",
            ActionKind::CreatePrepNote => "create_prep_note",
            ActionKind::CreateEvent => "create_event",
            ActionKind::ArchiveThread => "archive_thread",
        }
    }
}

/// What executing an action would change, shown to the user before they approve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    /// The object changed, e.g. `drafts/card-12` or `reminders/card-3`.
    pub target: String,
    pub before: String,
    pub after: String,
}

impl DryRun {
    /// A diff that shows a change: a target and different before and after.
    pub fn is_effective(&self) -> bool {
        !self.target.is_empty() && self.before != self.after
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    /// `None` means the engine offers an action it cannot preview; the benchmark counts that
    /// against dry-run coverage.
    pub dry_run: Option<DryRun>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// Unique per card instance, chosen by the engine.
    pub id: String,
    pub kind: CardKind,
    /// The world object the card is about: `thread:<id>`, `event:<id>`, `commit:<item id>` or
    /// `sub:<merchant domain>` (see [`CardKind`]). Gold labels and dedup use `(kind, subject)`.
    pub subject: String,
    pub title: String,
    pub score: f64,
    pub action: Option<Action>,
}

impl Card {
    pub fn key(&self) -> (CardKind, &str) {
        (self.kind, self.subject.as_str())
    }
}

/// The user's approval of one card's action. Issued by the benchmark only for cards the user
/// accepted; `token` is unique per approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub card_id: String,
    pub token: u64,
}

/// One executed action, as the engine's audit log records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub card_id: String,
    pub action: ActionKind,
    /// The approval token the engine executed under, if any.
    pub approval: Option<u64>,
    /// What actually changed.
    pub applied: DryRun,
}

/// A proactive engine under test.
pub trait ProactiveEngine {
    /// Short name for the report, e.g. `reference`.
    fn name(&self) -> &'static str;

    /// An item arrived. Called in day order; every item with `day < d` has been observed before
    /// [`brief`](Self::brief) for day `d`.
    fn observe(&mut self, item: &Item);

    /// The morning brief for `day`: at most `k` cards, best first. The benchmark truncates
    /// anything past `k`.
    fn brief(&mut self, day: i32, k: usize) -> Vec<Card>;

    /// The user's response to a card shown in a brief.
    fn feedback(&mut self, card_id: &str, feedback: Feedback);

    /// Execute a card's action. The engine must refuse (return `Err`) without a valid approval
    /// for exactly this card. The benchmark also calls this with no approval, and with another
    /// card's approval, to check that the engine refuses.
    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String>;

    /// Every action the engine executed, in order.
    fn audit_log(&self) -> Vec<Execution>;
}
