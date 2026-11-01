//! Double Tap drafting engine (daemon side): rule-based field classification and gating
//! ([`classify`](mod@classify)), provider choice, budgeted retrieval with sensitive items kept
//! from cloud providers, voice, fenced prompts ([`prompt`]), streaming post-processing
//! ([`post`]) and grounding ([`grounding`]), prewarm and read-ahead, cancellation and feedback.

pub mod classify;
mod engine;
pub mod grounding;
pub mod post;
pub mod prompt;
pub mod prompt_facts;
mod voice;
mod warm;

pub use classify::{
    DEFAULT_EXCLUDED_APPS, DEFAULT_TERMINAL_APPS, Refusal, classify, gate, gate_with, is_secure,
    is_terminal,
};
pub use engine::{
    DraftConfig, DraftStream, DraftTimings, Drafter, FeedbackOutcome, FeedbackRecord, FeedbackSink,
    LocalBudget, MAX_BUDGET_TOKENS, NoVoice, Prewarm, RetrievalRunner, SensitiveScan, VoiceQuery,
    VoiceSource, WITHHELD_TEXT, Warmer, post_options, retrieval_query, withhold_sensitive,
};
pub use grounding::{Grounder, GroundingCheck, GroundingReport};
pub use prompt::{FEATURE_DRAFT, LengthBand, PromptLimits};
pub use voice::{ProfileVoice, VoiceFeedback, feedback_parts, profile_query};
pub use warm::{ReadAheadConfig, ReadAheadOutcome, ReadAheadSkip};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    EmailBody,
    ChatMessage,
    Comment,
    Document,
    FormField,
    Search,
    Unknown,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmailBody => "email_body",
            Self::ChatMessage => "chat_message",
            Self::Comment => "comment",
            Self::Document => "document",
            Self::FormField => "form_field",
            Self::Search => "search",
            Self::Unknown => "unknown",
        }
    }

    /// Whether drafting a reply makes sense for this kind.
    pub fn draftable(self) -> bool {
        !matches!(self, Self::Search)
    }
}
