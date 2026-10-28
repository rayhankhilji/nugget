//! Knowledge graph: typed entities, claims backed by evidence, and mentions.
//!
//! **Contract (stable for M2):** the types in [`types`] and the [`GraphStore`] trait. Work package
//! M2-A provides the SQLite implementation in `nugget-store`; [`MemGraph`] is the reference
//! implementation and test double. Both must pass the same behaviour tests ([`conformance`]).
//!
//! Invariants every implementation keeps:
//! - Entity ids are never reused. A merged entity keeps its id as a tombstone (`merged_into`), and
//!   every lookup by id follows it to the surviving entity. An entity removed by
//!   [`GraphStore::prune_orphans`] is a tombstone too: lookups return `None`.
//! - A claim always has at least one [`Evidence`] unless its status is a user decision
//!   (`UserAsserted` or `Rejected`). Removing the last evidence of a claim retracts it, except a
//!   `Rejected` (or `UserAsserted`) one, which keeps its status.
//! - **No duplicate live triples.** At most one claim per `(subject, predicate, object)` is live
//!   ([`ClaimStatus::is_live`]: `Active`, `UserAsserted` or `Rejected`). Adding a claim that
//!   matches a live claim adds the evidence to it instead of creating a duplicate, keeping the
//!   **higher confidence** and the live claim's status (a new `UserAsserted` or `Rejected` claim
//!   sets that status: it is a user decision). In particular a rejected fact stays rejected when
//!   its source is re-extracted. Anything that makes a claim live again (`add_evidence` on a
//!   retracted claim, `set_claim_status`, a merge) **folds** it into an existing live twin.
//! - **Fold survivor:** when claims fold, the one with the **oldest (smallest) claim id**
//!   survives; it gets the union of the evidence, the maximum confidence and the strongest status
//!   ([`ClaimStatus::strongest`]: `UserAsserted` > `Rejected` > `Active`), except that an explicit
//!   `set_claim_status` to `UserAsserted` or `Rejected` keeps the requested status. Folded-away
//!   claim ids are gone (`get_claim` returns `None`) and never reused.
//! - Quotes are cut to [`MAX_QUOTE_CHARS`] characters on every write.
//! - **`limit` 0 means unlimited** everywhere a method takes a `limit` (`list_entities`,
//!   `mentions_for`, `neighbourhood`, `changes_since`); implementations may still apply a
//!   documented hard cap (e.g. `SqliteGraph`'s neighbourhood bound).
//! - Every mutation advances the change feed ([`GraphStore::changes_since`]).
//! - [`GraphStore::apply_batch`] applies its ops in one transaction where the implementation
//!   supports it (both `MemGraph` and `SqliteGraph` do): all or nothing, and readers never see a
//!   half-applied batch.

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
    /// Live entities by lower-cased name, then id; `query.limit` 0 = unlimited.
    fn list_entities(&self, query: &EntityQuery) -> Result<Vec<EntitySummary>>;
    /// Merge `from` into `into`: aliases, identifiers, claims (as subject and object) and mentions
    /// move; `from` becomes a tombstone pointing at `into`.
    fn merge(&self, into: &EntityId, from: &EntityId) -> Result<()>;

    // ---- claims ----
    /// Insert, or add the evidence to the live claim with the same triple (see the crate
    /// invariants: the higher confidence is kept, and a rejected claim stays rejected). Returns
    /// the id of the claim the triple landed in.
    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId>;
    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>>;
    /// Add evidence to a claim. A `Retracted` claim becomes `Active` again (folding into a live
    /// twin, if any); other statuses are kept.
    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()>;
    /// Set a claim's status. `Active` needs evidence (`Invalid` otherwise). Making a claim live
    /// folds it into a live twin, if any (see the crate invariants for the survivor).
    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()>;
    /// Claims where the entity is subject or object.
    fn claims_for(&self, entity: &EntityId, include_inactive: bool) -> Result<Vec<Claim>>;

    // ---- mentions ----
    fn add_mention(&self, m: &Mention) -> Result<()>;
    /// At most `limit` mentions (0 = unlimited), oldest first.
    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>>;

    // ---- provenance ----
    /// Drop all evidence, mentions and detail provenance from `item` (it changed or was
    /// deleted). Claims left with no evidence are retracted (user-asserted and rejected ones keep
    /// their status). Returns the affected claim ids, ascending.
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

    // ---- M2 wave 2 additions (all have defaults, so older implementations keep compiling) ----

    /// Change a claim's confidence, validity bounds or text (not its triple or status). Returns
    /// the updated claim. `NotFound` for an unknown claim, `Invalid` for a confidence outside
    /// `0..=1` or a blank text.
    fn update_claim(&self, claim: ClaimId, patch: &ClaimPatch) -> Result<Claim> {
        let _ = (claim, patch);
        Err(nugget_core::Error::Unsupported(
            "this graph store cannot update claims".into(),
        ))
    }

    /// Record that `item` supplied `detail` (an alias or identifier) of `entity`. Recorded even
    /// if the entity does not hold the detail (yet); [`GraphStore::entity_provenance`] only
    /// reports details the entity holds. Dropped by [`GraphStore::retract_item`] for that item;
    /// moves with the detail on merge and split. The default keeps no provenance.
    fn add_detail_source(
        &self,
        entity: &EntityId,
        detail: &EntityDetail,
        item: &ItemRef,
    ) -> Result<()> {
        let _ = (entity, detail, item);
        Ok(())
    }

    /// Which items supplied each alias and identifier (following merges). `None` if the id never
    /// existed. The default reports every detail with no items.
    fn entity_provenance(&self, entity: &EntityId) -> Result<Option<EntityProvenance>> {
        Ok(self.get_entity(entity)?.map(|e| EntityProvenance {
            aliases: e.aliases.into_iter().map(|a| (a, Vec::new())).collect(),
            identifiers: e.identifiers.into_iter().map(|i| (i, Vec::new())).collect(),
        }))
    }

    /// Split `claims` off `entity` into a new entity created from `new`: the claims are
    /// re-pointed (subject and/or object) from `entity` to the new one. Identifiers of `entity`
    /// listed in `new.identifiers` move (with their provenance); aliases of `entity` listed in
    /// `new.aliases` move too (its name stays). Mentions of `entity` move when their item
    /// supports a moved claim and no claim left on `entity`. Every claim must have `entity` as
    /// subject or object (`Invalid` otherwise); an identifier held by a third entity is
    /// `Invalid`. Returns the new entity's id.
    fn split(&self, entity: &EntityId, claims: &[ClaimId], new: &NewEntity) -> Result<EntityId> {
        let _ = (entity, claims, new);
        Err(nugget_core::Error::Unsupported(
            "this graph store cannot split entities".into(),
        ))
    }

    /// Tombstone orphaned entities among `among` (every live entity when `None`) and return
    /// them. An entity is an orphan when it has no mentions and every claim that references it
    /// is `Retracted` (those claims are deleted with it). Never pruned: entities with an
    /// attribute `is_me` or `user_asserted` set to `true`, and entities with any active,
    /// superseded, user-asserted or rejected claim. A pruned id resolves to nothing afterwards
    /// (and is never reused); its identifiers are released. The default prunes nothing.
    fn prune_orphans(&self, among: Option<&[EntityId]>) -> Result<Vec<EntityId>> {
        let _ = among;
        Ok(Vec::new())
    }

    /// Apply `batch` in order and return one result per op. `MemGraph` and `SqliteGraph` apply it
    /// atomically (one transaction: all or nothing, one change-feed step per touched entity);
    /// the default applies the ops one by one through the methods above and stops at the first
    /// error.
    fn apply_batch(&self, batch: &GraphBatch) -> Result<Vec<BatchResult>> {
        let mut out = Vec::with_capacity(batch.ops.len());
        for op in &batch.ops {
            out.push(apply_op(self, op)?);
        }
        Ok(out)
    }
}

/// Apply one op through the single-write methods of `g` (the default `apply_batch`).
pub fn apply_op<G: GraphStore + ?Sized>(g: &G, op: &GraphOp) -> Result<BatchResult> {
    Ok(match op {
        GraphOp::CreateEntity { entity } => BatchResult::Entity(g.create_entity(entity)?),
        GraphOp::UpdateEntity { entity, patch } => {
            g.update_entity(entity, patch)?;
            BatchResult::Done
        }
        GraphOp::AddAlias { entity, alias } => {
            g.add_alias(entity, alias)?;
            BatchResult::Done
        }
        GraphOp::AddIdentifier { entity, identifier } => {
            g.add_identifier(entity, identifier)?;
            BatchResult::Done
        }
        GraphOp::AddDetailSource {
            entity,
            detail,
            item,
        } => {
            g.add_detail_source(entity, detail, item)?;
            BatchResult::Done
        }
        GraphOp::AddClaim { claim } => BatchResult::Claim(g.add_claim(claim)?),
        GraphOp::AddEvidence { claim, evidence } => {
            g.add_evidence(*claim, evidence)?;
            BatchResult::Done
        }
        GraphOp::SetClaimStatus { claim, status } => {
            g.set_claim_status(*claim, *status)?;
            BatchResult::Done
        }
        GraphOp::UpdateClaim { claim, patch } => {
            g.update_claim(*claim, patch)?;
            BatchResult::Done
        }
        GraphOp::AddMention { mention } => {
            g.add_mention(mention)?;
            BatchResult::Done
        }
        GraphOp::RetractItem { item } => BatchResult::Claims(g.retract_item(item)?),
    })
}
