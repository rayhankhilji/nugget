//! `nugget-knowledge`: turns indexed items into a typed, cited knowledge graph (M2-B).
//!
//! # Pipeline
//! [`Knowledge::run_once`] works through a queue of changed items:
//! 1. **Work queue.** The store's item change log (`items_changed_since`, with deletion
//!    tombstones) since the last fully processed position; the first run, and any run after a
//!    reset or an extractor upgrade, diffs each source's item keys and versions against a ledger
//!    kept in the store's cursor table instead (see [`ItemFeed`]). New and changed items are
//!    extracted; items gone from the store are retracted. Progress is persisted as items complete, so runs are
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
//! 5. **Claim lifecycle** (time-aware). Every claim the pipeline writes carries `valid_from`:
//!    the date of its earliest evidence item (a mail's `Date`, a note's frontmatter date or
//!    modification time, an item's time; dates more than a day in the future are clamped), or
//!    the start the extractor knows (a calendar event's, which evidence from other items never
//!    moves). A multi-valued claim's start follows its evidence: earlier evidence lowers it, and
//!    losing the earliest evidence item raises it. Confidence rises with independent evidence
//!    (noisy-OR). Single-valued predicates ([`SINGLE_VALUED_PREDICATES`]: `works_at`,
//!    `has_title`, `lives_in`, `reports_to`, ...) keep one current value; after every item, merge
//!    or deletion that touches one, the (subject, predicate) pair is *settled* from scratch, as
//!    a deterministic function of its claims' evidence, so the order items arrive in never
//!    changes the outcome:
//!    - a quote saying the fact **ended** ("my last day at X", "left X", "moved from X",
//!      "former", "previously", "used to", "no longer", "ex-"; see `temporal`) is kept as
//!      cited evidence but never makes its value current: a value whose newest ended-quote is
//!      at least as recent as its newest other evidence is superseded, with `valid_to` that
//!      date (or the first sighting of another value after its last one, when earlier). Cues
//!      only count right before the value they name, not across a clause or a negation;
//!    - among the other values the one seen most recently is current; a tie (one note naming
//!      both) goes to the value whose quote marks a start ("joined", "now", "new role", "moved
//!      to", "from X to Y"), then to the one later in the text, then by name;
//!    - a superseded value is valid until the first date another value is seen at or after its
//!      last sighting (when the value that replaced it starts);
//!    - a user-asserted value is current whatever the evidence says (until the user changes
//!      it: newer evidence of a job change does not override it); a value the user
//!      **rejected** stays rejected and never current, even through a twin claim
//!      (re-extraction only adds evidence to it); when the current
//!      value loses its evidence (a deletion), the best remaining superseded value is current
//!      again, open-ended.
//!
//!    Graphs written before validity intervals are migrated once, without the LLM, at the start
//!    of the next run (`state::TEMPORAL_VERSION`), resumably: a run out of time keeps its place.
//!    One item's mention, claim, evidence and
//!    provenance writes are applied as one `GraphBatch` (one transaction). Entities left with
//!    nothing behind them after retractions are pruned (never the user's entity).
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
mod splits;
mod state;
mod temporal;
mod text;

pub use config::{
    KnowledgeConfig, LlmConfig, LocalLlmLimits, MeConfig, ResolutionConfig, RunBudget,
};
pub use feed::ItemFeed;
pub use knowledge::{Knowledge, KnowledgeStatus, RunStats};
pub use session::SINGLE_VALUED as SINGLE_VALUED_PREDICATES;
pub use state::EXTRACTOR_VERSION;
