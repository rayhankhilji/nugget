//! Knowledge graph: typed entities, claims backed by evidence, and mentions.
//!
//! Invariants every [`GraphStore`] keeps (checked by [`conformance`]):
//! - Entity ids are never reused; merged and pruned ids are tombstones that lookups follow.
//! - A claim has evidence unless it is a user decision (`UserAsserted`, `Rejected`); losing its
//!   last evidence retracts it, except a user decision, which keeps its status.
//! - At most one live claim per triple: duplicates fold into the user's claim, else the oldest,
//!   with the union of evidence, max confidence and the strongest status.
//! - A claim the user replaced (`supersede_by_user`) stays `Superseded` while its evidence is
//!   what it had then; new evidence makes it `Active`.
//! - Quotes are cut to [`MAX_QUOTE_CHARS`]; `limit` 0 means unlimited; every mutation advances
//!   the change feed; `apply_batch` is all or nothing.

pub mod conformance;
pub mod mem;
pub mod types;

pub use mem::MemGraph;
pub use types::*;

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef, SourceId};

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
    /// Live entities by lower-cased name, then id; `query.limit` 0 = unlimited.
    fn list_entities(&self, query: &EntityQuery) -> Result<Vec<EntitySummary>>;
    /// Merge `from` into `into`: aliases, identifiers, claims (as subject and object) and mentions
    /// move; `from` becomes a tombstone pointing at `into`.
    fn merge(&self, into: &EntityId, from: &EntityId) -> Result<()>;

    // ---- claims ----
    /// Insert, or add the evidence to the live claim with the same triple (see the crate
    /// invariants: the higher confidence is kept, and a rejected claim stays rejected).
    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId>;
    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>>;
    /// Add evidence to a claim.
    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()>;
    /// Set a claim's status.
    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()>;
    /// The user replaced `claim` with their own wording (a vault edit): it becomes `Superseded`,
    /// then `by` (the user's claim) is added as with [`GraphStore::add_claim`], in one write.
    fn supersede_by_user(&self, claim: ClaimId, by: &NewClaim) -> Result<ClaimId>;

    /// Whether `claim` was replaced by the user ([`GraphStore::supersede_by_user`]) and every piece
    /// of `evidence` (non-empty) is remembered: from an item it had evidence from then, with the
    /// same quote or with the item unchanged since.
    fn replaced_by_user(&self, claim: ClaimId, evidence: &[Evidence]) -> Result<bool>;

    /// Whether the user replaced `claim` ([`GraphStore::supersede_by_user`]) and the record
    /// is still kept: while it is, evidence that is not remembered makes the claim current
    /// again (see the crate invariants), whatever evidence the claim has now.
    fn has_user_replacement(&self, claim: ClaimId) -> Result<bool>;

    /// When the claim was last made `UserAsserted` (added as such, a user claim folded into it, or
    /// its status set to it); kept through folds (the latest wins) and later evidence.
    fn asserted_at(&self, claim: ClaimId) -> Result<Option<time::OffsetDateTime>>;

    /// Claims where the entity is subject or object.
    fn claims_for(&self, entity: &EntityId, include_inactive: bool) -> Result<Vec<Claim>>;

    // ---- mentions ----
    fn add_mention(&self, m: &Mention) -> Result<()>;
    /// At most `limit` mentions (0 = unlimited), oldest first.
    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>>;

    // ---- provenance ----
    /// Drop all evidence, mentions and detail provenance from `item` (it changed or was deleted).
    fn retract_item(&self, item: &ItemRef) -> Result<Vec<ClaimId>>;

    // ---- graph views ----
    /// Breadth-first neighbourhood over active entity→entity claims, at most `limit` nodes
    /// (0 = unlimited).
    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView>;

    // ---- change feed ----
    /// Entities changed after `seq` (ascending, at most `limit`, 0 = unlimited) and the new
    /// high-water mark.
    fn changes_since(&self, seq: u64, limit: usize) -> Result<(Vec<EntityId>, u64)>;
    fn counts(&self) -> Result<GraphCounts>;

    /// Change a claim's confidence, validity bounds or text (not its triple or status).
    fn update_claim(&self, claim: ClaimId, patch: &ClaimPatch) -> Result<Claim>;

    /// Record that `item` supplied `detail` (an alias or identifier) of `entity`.
    fn add_detail_source(
        &self,
        entity: &EntityId,
        detail: &EntityDetail,
        item: &ItemRef,
    ) -> Result<()>;

    /// Which items supplied each alias and identifier (following merges).
    fn entity_provenance(&self, entity: &EntityId) -> Result<Option<EntityProvenance>>;

    /// Split `claims` off `entity` into a new entity created from `new`: the claims are re-pointed
    /// (subject and/or object) from `entity` to the new one.
    fn split(&self, entity: &EntityId, claims: &[ClaimId], new: &NewEntity) -> Result<EntityId>;

    /// Tombstone orphaned entities among `among` (every live entity when `None`) and return them.
    fn prune_orphans(&self, among: Option<&[EntityId]>) -> Result<Vec<EntityId>>;

    /// Live entities that items of `source` support, ascending by id: entities with a mention
    /// from an item of `source`, or that are subject or object of an active claim
    /// ([`ClaimStatus::is_active`]) with evidence from an item of `source`.
    fn entities_supported_by(&self, source: &SourceId) -> Result<Vec<EntityId>>;

    /// Live entities of `kinds` (empty = every kind), most connected first: by `claims` descending,
    /// then `mentions` descending, then `name` ascending (byte order), then id; counts as in
    /// [`GraphStore::list_entities`].
    fn top_entities(
        &self,
        kinds: &[EntityKind],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EntitySummary>> {
        top_entities_scan(self, kinds, limit, offset)
    }

    /// Apply `batch` in order, atomically (all or nothing, one change-feed step per touched
    /// entity), and return one result per op.
    fn apply_batch(&self, batch: &GraphBatch) -> Result<Vec<BatchResult>>;
}

/// [`GraphStore::top_entities`] through `list_entities` (the default): every live entity of
/// `kinds` is listed, sorted by (claims desc, mentions desc, name, id) and the page cut out.
pub fn top_entities_scan<G: GraphStore + ?Sized>(
    g: &G,
    kinds: &[EntityKind],
    limit: usize,
    offset: usize,
) -> Result<Vec<EntitySummary>> {
    let kind = (kinds.len() == 1).then(|| kinds[0]);
    let mut all: Vec<EntitySummary> = g
        .list_entities(&EntityQuery {
            kind,
            ..Default::default()
        })?
        .into_iter()
        .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
        .collect();
    all.sort_by(|a, b| {
        b.claims
            .cmp(&a.claims)
            .then(b.mentions.cmp(&a.mentions))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let limit = if limit == 0 { usize::MAX } else { limit };
    Ok(all.into_iter().skip(offset).take(limit).collect())
}
