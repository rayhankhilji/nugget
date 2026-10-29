//! Double Tap drafting engine (daemon side).
//!
//! **Contract (M3):** [`FieldKind`] and [`classify()`]. Field kinds are decided here by rules,
//! never by the model (the [`classify`](mod@classify) module documents the rule order), and secure
//! fields and excluded apps are refused first ([`gate`]).
//!
//! [`Drafter`] is the engine:
//! - **Provider first.** The request's provider, else the user's choice for the app
//!   ([`DraftConfig::app_providers`]), else the Synthesis tier with fallback, mirroring
//!   `retrieve.ask`: what may be sent depends on where it goes.
//! - **Budgeted, cited context.** Retrieval from the thread, subject and recipients, packed into
//!   [`DraftConfig::budget_tokens`] (1,500 by default), with sensitive items withheld from `Cloud`
//!   providers and the cited items passed to the egress ledger (`ChatRequest.items`, feature
//!   `draft`). No whole-vault dumps, no silent truncation.
//! - **Local budget.** Non-cloud providers, whose prompt reading dominates latency, get a
//!   smaller prompt ([`LocalBudget`], on by default): 300 retrieval tokens, the best 3 passages
//!   (citations and egress items trimmed to match), [`PromptLimits::local`] caps and a terser
//!   voice block. Cloud prompts are unchanged.
//! - **Voice.** Style stats and exemplars from a [`VoiceSource`], in a stable system prefix
//!   (cache-friendly), with the thread and field text fenced as untrusted data ([`prompt`]).
//! - **Streaming post-processing** ([`post`]): preambles, wrapping quotes, duplicate greetings
//!   and sign-offs, citation markers and (for chat and email) markdown are stripped; the draft
//!   is cut to the voice's length band; nothing emitted is ever retracted and no trailing newline
//!   is ever emitted.
//! - **Latency.** [`Drafter::prewarm`] starts provider warm-up, retrieval and the voice lookup on
//!   trigger; for non-cloud providers it also warms the prompt cache with the exact system
//!   prefix the draft will send ([`Warmer::warm_prefix`], llama-server `n_predict: 0`), and
//!   [`Drafter::prewarm_idle`] caches the rules ahead of any trigger. Non-cloud draft requests
//!   carry a `draft:<channel>` cache hint. [`DraftTimings`] records retrieval, provider open,
//!   first token, total, and prompt and cached prompt tokens.
//! - **Cancellation** ([`Drafter::cancel`], [`DraftStream::cancel`]) and **feedback**
//!   ([`Drafter::feedback`]: an audit event plus the texts to a [`FeedbackSink`]).
//! - **Adapters** to `nugget-voice`: [`ProfileVoice`] (a profile as the engine's
//!   [`VoiceSource`]) and [`VoiceFeedback`] (the engine's [`FeedbackSink`] over the voice store
//!   sink), each running the synchronous call on a blocking thread.

pub mod classify;
mod engine;
pub mod post;
pub mod prompt;
mod voice;

pub use classify::{
    DEFAULT_EXCLUDED_APPS, DEFAULT_TERMINAL_APPS, Refusal, classify, gate, gate_with, is_secure,
    is_terminal,
};
pub use engine::{
    DraftConfig, DraftStream, DraftTimings, Drafter, FeedbackOutcome, FeedbackRecord, FeedbackSink,
    LocalBudget, MAX_BUDGET_TOKENS, NoVoice, Prewarm, RetrievalRunner, VoiceQuery, VoiceSource,
    Warmer, post_options, retrieval_query,
};
pub use prompt::{FEATURE_DRAFT, LengthBand, PromptLimits};
pub use voice::{ProfileVoice, VoiceFeedback, feedback_parts, profile_query};

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
