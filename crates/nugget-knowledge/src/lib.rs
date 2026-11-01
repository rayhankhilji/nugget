//! `nugget-knowledge`: turns indexed items into a typed, cited knowledge graph.
//!
//! [`Knowledge::run_once`] works through the store's item change log (a full diff against the
//! ledger after a reset or extractor upgrade), runs the deterministic extractors (mail, calendar,
//! Markdown, contacts) and the optional quote-gated [`llm`] extractor, resolves entities (never
//! across conflicting identifiers; short names join only when corroborated), and keeps claims
//! time-aware: single-valued predicates ([`SINGLE_VALUED_PREDICATES`]) are settled per
//! (subject, predicate) as a deterministic function of their evidence, so item order never
//! changes the outcome; user-asserted values stay current and rejected ones stay rejected.
//! Every claim carries [`Evidence`](nugget_graph::Evidence). The user is the person entity with
//! `is_me: true`, identified by [`MeConfig::emails`] or learned from sent mail.

mod config;
mod extract;
mod feed;
mod knowledge;
pub mod llm;
mod names;
mod resolve;
mod session;
mod spec;
mod splits;
mod state;
mod temporal;
mod text;

/// Lock a mutex, recovering the data from a poisoned one.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub use config::{
    KnowledgeConfig, LlmConfig, LocalLlmLimits, MeConfig, ResolutionConfig, RunBudget,
};
pub use feed::ItemFeed;
pub use knowledge::{Knowledge, KnowledgeStatus, RunStats};
pub use session::SINGLE_VALUED as SINGLE_VALUED_PREDICATES;
pub use state::EXTRACTOR_VERSION;
