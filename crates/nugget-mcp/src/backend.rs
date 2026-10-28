//! The [`KnowledgeBackend`] contract: what the MCP server reads, as plain structs.
//!
//! `nuggetd` implements it in wave 2 over `nugget-retrieve` (search, context), the graph store
//! (entities, neighbourhood, change feed) and `nugget-store` (items). [`crate::FakeBackend`] is
//! the in-memory implementation used by tests.
//!
//! **Provenance is part of the contract.** Every record that carries source content says which
//! item it came from and whether that item is sensitive (`nugget_retrieve::is_sensitive` on the
//! item's metadata, which fails closed: an item without an explicit `"sensitive": false` counts as
//! sensitive). Entities, graph nodes and graph edges carry their [`Origin`]s: the distinct
//! `(source, sensitive)` pairs over all their evidence and mentions. The MCP layer uses these to
//! enforce each client's scopes on every result, even when the backend already filtered.
//!
//! Every query also receives the client's [`ScopeFilter`] so the backend can filter *before*
//! ranking and limiting (so a scoped client still gets `limit` results). Filtering there is an
//! optimisation; correctness never depends on it.

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef, SourceId};
use nugget_core::ipc::{ClaimObjectView, EntityLink};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::scope::ScopeFilter;

/// One source an entity, edge or fact draws on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Origin {
    pub source: SourceId,
    /// The contributing item is sensitive.
    pub sensitive: bool,
}

impl Origin {
    pub fn new(source: impl Into<String>, sensitive: bool) -> Self {
        Self {
            source: SourceId::new(source),
            sensitive,
        }
    }
}

/// A search hit: the best passage of one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRecord {
    pub item: ItemRef,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub snippet: String,
    pub score: f32,
    pub sensitive: bool,
}

/// One packed passage of a context, in rank order. The MCP layer numbers passages `[n]` and
/// renders the text, so passages it must drop never leave a gap in the numbering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPassage {
    pub item: ItemRef,
    /// The passage's primary chunk.
    pub chunk_id: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub sensitive: bool,
    /// Passage text (possibly several adjacent chunks), without any `[n]` header.
    pub text: String,
}

/// Passages packed under a token budget.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub passages: Vec<ContextPassage>,
    /// Relevant sensitive items the backend left out because the filter excluded them.
    #[serde(default)]
    pub excluded_sensitive: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySummaryRecord {
    pub id: EntityId,
    /// `person`, `organization`, ... (see `nugget_graph::EntityKind`).
    pub kind: String,
    pub name: String,
    pub claims: u64,
    pub mentions: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Distinct origins over all the entity's evidence and mentions. Empty means the entity
    /// exists only through vault edits (see [`crate::scope::VAULT_SOURCE`]).
    pub origins: Vec<Origin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub item: ItemRef,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    /// Short supporting quote from the source.
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub sensitive: bool,
}

/// A claim about (or pointing at) an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactRecord {
    pub id: i64,
    pub subject: EntityLink,
    pub predicate: String,
    pub object: ClaimObjectView,
    /// One sentence stating the fact.
    pub text: String,
    pub confidence: f32,
    /// `active`, `superseded`, `retracted` or `user_asserted`.
    pub status: String,
    /// Empty only for `user_asserted` facts written in the vault.
    pub evidence: Vec<EvidenceRecord>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_from: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_to: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MentionRecord {
    pub item: ItemRef,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    pub surface: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub sensitive: bool,
}

/// An entity with its facts and mentions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    /// The surviving id (merged ids resolve here).
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// `(scheme, value)` pairs, e.g. `("email", "sam@example.com")`.
    pub identifiers: Vec<(String, String)>,
    pub attributes: Value,
    /// Active and user-asserted facts where the entity is subject or object.
    pub facts: Vec<FactRecord>,
    /// Most relevant mentions (the backend may cap these).
    pub mentions: Vec<MentionRecord>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Distinct origins over **all** the entity's evidence and mentions, not only the ones
    /// returned. Aliases, identifiers and attributes carry no provenance of their own, so they are
    /// shown only to clients allowed to see every origin.
    pub origins: Vec<Origin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    /// Hops from the centre.
    pub depth: u32,
    pub origins: Vec<Origin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub claim: i64,
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
    /// Origins of the claim's evidence; empty for a vault-only (`user_asserted`) claim.
    pub origins: Vec<Origin>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NeighbourhoodRecord {
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
    pub truncated: bool,
}

/// A whole item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRecord {
    pub item: ItemRef,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    /// Extracted plain text, when the store kept it (otherwise the chunks joined).
    #[serde(default)]
    pub text: Option<String>,
    pub sensitive: bool,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub source_modified_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSummaryRecord {
    pub item: ItemRef,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub sensitive: bool,
}

/// What changed after a point in time, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentChanges {
    pub entities: Vec<EntitySummaryRecord>,
    pub items: Vec<ItemSummaryRecord>,
}

/// Read-only access to Nugget's knowledge for the MCP server.
///
/// Implementations return `Ok(None)` / empty results for unknown ids, and reserve `Err` for
/// failures. They must never return content from outside the store (no network fetches).
#[async_trait]
pub trait KnowledgeBackend: Send + Sync {
    /// Hybrid search, best item first, at most `limit` hits.
    async fn search(
        &self,
        query: &str,
        limit: usize,
        filter: &ScopeFilter,
    ) -> Result<Vec<SearchRecord>>;

    /// Relevant passages packed under roughly `budget_tokens` tokens. With
    /// `filter.include_sensitive == false` this is `Retriever::build_context_with` with
    /// `ContextOptions { exclude_sensitive: true }`.
    async fn context(
        &self,
        query: &str,
        budget_tokens: u32,
        filter: &ScopeFilter,
    ) -> Result<ContextPack>;

    /// Entities whose name or alias contains `text` (all entities when `None`), most recently
    /// updated first.
    async fn find_entities(
        &self,
        text: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
        filter: &ScopeFilter,
    ) -> Result<Vec<EntitySummaryRecord>>;

    /// One entity, following merges. `None` if the id never existed.
    async fn get_entity(&self, id: &EntityId, filter: &ScopeFilter)
    -> Result<Option<EntityRecord>>;

    /// Breadth-first neighbourhood over active entity-to-entity claims.
    async fn neighbourhood(
        &self,
        id: &EntityId,
        depth: u32,
        limit: usize,
        filter: &ScopeFilter,
    ) -> Result<NeighbourhoodRecord>;

    async fn get_item(&self, item: &ItemRef, filter: &ScopeFilter) -> Result<Option<ItemRecord>>;

    /// Entities and items changed after `since`, newest first, at most `limit` of each.
    async fn recent_changes(
        &self,
        since: OffsetDateTime,
        limit: usize,
        filter: &ScopeFilter,
    ) -> Result<RecentChanges>;
}
