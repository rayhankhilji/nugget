//! Configuration of the knowledge pipeline (serde, so the daemon can load it from TOML).

use std::time::Duration;

use nugget_graph::EntityKind;
use serde::{Deserialize, Serialize};

/// Everything [`Knowledge`](crate::Knowledge) can be configured with. `Default` is the
/// conservative, local-only setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    /// Who the user ("me") is.
    pub me: MeConfig,
    /// Extra webmail / consumer mail domains (added to the built-in list). Addresses at these
    /// domains never produce organisations or `works_at` claims.
    pub extra_freemail_domains: Vec<String>,
    /// Confidence of a `works_at` claim inferred from a corporate mail domain (a penalty: the
    /// address may be a contractor's, an alias or a shared mailbox).
    pub mail_works_at_confidence: f32,
    /// Messages with more participants than this produce no `corresponded_with` claims for the
    /// recipients (mailing lists, announcements); the sender is still linked.
    pub max_correspondents: usize,
    /// Predicates that hold one value at a time: a fresher claim supersedes older ones with a
    /// different object. Replaces the built-in list when non-empty.
    pub single_valued_predicates: Vec<String>,
    /// The optional LLM extractor.
    pub llm: LlmConfig,
    /// Entity-resolution options beyond exact identifiers and names.
    pub resolution: ResolutionConfig,
    /// Persist work-queue progress at least every this many processed items.
    pub flush_every: usize,
    /// Chunks read per item at most (longer items are cut for extraction).
    pub max_chunks_per_item: usize,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            me: MeConfig::default(),
            extra_freemail_domains: Vec::new(),
            mail_works_at_confidence: 0.5,
            max_correspondents: 20,
            single_valued_predicates: Vec::new(),
            llm: LlmConfig::default(),
            resolution: ResolutionConfig::default(),
            flush_every: 64,
            max_chunks_per_item: 512,
        }
    }
}

/// The user. Their addresses identify the special "me" person, which `corresponded_with`,
/// `attended` and similar claims hang off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeConfig {
    /// Display name of the "me" entity. Default: inferred from sent mail, else `Me`.
    pub name: Option<String>,
    /// The user's own mail addresses.
    pub emails: Vec<String>,
    /// Learn further addresses from mail in a sent folder (IMAP folder names like `Sent`,
    /// Gmail's `SENT` label): its `From` address is the user's.
    pub infer_from_sent: bool,
}

impl Default for MeConfig {
    fn default() -> Self {
        Self {
            name: None,
            emails: Vec::new(),
            infer_from_sent: true,
        }
    }
}

/// The LLM extractor (`Tier::Synthesis`).
///
/// **Privacy:** only `OnDevice` / `LocalNetwork` providers are used, except that a `Cloud`
/// provider may be used for an item whose source is listed in [`cloud_sources`](Self::cloud_sources)
/// **and** which is not sensitive. Sensitive items (or items whose sensitivity is unknown) never
/// reach a cloud provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    /// Source ids (exact) whose non-sensitive items may be sent to a cloud provider.
    pub cloud_sources: Vec<String>,
    /// Characters of chunk text per request (≈ 4 characters per token).
    pub max_input_chars: usize,
    /// Requests per item at most; the rest of a long item is not sent.
    pub max_requests_per_item: usize,
    /// Output token cap per request.
    pub max_output_tokens: u32,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Upper bound on the confidence of an LLM-extracted claim.
    pub max_confidence: f32,
    /// Give up on an item after this many failed attempts (until it changes).
    pub max_attempts: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cloud_sources: Vec::new(),
            max_input_chars: 12_000,
            max_requests_per_item: 4,
            max_output_tokens: 2_048,
            timeout_ms: 120_000,
            max_confidence: 0.85,
            max_attempts: 3,
        }
    }
}

impl LlmConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.max(1))
    }
}

/// Resolution beyond identifiers and exact names. Both steps are off by default and only ever
/// use local providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResolutionConfig {
    /// Compare names by embedding similarity (`Tier::Embedding`, local providers only).
    pub embedding: bool,
    /// Cosine similarity at or above which two names are the same entity.
    pub embedding_threshold: f32,
    /// Kinds the embedding check applies to.
    pub embedding_kinds: Vec<EntityKind>,
    /// Ask a local `Tier::Synthesis` model about ambiguous pairs (similarity in
    /// `[adjudicate_above, embedding_threshold)`).
    pub adjudicate: bool,
    pub adjudicate_above: f32,
    /// Candidates compared per entity at most.
    pub max_candidates: usize,
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            embedding: false,
            embedding_threshold: 0.95,
            embedding_kinds: vec![
                EntityKind::Organization,
                EntityKind::Project,
                EntityKind::Topic,
                EntityKind::Place,
            ],
            adjudicate: false,
            adjudicate_above: 0.85,
            max_candidates: 20,
        }
    }
}

/// Limits for one [`Knowledge::run_once`](crate::Knowledge::run_once) call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunBudget {
    /// Changed or deleted items processed at most.
    pub max_items: usize,
    /// LLM extraction requests at most.
    pub max_llm_requests: usize,
    /// Wall-clock limit in milliseconds (checked between items). `None`: no limit.
    pub max_millis: Option<u64>,
}

impl Default for RunBudget {
    fn default() -> Self {
        Self {
            max_items: 500,
            max_llm_requests: 50,
            max_millis: None,
        }
    }
}

impl RunBudget {
    /// No limits.
    pub fn unlimited() -> Self {
        Self {
            max_items: usize::MAX,
            max_llm_requests: usize::MAX,
            max_millis: None,
        }
    }

    pub fn items(n: usize) -> Self {
        Self {
            max_items: n,
            ..Self::unlimited()
        }
    }
}
