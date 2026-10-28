//! `nugget-knowledge`: turns indexed items into a typed, cited knowledge graph (M2-B).
//!
//! # Pipeline
//! [`Knowledge::run_once`] works through a queue of changed items:
//! 1. **Work queue.** Each source's item keys and versions are diffed against a ledger kept in
//!    the store's cursor table (see [`ItemFeed`]). New and changed items are extracted; items
//!    gone from the store are retracted. Progress is persisted as items complete, so runs are
//!    resumable, and re-processing is idempotent: an item's old evidence is retracted
//!    (`GraphStore::retract_item`) before it is extracted again, and claims that come back
//!    keep their ids.
//! 2. **Deterministic extractors** (always on): mail headers (people by address, organisations
//!    by corporate domain, a penalised `works_at`, `corresponded_with` from the user), calendar
//!    events (event entity with time attributes, `attended` / `invited_to` / `organized`, the
//!    location as a place), Markdown (topics and projects from tags and frontmatter, wikilinks as
//!    `references`), and contacts (vCards and contact records).
//! 3. **LLM extractor** (optional, [`llm`]): `Tier::Synthesis` with a JSON schema. Local
//!    providers only by default; every claim must carry a quote found verbatim in the text.
//! 4. **Entity resolution** (`resolve`): identifiers, then names and aliases,
//!    then (optionally) embeddings and LLM adjudication; never across conflicting identifiers.
//! 5. **Claim lifecycle:** single-valued predicates (`works_at`, `lives_in`, ...) keep one
//!    active value, the one with the freshest evidence; confidence rises with independent
//!    evidence (noisy-OR).
//!
//! Every claim carries [`Evidence`](nugget_graph::Evidence) `{ item, chunk_id when known,
//! quote }`, and every entity occurrence is recorded as a mention.
//!
//! # The user
//! The user is a person entity (attribute `is_me: true`) identified by their addresses: from
//! [`MeConfig::emails`], or learned from mail in a sent folder ([`MeConfig::infer_from_sent`]).
//! A person found to hold one of those addresses is merged into the user's entity.

mod config;
mod extract;
mod feed;
mod knowledge;
pub mod llm;
mod resolve;
mod session;
mod spec;
mod state;
mod text;

pub use config::{KnowledgeConfig, LlmConfig, MeConfig, ResolutionConfig, RunBudget};
pub use feed::ItemFeed;
pub use knowledge::{Knowledge, KnowledgeStatus, RunStats};
pub use session::SINGLE_VALUED as SINGLE_VALUED_PREDICATES;
pub use state::EXTRACTOR_VERSION;
