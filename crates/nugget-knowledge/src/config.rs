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
    /// Predicates that hold one value at a time: the value seen most recently (and not said to
    /// have ended) is current, the others are superseded history (see the crate docs, "Claim
    /// lifecycle"). Replaces the built-in list ([`crate::SINGLE_VALUED_PREDICATES`]) when
    /// non-empty.
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
    /// Per-request timeout in milliseconds. For a local provider this is the floor: the
    /// timeout is derived from the request size (see [`LlmConfig::request_timeout`]).
    pub timeout_ms: u64,
    /// Upper bound on the confidence of an LLM-extracted claim.
    pub max_confidence: f32,
    /// Give up on an item after this many failed attempts (until it changes).
    pub max_attempts: u32,
    /// Entities a provider may return per request (the schema's `maxItems`).
    pub max_entities_per_request: usize,
    /// Claims a provider may return per request (the schema's `maxItems`).
    pub max_claims_per_request: usize,
    /// Tighter limits for when the item's first-choice provider is local (`OnDevice` /
    /// `LocalNetwork`): each applies as the smaller of it and the general limit above.
    pub local: LocalLlmLimits,
}

/// Per-request limits for a local provider. A small model on a CPU generates a few tokens per
/// second and reads a few tens, so requests are kept short: a chunk or two in, a capped number
/// of entities and claims out. Each request then finishes well inside the timeout, and the
/// per-item total stays bounded by `max_requests_per_item * max_input_chars`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalLlmLimits {
    /// Characters of chunk text per request (≈ 4 characters per token).
    pub max_input_chars: usize,
    /// Chunks per request at most.
    pub max_chunks_per_request: usize,
    /// Requests per item at most; the rest of a long item is not sent.
    pub max_requests_per_item: usize,
    /// Output token cap per request.
    pub max_output_tokens: u32,
    /// Entities per request (the schema's `maxItems`).
    pub max_entities_per_request: usize,
    /// Claims per request (the schema's `maxItems`).
    pub max_claims_per_request: usize,
    /// Generation speed assumed when deriving a request's timeout (tokens per second;
    /// conservative: a 4B model on a 4-core CPU).
    pub output_tokens_per_second: f32,
    /// Prompt-reading speed assumed when deriving a request's timeout (tokens per second).
    pub prompt_tokens_per_second: f32,
    /// Upper bound of a derived timeout in milliseconds (`timeout_ms` wins if larger).
    pub max_timeout_ms: u64,
}

impl Default for LocalLlmLimits {
    fn default() -> Self {
        Self {
            max_input_chars: 4_000,
            max_chunks_per_request: 2,
            max_requests_per_item: 4,
            max_output_tokens: 1_024,
            max_entities_per_request: 12,
            max_claims_per_request: 10,
            output_tokens_per_second: 6.0,
            prompt_tokens_per_second: 40.0,
            max_timeout_ms: 300_000,
        }
    }
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
            max_entities_per_request: 32,
            max_claims_per_request: 48,
            local: LocalLlmLimits::default(),
        }
    }
}

/// Fixed allowance per local request (connection, queueing behind another request), in
/// milliseconds.
const LOCAL_OVERHEAD_MS: f64 = 10_000.0;

impl LlmConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.max(1))
    }

    /// The timeout of one extraction request (QA GG5-18). A cloud provider gets
    /// [`timeout_ms`](Self::timeout_ms). A local one gets the time a slow CPU needs to read
    /// the prompt (`prompt_chars` / 4 tokens at `local.prompt_tokens_per_second`) and to write
    /// the whole output cap (`max_output_tokens` at `local.output_tokens_per_second`), plus a
    /// fixed allowance; at least `timeout_ms`, at most `local.max_timeout_ms` (or `timeout_ms`
    /// when that is larger). With the defaults: about 230 s for a full local request.
    pub fn request_timeout(
        &self,
        local: bool,
        prompt_chars: usize,
        max_output_tokens: u32,
    ) -> Duration {
        let floor = self.timeout_ms.max(1);
        if !local {
            return Duration::from_millis(floor);
        }
        let l = &self.local;
        let read = (prompt_chars as f64 / 4.0) / f64::from(l.prompt_tokens_per_second.max(0.1));
        let write = f64::from(max_output_tokens) / f64::from(l.output_tokens_per_second.max(0.1));
        let est = ((read + write) * 1000.0 + LOCAL_OVERHEAD_MS).ceil() as u64;
        Duration::from_millis(est.clamp(floor, l.max_timeout_ms.max(floor)))
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
