//! Knowledge graph: typed entities, claims backed by evidence, and mentions.
//!
//! **Contract (stable for M2):** the types in [`types`] and the [`GraphStore`] trait. Work package
//! M2-A provides the SQLite implementation in `nugget-store`; [`MemGraph`] is the reference
//! implementation and test double. Both must pass the same behaviour tests.
//!
//! Invariants every implementation keeps:
//! - Entity ids are never reused. A merged entity keeps its id as a tombstone (`merged_into`), and
//!   every lookup by id follows it to the surviving entity.
//! - A claim always has at least one [`Evidence`] unless its status is `UserAsserted`. Removing the
//!   last evidence of a claim retracts it.
//! - Adding a claim that matches an active claim's `(subject, predicate, object)` adds the evidence
//!   to it instead of creating a duplicate.
//! - Every mutation advances the change feed ([`GraphStore::changes_since`]).

pub mod conformance;
pub mod mem;
pub mod types;

pub use mem::MemGraph;
pub use types::*;

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};

/// Storage for the knowledge graph. Synchronous; implementations are `Send + Sync`.
pub trait GraphStore: Send + Sync {
    // ---- entities ----
    fn create_entity(&self, new: &NewEntity) -> Result<EntityId>;
    /// Follows `merged_into`. `None` if the id never existed.
    fn get_entity(&self, id: &EntityId) -> Result<Option<Entity>>;
    fn update_entity(&self, id: &EntityId, patch: &EntityPatch) -> Result<Entity>;
    fn add_alias(&self, id: &EntityId, alias: &str) -> Result<()>;
    fn add_identifier(&self, id: &EntityId, ident: &Identifier) -> Result<()>;
    /// Exact identifier lookup (scheme + normalised value).
    fn find_by_identifier(&self, ident: &Identifier) -> Result<Option<EntityId>>;
    /// Entities whose name or an alias normalises to the same string as `name`.
    fn find_by_name(&self, name: &str, kind: Option<EntityKind>) -> Result<Vec<EntityId>>;
    fn list_entities(&self, query: &EntityQuery) -> Result<Vec<EntitySummary>>;
    /// Merge `from` into `into`: aliases, identifiers, claims (as subject and object) and mentions
    /// move; `from` becomes a tombstone pointing at `into`.
    fn merge(&self, into: &EntityId, from: &EntityId) -> Result<()>;

    // ---- claims ----
    /// Insert, or add the evidence to an existing active claim with the same triple.
    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId>;
    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>>;
    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()>;
    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()>;
    /// Claims where the entity is subject or object.
    fn claims_for(&self, entity: &EntityId, include_inactive: bool) -> Result<Vec<Claim>>;

    // ---- mentions ----
    fn add_mention(&self, m: &Mention) -> Result<()>;
    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>>;

    // ---- provenance ----
    /// Drop all evidence and mentions from `item` (it changed or was deleted). Claims left with
    /// no evidence are retracted. Returns the affected claim ids.
    fn retract_item(&self, item: &ItemRef) -> Result<Vec<ClaimId>>;

    // ---- graph views ----
    /// Breadth-first neighbourhood over active entity→entity claims.
    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView>;

    // ---- change feed ----
    /// Entities changed after `seq` (ascending, at most `limit`) and the new high-water mark.
    fn changes_since(&self, seq: u64, limit: usize) -> Result<(Vec<EntityId>, u64)>;
    fn counts(&self) -> Result<GraphCounts>;
}
