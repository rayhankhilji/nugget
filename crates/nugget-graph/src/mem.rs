//! In-memory [`GraphStore`]: the reference implementation and test double.
//!
//! Every write is a method on the inner `State`, so [`GraphStore::apply_batch`] can run a whole
//! batch on a copy of the state and swap it in only when every op succeeded (all or nothing).
//! That copy makes a batch O(graph size): fine for tests and fixtures, which is what `MemGraph`
//! is for.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use nugget_core::ids::{EntityId, ItemRef};
use nugget_core::{Error, Result};
use serde_json::Value;
use time::OffsetDateTime;

use crate::GraphStore;
use crate::types::*;

/// Provenance key of an alias or identifier (see [`EntityDetail::key`]).
type DetailKey = (&'static str, String, String);

#[derive(Default, Clone)]
struct State {
    next_entity: u64,
    next_claim: ClaimId,
    entities: HashMap<EntityId, Entity>,
    /// Tombstones: merged id → surviving id.
    merged: HashMap<EntityId, EntityId>,
    /// Tombstones of pruned entities (they resolve to nothing).
    pruned: HashSet<EntityId>,
    claims: BTreeMap<ClaimId, Claim>,
    mentions: Vec<Mention>,
    /// Which items supplied each alias / identifier, per entity.
    detail_sources: HashMap<EntityId, Vec<(DetailKey, ItemRef)>>,
    seq: u64,
    changed: HashMap<EntityId, u64>,
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

impl State {
    fn resolve(&self, id: &EntityId) -> Option<EntityId> {
        let mut cur = id.clone();
        for _ in 0..64 {
            match self.merged.get(&cur) {
                Some(next) => cur = next.clone(),
                None => return self.entities.contains_key(&cur).then_some(cur),
            }
        }
        None
    }

    fn live(&self, id: &EntityId) -> Result<EntityId> {
        self.resolve(id)
            .ok_or_else(|| Error::NotFound(format!("entity {id}")))
    }

    fn touch(&mut self, id: &EntityId) {
        self.seq += 1;
        self.changed.insert(id.clone(), self.seq);
        if let Some(e) = self.entities.get_mut(id) {
            e.updated_at = now();
        }
    }

    fn touch_ends(&mut self, subject: &EntityId, object: &ClaimObject) {
        self.touch(&subject.clone());
        if let ClaimObject::Entity(o) = object {
            self.touch(&o.clone());
        }
    }

    fn touch_claim(&mut self, id: ClaimId) {
        if let Some(c) = self.claims.get(&id) {
            let (s, o) = (c.subject.clone(), c.object.clone());
            self.touch_ends(&s, &o);
        }
    }

    fn claim_mut(&mut self, id: ClaimId) -> Result<&mut Claim> {
        self.claims
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("claim {id}")))
    }

    fn identifier_owner(&self, n: &Identifier) -> Option<&EntityId> {
        self.entities
            .values()
            .find(|e| e.identifiers.contains(n))
            .map(|e| &e.id)
    }

    /// The live claim with this triple, other than `except`.
    fn live_twin(
        &self,
        subject: &EntityId,
        predicate: &str,
        object: &ClaimObject,
        except: Option<ClaimId>,
    ) -> Option<ClaimId> {
        self.claims
            .values()
            .find(|c| {
                Some(c.id) != except
                    && c.status.is_live()
                    && &c.subject == subject
                    && c.predicate == predicate
                    && &c.object == object
            })
            .map(|c| c.id)
    }

    /// Fold `id` (just made live) with every live twin into the oldest of them. Returns the
    /// survivor.
    fn fold(&mut self, id: ClaimId, explicit: Option<ClaimStatus>) -> ClaimId {
        let Some(c) = self.claims.get(&id) else {
            return id;
        };
        if !c.status.is_live() {
            return id;
        }
        let mut group: Vec<ClaimId> = self
            .claims
            .values()
            .filter(|o| {
                o.status.is_live()
                    && o.subject == c.subject
                    && o.predicate == c.predicate
                    && o.object == c.object
            })
            .map(|o| o.id)
            .collect();
        if group.len() < 2 {
            return id;
        }
        group.sort_unstable();
        let keep = group[0];
        let mut status = self.claims[&keep].status;
        let mut confidence = self.claims[&keep].confidence;
        for gone in &group[1..] {
            let g = self.claims.remove(gone).expect("claim in group");
            status = status.strongest(g.status);
            confidence = confidence.max(g.confidence);
            let k = self.claims.get_mut(&keep).expect("survivor");
            for ev in g.evidence {
                push_unique(&mut k.evidence, ev);
            }
        }
        let k = self.claims.get_mut(&keep).expect("survivor");
        k.status = explicit.unwrap_or(status);
        k.confidence = confidence;
        k.updated_at = now();
        keep
    }

    // ---- entities ----

    fn create_entity(
        &mut self,
        new: &NewEntity,
        allow_owner: Option<&EntityId>,
    ) -> Result<EntityId> {
        if new.name.trim().is_empty() {
            return Err(Error::Invalid("entity name is empty".into()));
        }
        for ident in &new.identifiers {
            let n = ident.normalized();
            if let Some(owner) = self.identifier_owner(&n)
                && Some(owner) != allow_owner
            {
                return Err(Error::Invalid(format!(
                    "identifier {}:{} already belongs to another entity",
                    n.scheme, n.value
                )));
            }
        }
        self.next_entity += 1;
        let id = EntityId::new(format!("ent_{:06}", self.next_entity));
        let at = now();
        let mut aliases = Vec::new();
        for a in &new.aliases {
            if !a.trim().is_empty() && normalize_name(a) != normalize_name(&new.name) {
                push_unique(&mut aliases, a.trim().to_owned());
            }
        }
        let mut identifiers = Vec::new();
        for i in &new.identifiers {
            push_unique(&mut identifiers, i.normalized());
        }
        let attributes = match &new.attributes {
            Value::Null => Value::Object(Default::default()),
            v => v.clone(),
        };
        self.entities.insert(
            id.clone(),
            Entity {
                id: id.clone(),
                kind: new.kind,
                name: new.name.trim().to_owned(),
                aliases,
                identifiers,
                attributes,
                created_at: at,
                updated_at: at,
                merged_ids: Vec::new(),
            },
        );
        self.touch(&id);
        Ok(id)
    }

    fn update_entity(&mut self, id: &EntityId, patch: &EntityPatch) -> Result<EntityId> {
        let id = self.live(id)?;
        {
            let e = self.entities.get_mut(&id).expect("live entity");
            if let Some(n) = &patch.name {
                if n.trim().is_empty() {
                    return Err(Error::Invalid("entity name is empty".into()));
                }
                let old = std::mem::replace(&mut e.name, n.trim().to_owned());
                if normalize_name(&old) != normalize_name(&e.name) {
                    push_unique(&mut e.aliases, old);
                }
            }
            if let Some(k) = patch.kind {
                e.kind = k;
            }
            if let Some(a) = &patch.attributes {
                merge_attrs(&mut e.attributes, a);
            }
        }
        self.touch(&id);
        Ok(id)
    }

    fn add_alias(&mut self, id: &EntityId, alias: &str) -> Result<()> {
        let id = self.live(id)?;
        let e = self.entities.get_mut(&id).expect("live entity");
        let a = alias.trim();
        if !a.is_empty() && normalize_name(a) != normalize_name(&e.name) {
            push_unique(&mut e.aliases, a.to_owned());
        }
        self.touch(&id);
        Ok(())
    }

    fn add_identifier(&mut self, id: &EntityId, ident: &Identifier) -> Result<()> {
        let id = self.live(id)?;
        let n = ident.normalized();
        if let Some(other) = self.identifier_owner(&n)
            && *other != id
        {
            return Err(Error::Invalid(format!(
                "identifier {}:{} already belongs to {other}",
                n.scheme, n.value
            )));
        }
        push_unique(
            &mut self.entities.get_mut(&id).expect("live entity").identifiers,
            n,
        );
        self.touch(&id);
        Ok(())
    }

    fn add_detail_source(
        &mut self,
        id: &EntityId,
        detail: &EntityDetail,
        item: &ItemRef,
    ) -> Result<()> {
        let id = self.live(id)?;
        let row = (detail.key(), item.clone());
        if row.0.2.is_empty() {
            return Ok(());
        }
        push_unique(self.detail_sources.entry(id.clone()).or_default(), row);
        self.touch(&id);
        Ok(())
    }

    fn provenance(&self, id: &EntityId) -> Option<EntityProvenance> {
        let id = self.resolve(id)?;
        let e = &self.entities[&id];
        let rows = self.detail_sources.get(&id);
        let items = |key: DetailKey| -> Vec<ItemRef> {
            rows.map(|r| {
                r.iter()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, i)| i.clone())
                    .collect()
            })
            .unwrap_or_default()
        };
        Some(EntityProvenance {
            aliases: e
                .aliases
                .iter()
                .map(|a| (a.clone(), items(EntityDetail::Alias(a.clone()).key())))
                .collect(),
            identifiers: e
                .identifiers
                .iter()
                .map(|i| (i.clone(), items(EntityDetail::Identifier(i.clone()).key())))
                .collect(),
        })
    }

    fn merge(&mut self, into: &EntityId, from: &EntityId) -> Result<()> {
        let into = self.live(into)?;
        let from = self.live(from)?;
        if into == from {
            return Err(Error::Invalid("cannot merge an entity into itself".into()));
        }
        let gone = self.entities.remove(&from).expect("live entity");
        {
            let e = self.entities.get_mut(&into).expect("live entity");
            if normalize_name(&gone.name) != normalize_name(&e.name) {
                push_unique(&mut e.aliases, gone.name.clone());
            }
            for a in gone.aliases {
                if normalize_name(&a) != normalize_name(&e.name) {
                    push_unique(&mut e.aliases, a);
                }
            }
            for i in gone.identifiers {
                push_unique(&mut e.identifiers, i);
            }
            if let Value::Object(extra) = gone.attributes
                && let Value::Object(own) = &mut e.attributes
            {
                for (k, v) in extra {
                    own.entry(k).or_insert(v);
                }
            }
            push_unique(&mut e.merged_ids, from.clone());
            for m in gone.merged_ids {
                push_unique(&mut e.merged_ids, m);
            }
        }
        self.merged.insert(from.clone(), into.clone());
        if let Some(rows) = self.detail_sources.remove(&from) {
            let dst = self.detail_sources.entry(into.clone()).or_default();
            for r in rows {
                push_unique(dst, r);
            }
        }
        // Re-point claims, then fold duplicate live triples into the oldest claim.
        let mut moved = Vec::new();
        for c in self.claims.values_mut() {
            let mut hit = false;
            if c.subject == from {
                c.subject = into.clone();
                hit = true;
            }
            if c.object == ClaimObject::Entity(from.clone()) {
                c.object = ClaimObject::Entity(into.clone());
                hit = true;
            }
            if hit {
                c.updated_at = now();
                moved.push(c.id);
            }
        }
        let others: Vec<EntityId> = moved
            .iter()
            .filter_map(|id| self.claims.get(id))
            .flat_map(|c| {
                let mut v = vec![c.subject.clone()];
                if let ClaimObject::Entity(o) = &c.object {
                    v.push(o.clone());
                }
                v
            })
            .filter(|e| *e != into)
            .collect();
        for id in moved {
            self.fold(id, None);
        }
        let mut seen = HashSet::new();
        let mut mentions = Vec::with_capacity(self.mentions.len());
        for mut m in std::mem::take(&mut self.mentions) {
            if m.entity == from {
                m.entity = into.clone();
            }
            if !mentions.contains(&m) {
                mentions.push(m);
            }
        }
        self.mentions = mentions;
        for o in others {
            if seen.insert(o.clone()) {
                self.touch(&o);
            }
        }
        self.touch(&into);
        self.seq += 1;
        let seq = self.seq;
        self.changed.insert(from, seq);
        Ok(())
    }

    // ---- claims ----

    fn add_claim(&mut self, new: &NewClaim) -> Result<ClaimId> {
        validate_claim(new)?;
        let subject = self.live(&new.subject)?;
        let object = match &new.object {
            ClaimObject::Entity(o) => ClaimObject::Entity(self.live(o)?),
            lit => lit.clone(),
        };
        if let Some(existing) = self.live_twin(&subject, &new.predicate, &object, None) {
            let c = self.claims.get_mut(&existing).expect("claim");
            for ev in &new.evidence {
                push_unique(&mut c.evidence, ev.clamped());
            }
            c.confidence = c.confidence.max(new.confidence);
            if new.status.is_user_decision() {
                c.status = new.status;
            }
            c.updated_at = now();
            self.touch_ends(&subject, &object);
            return Ok(existing);
        }
        self.next_claim += 1;
        let id = self.next_claim;
        let at = now();
        let mut evidence = Vec::new();
        for ev in &new.evidence {
            push_unique(&mut evidence, ev.clamped());
        }
        self.claims.insert(
            id,
            Claim {
                id,
                subject: subject.clone(),
                predicate: new.predicate.clone(),
                object: object.clone(),
                text: new.text.clone(),
                confidence: new.confidence,
                status: new.status,
                evidence,
                valid_from: new.valid_from,
                valid_to: new.valid_to,
                created_at: at,
                updated_at: at,
            },
        );
        self.touch_ends(&subject, &object);
        Ok(id)
    }

    fn add_evidence(&mut self, claim: ClaimId, ev: &Evidence) -> Result<()> {
        let c = self.claim_mut(claim)?;
        push_unique(&mut c.evidence, ev.clamped());
        let revived = c.status == ClaimStatus::Retracted;
        if revived {
            c.status = ClaimStatus::Active;
        }
        c.updated_at = now();
        self.touch_claim(claim);
        if revived {
            self.fold(claim, None);
        }
        Ok(())
    }

    fn set_claim_status(&mut self, claim: ClaimId, status: ClaimStatus) -> Result<()> {
        let c = self.claim_mut(claim)?;
        if status == ClaimStatus::Active && c.evidence.is_empty() {
            return Err(Error::Invalid(
                "a claim without evidence cannot be active".into(),
            ));
        }
        c.status = status;
        c.updated_at = now();
        self.touch_claim(claim);
        if status.is_live() {
            self.fold(claim, status.is_user_decision().then_some(status));
        }
        Ok(())
    }

    fn update_claim(&mut self, claim: ClaimId, patch: &ClaimPatch) -> Result<ClaimId> {
        validate_patch(patch)?;
        let c = self.claim_mut(claim)?;
        if let Some(conf) = patch.confidence {
            c.confidence = conf;
        }
        if let Some(v) = patch.valid_from {
            c.valid_from = v;
        }
        if let Some(v) = patch.valid_to {
            c.valid_to = v;
        }
        if let Some(t) = &patch.text {
            c.text = t.trim().to_owned();
        }
        c.updated_at = now();
        self.touch_claim(claim);
        Ok(claim)
    }

    // ---- mentions and provenance ----

    fn add_mention(&mut self, m: &Mention) -> Result<()> {
        let entity = self.live(&m.entity)?;
        let m = Mention {
            entity: entity.clone(),
            ..m.clone()
        };
        if !self.mentions.contains(&m) {
            self.mentions.push(m);
        }
        self.touch(&entity);
        Ok(())
    }

    fn retract_item(&mut self, item: &ItemRef) -> Vec<ClaimId> {
        let mut affected = Vec::new();
        for c in self.claims.values_mut() {
            let before = c.evidence.len();
            c.evidence.retain(|e| &e.item != item);
            if c.evidence.len() != before {
                affected.push(c.id);
                if c.evidence.is_empty() && !c.status.is_user_decision() {
                    c.status = ClaimStatus::Retracted;
                }
                c.updated_at = now();
            }
        }
        let gone: HashSet<EntityId> = self
            .mentions
            .iter()
            .filter(|m| &m.item == item)
            .map(|m| m.entity.clone())
            .collect();
        self.mentions.retain(|m| &m.item != item);
        let mut sourced = Vec::new();
        for (e, rows) in &mut self.detail_sources {
            let before = rows.len();
            rows.retain(|(_, i)| i != item);
            if rows.len() != before {
                sourced.push(e.clone());
            }
        }
        for &id in &affected {
            self.touch_claim(id);
        }
        for e in gone.into_iter().chain(sourced) {
            self.touch(&e);
        }
        affected
    }

    // ---- split and prune ----

    fn split(
        &mut self,
        entity: &EntityId,
        claims: &[ClaimId],
        new: &NewEntity,
    ) -> Result<EntityId> {
        let from = self.live(entity)?;
        let mut ids: Vec<ClaimId> = claims.to_vec();
        ids.sort_unstable();
        ids.dedup();
        for id in &ids {
            let c = self
                .claims
                .get(id)
                .ok_or_else(|| Error::NotFound(format!("claim {id}")))?;
            if c.subject != from && c.object != ClaimObject::Entity(from.clone()) {
                return Err(Error::Invalid(format!(
                    "claim {id} does not reference entity {from}"
                )));
            }
        }
        let to = self.create_entity(new, Some(&from))?;
        // Identifiers and aliases named by `new` leave `from`, with their provenance.
        let moved_idents: Vec<Identifier> =
            new.identifiers.iter().map(Identifier::normalized).collect();
        let to_aliases: HashSet<String> = new
            .aliases
            .iter()
            .chain(std::iter::once(&new.name))
            .map(|a| normalize_name(a))
            .collect();
        let mut moved_keys: Vec<DetailKey> = Vec::new();
        {
            let e = self.entities.get_mut(&from).expect("live entity");
            e.identifiers.retain(|i| {
                let hit = moved_idents.contains(i);
                if hit {
                    moved_keys.push(EntityDetail::Identifier(i.clone()).key());
                }
                !hit
            });
            e.aliases.retain(|a| {
                let hit = to_aliases.contains(&normalize_name(a));
                if hit {
                    moved_keys.push(EntityDetail::Alias(a.clone()).key());
                }
                !hit
            });
        }
        if let Some(rows) = self.detail_sources.get_mut(&from) {
            let (go, stay): (Vec<_>, Vec<_>) = std::mem::take(rows)
                .into_iter()
                .partition(|(k, _)| moved_keys.contains(k));
            *rows = stay;
            let dst = self.detail_sources.entry(to.clone()).or_default();
            for r in go {
                push_unique(dst, r);
            }
        }
        let mut others = Vec::new();
        for id in &ids {
            let c = self.claims.get_mut(id).expect("checked");
            if c.subject == from {
                c.subject = to.clone();
            }
            if c.object == ClaimObject::Entity(from.clone()) {
                c.object = ClaimObject::Entity(to.clone());
            }
            c.updated_at = now();
            others.push(c.subject.clone());
            if let ClaimObject::Entity(o) = &c.object {
                others.push(o.clone());
            }
        }
        // Mentions follow items that now only support the new entity.
        let moved_items: HashSet<&ItemRef> = ids
            .iter()
            .flat_map(|id| self.claims[id].evidence.iter().map(|e| &e.item))
            .collect();
        let staying_items: HashSet<&ItemRef> = self
            .claims
            .values()
            .filter(|c| c.subject == from || c.object == ClaimObject::Entity(from.clone()))
            .flat_map(|c| c.evidence.iter().map(|e| &e.item))
            .collect();
        let movable: HashSet<ItemRef> = moved_items
            .difference(&staying_items)
            .map(|i| (*i).clone())
            .collect();
        let mut mentions = Vec::with_capacity(self.mentions.len());
        for mut m in std::mem::take(&mut self.mentions) {
            if m.entity == from && movable.contains(&m.item) {
                m.entity = to.clone();
            }
            if !mentions.contains(&m) {
                mentions.push(m);
            }
        }
        self.mentions = mentions;
        for id in &ids {
            self.fold(*id, None);
        }
        let mut seen = HashSet::new();
        for e in std::iter::once(from).chain(others) {
            if seen.insert(e.clone()) && self.entities.contains_key(&e) {
                self.touch(&e);
            }
        }
        Ok(to)
    }

    fn prune_orphans(&mut self, among: Option<&[EntityId]>) -> Vec<EntityId> {
        let candidates: Vec<EntityId> = match among {
            Some(ids) => {
                let mut v: Vec<EntityId> = ids.iter().filter_map(|i| self.resolve(i)).collect();
                v.sort();
                v.dedup();
                v
            }
            None => {
                let mut v: Vec<EntityId> = self.entities.keys().cloned().collect();
                v.sort();
                v
            }
        };
        let mut pruned = Vec::new();
        for id in candidates {
            let e = &self.entities[&id];
            if is_protected(&e.attributes) || self.mentions.iter().any(|m| m.entity == id) {
                continue;
            }
            let refs: Vec<&Claim> = self
                .claims
                .values()
                .filter(|c| c.subject == id || c.object == ClaimObject::Entity(id.clone()))
                .collect();
            if refs.iter().any(|c| c.status != ClaimStatus::Retracted) {
                continue;
            }
            let dead: Vec<ClaimId> = refs.iter().map(|c| c.id).collect();
            let mut others = Vec::new();
            for c in dead {
                let c = self.claims.remove(&c).expect("claim");
                others.push(c.subject);
                if let ClaimObject::Entity(o) = c.object {
                    others.push(o);
                }
            }
            self.entities.remove(&id);
            self.detail_sources.remove(&id);
            self.pruned.insert(id.clone());
            for o in others {
                if o != id && self.entities.contains_key(&o) {
                    self.touch(&o);
                }
            }
            self.seq += 1;
            let seq = self.seq;
            self.changed.insert(id.clone(), seq);
            pruned.push(id);
        }
        pruned
    }

    fn apply(&mut self, op: &GraphOp) -> Result<BatchResult> {
        Ok(match op {
            GraphOp::CreateEntity { entity } => {
                BatchResult::Entity(self.create_entity(entity, None)?)
            }
            GraphOp::UpdateEntity { entity, patch } => {
                self.update_entity(entity, patch)?;
                BatchResult::Done
            }
            GraphOp::AddAlias { entity, alias } => {
                self.add_alias(entity, alias)?;
                BatchResult::Done
            }
            GraphOp::AddIdentifier { entity, identifier } => {
                self.add_identifier(entity, identifier)?;
                BatchResult::Done
            }
            GraphOp::AddDetailSource {
                entity,
                detail,
                item,
            } => {
                self.add_detail_source(entity, detail, item)?;
                BatchResult::Done
            }
            GraphOp::AddClaim { claim } => BatchResult::Claim(self.add_claim(claim)?),
            GraphOp::AddEvidence { claim, evidence } => {
                self.add_evidence(*claim, evidence)?;
                BatchResult::Done
            }
            GraphOp::SetClaimStatus { claim, status } => {
                self.set_claim_status(*claim, *status)?;
                BatchResult::Done
            }
            GraphOp::UpdateClaim { claim, patch } => {
                self.update_claim(*claim, patch)?;
                BatchResult::Done
            }
            GraphOp::AddMention { mention } => {
                self.add_mention(mention)?;
                BatchResult::Done
            }
            GraphOp::RetractItem { item } => BatchResult::Claims(self.retract_item(item)),
        })
    }
}

/// Entities `prune_orphans` never removes.
pub(crate) fn is_protected(attributes: &Value) -> bool {
    ["is_me", "user_asserted"]
        .iter()
        .any(|k| attributes.get(*k) == Some(&Value::Bool(true)))
}

/// Thread-safe in-memory graph.
#[derive(Default)]
pub struct MemGraph {
    s: Mutex<State>,
}

impl MemGraph {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.s.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn merge_attrs(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                b.insert(k.clone(), v.clone());
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, x: T) {
    if !v.contains(&x) {
        v.push(x);
    }
}

fn validate_claim(new: &NewClaim) -> Result<()> {
    if new.predicate.trim().is_empty() {
        return Err(Error::Invalid("claim predicate is empty".into()));
    }
    if new.evidence.is_empty() && !new.status.is_user_decision() {
        return Err(Error::Invalid(
            "a claim needs evidence unless it is user-asserted or rejected".into(),
        ));
    }
    if !(0.0..=1.0).contains(&new.confidence) {
        return Err(Error::Invalid("confidence must be within 0..=1".into()));
    }
    Ok(())
}

/// Shared by both implementations' `update_claim`.
pub fn validate_patch(patch: &ClaimPatch) -> Result<()> {
    if let Some(c) = patch.confidence
        && !(0.0..=1.0).contains(&c)
    {
        return Err(Error::Invalid("confidence must be within 0..=1".into()));
    }
    if let Some(t) = &patch.text
        && t.trim().is_empty()
    {
        return Err(Error::Invalid("claim text is empty".into()));
    }
    Ok(())
}

impl GraphStore for MemGraph {
    fn create_entity(&self, new: &NewEntity) -> Result<EntityId> {
        self.lock().create_entity(new, None)
    }

    fn get_entity(&self, id: &EntityId) -> Result<Option<Entity>> {
        let s = self.lock();
        Ok(s.resolve(id).and_then(|r| s.entities.get(&r).cloned()))
    }

    fn update_entity(&self, id: &EntityId, patch: &EntityPatch) -> Result<Entity> {
        let mut s = self.lock();
        let id = s.update_entity(id, patch)?;
        Ok(s.entities[&id].clone())
    }

    fn add_alias(&self, id: &EntityId, alias: &str) -> Result<()> {
        self.lock().add_alias(id, alias)
    }

    fn add_identifier(&self, id: &EntityId, ident: &Identifier) -> Result<()> {
        self.lock().add_identifier(id, ident)
    }

    fn find_by_identifier(&self, ident: &Identifier) -> Result<Option<EntityId>> {
        let n = ident.normalized();
        Ok(self.lock().identifier_owner(&n).cloned())
    }

    fn find_by_name(&self, name: &str, kind: Option<EntityKind>) -> Result<Vec<EntityId>> {
        let want = normalize_name(name);
        if want.is_empty() {
            return Ok(Vec::new());
        }
        let s = self.lock();
        let mut out: Vec<EntityId> = s
            .entities
            .values()
            .filter(|e| kind.is_none_or(|k| e.kind == k))
            .filter(|e| {
                normalize_name(&e.name) == want
                    || e.aliases.iter().any(|a| normalize_name(a) == want)
            })
            .map(|e| e.id.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    fn list_entities(&self, q: &EntityQuery) -> Result<Vec<EntitySummary>> {
        let s = self.lock();
        let needle = q.text.as_deref().map(str::to_lowercase);
        let mut v: Vec<&Entity> = s
            .entities
            .values()
            .filter(|e| q.kind.is_none_or(|k| e.kind == k))
            .filter(|e| {
                needle.as_deref().is_none_or(|n| {
                    e.name.to_lowercase().contains(n)
                        || e.aliases.iter().any(|a| a.to_lowercase().contains(n))
                })
            })
            .collect();
        v.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        let limit = if q.limit == 0 { usize::MAX } else { q.limit };
        Ok(v.into_iter()
            .skip(q.offset)
            .take(limit)
            .map(|e| EntitySummary {
                id: e.id.clone(),
                kind: e.kind,
                name: e.name.clone(),
                claims: s
                    .claims
                    .values()
                    .filter(|c| c.status.is_active())
                    .filter(|c| c.subject == e.id || c.object == ClaimObject::Entity(e.id.clone()))
                    .count() as u64,
                mentions: s.mentions.iter().filter(|m| m.entity == e.id).count() as u64,
                updated_at: e.updated_at,
            })
            .collect())
    }

    fn merge(&self, into: &EntityId, from: &EntityId) -> Result<()> {
        self.lock().merge(into, from)
    }

    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId> {
        self.lock().add_claim(new)
    }

    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>> {
        Ok(self.lock().claims.get(&id).cloned())
    }

    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()> {
        self.lock().add_evidence(claim, ev)
    }

    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()> {
        self.lock().set_claim_status(claim, status)
    }

    fn claims_for(&self, entity: &EntityId, include_inactive: bool) -> Result<Vec<Claim>> {
        let s = self.lock();
        let Some(id) = s.resolve(entity) else {
            return Ok(Vec::new());
        };
        Ok(s.claims
            .values()
            .filter(|c| include_inactive || c.status.is_active())
            .filter(|c| c.subject == id || c.object == ClaimObject::Entity(id.clone()))
            .cloned()
            .collect())
    }

    fn add_mention(&self, m: &Mention) -> Result<()> {
        self.lock().add_mention(m)
    }

    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>> {
        let s = self.lock();
        let Some(id) = s.resolve(entity) else {
            return Ok(Vec::new());
        };
        Ok(s.mentions
            .iter()
            .filter(|m| m.entity == id)
            .take(if limit == 0 { usize::MAX } else { limit })
            .cloned()
            .collect())
    }

    fn retract_item(&self, item: &ItemRef) -> Result<Vec<ClaimId>> {
        Ok(self.lock().retract_item(item))
    }

    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView> {
        let s = self.lock();
        let start = s.live(center)?;
        let limit = if limit == 0 { usize::MAX } else { limit };
        let mut view = GraphView::default();
        let mut seen: HashMap<EntityId, u32> = HashMap::new();
        let mut queue = VecDeque::from([(start.clone(), 0u32)]);
        seen.insert(start, 0);
        let mut edges = HashSet::new();
        while let Some((id, d)) = queue.pop_front() {
            let e = &s.entities[&id];
            view.nodes.push(GraphNode {
                id: id.clone(),
                kind: e.kind,
                name: e.name.clone(),
                depth: d,
            });
            if d >= depth {
                continue;
            }
            for c in s.claims.values().filter(|c| c.status.is_active()) {
                let ClaimObject::Entity(o) = &c.object else {
                    continue;
                };
                let other = if c.subject == id {
                    o
                } else if *o == id {
                    &c.subject
                } else {
                    continue;
                };
                if !seen.contains_key(other) {
                    if seen.len() >= limit {
                        view.truncated = true;
                        continue;
                    }
                    seen.insert(other.clone(), d + 1);
                    queue.push_back((other.clone(), d + 1));
                }
                if edges.insert(c.id) {
                    view.edges.push(GraphEdge {
                        claim: c.id,
                        from: c.subject.clone(),
                        to: o.clone(),
                        predicate: c.predicate.clone(),
                    });
                }
            }
        }
        view.edges
            .retain(|e| seen.contains_key(&e.from) && seen.contains_key(&e.to));
        Ok(view)
    }

    fn changes_since(&self, seq: u64, limit: usize) -> Result<(Vec<EntityId>, u64)> {
        let s = self.lock();
        let mut v: Vec<(u64, EntityId)> = s
            .changed
            .iter()
            .filter(|(_, n)| **n > seq)
            .map(|(id, n)| (*n, id.clone()))
            .collect();
        v.sort();
        v.truncate(if limit == 0 { usize::MAX } else { limit });
        let high = v.last().map_or(seq, |(n, _)| *n);
        Ok((v.into_iter().map(|(_, id)| id).collect(), high))
    }

    fn counts(&self) -> Result<GraphCounts> {
        let s = self.lock();
        Ok(GraphCounts {
            entities: s.entities.len() as u64,
            claims: s.claims.values().filter(|c| c.status.is_active()).count() as u64,
            mentions: s.mentions.len() as u64,
        })
    }

    fn update_claim(&self, claim: ClaimId, patch: &ClaimPatch) -> Result<Claim> {
        let mut s = self.lock();
        let id = s.update_claim(claim, patch)?;
        Ok(s.claims[&id].clone())
    }

    fn add_detail_source(
        &self,
        entity: &EntityId,
        detail: &EntityDetail,
        item: &ItemRef,
    ) -> Result<()> {
        self.lock().add_detail_source(entity, detail, item)
    }

    fn entity_provenance(&self, entity: &EntityId) -> Result<Option<EntityProvenance>> {
        Ok(self.lock().provenance(entity))
    }

    fn split(&self, entity: &EntityId, claims: &[ClaimId], new: &NewEntity) -> Result<EntityId> {
        // On a copy, so a failure half-way leaves nothing behind.
        let mut s = self.lock();
        let mut work = s.clone();
        let id = work.split(entity, claims, new)?;
        *s = work;
        Ok(id)
    }

    fn prune_orphans(&self, among: Option<&[EntityId]>) -> Result<Vec<EntityId>> {
        Ok(self.lock().prune_orphans(among))
    }

    fn apply_batch(&self, batch: &GraphBatch) -> Result<Vec<BatchResult>> {
        let mut s = self.lock();
        let mut work = s.clone();
        let mut out = Vec::with_capacity(batch.ops.len());
        for op in &batch.ops {
            out.push(work.apply(op)?);
        }
        *s = work;
        Ok(out)
    }
}
