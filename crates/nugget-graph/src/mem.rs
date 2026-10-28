//! In-memory [`GraphStore`]: the reference implementation and test double.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use nugget_core::ids::{EntityId, ItemRef};
use nugget_core::{Error, Result};
use serde_json::Value;
use time::OffsetDateTime;

use crate::GraphStore;
use crate::types::*;

#[derive(Default)]
struct State {
    next_entity: u64,
    next_claim: ClaimId,
    entities: HashMap<EntityId, Entity>,
    /// Tombstones: merged id → surviving id.
    merged: HashMap<EntityId, EntityId>,
    claims: BTreeMap<ClaimId, Claim>,
    mentions: Vec<Mention>,
    seq: u64,
    changed: HashMap<EntityId, u64>,
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
            e.updated_at = OffsetDateTime::now_utc();
        }
    }

    fn touch_claim(&mut self, c: &Claim) {
        self.touch(&c.subject.clone());
        if let ClaimObject::Entity(o) = &c.object {
            self.touch(&o.clone());
        }
    }
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
    if new.evidence.is_empty() && new.status != ClaimStatus::UserAsserted {
        return Err(Error::Invalid(
            "a claim needs evidence unless it is user-asserted".into(),
        ));
    }
    if !(0.0..=1.0).contains(&new.confidence) {
        return Err(Error::Invalid("confidence must be within 0..=1".into()));
    }
    Ok(())
}

impl GraphStore for MemGraph {
    fn create_entity(&self, new: &NewEntity) -> Result<EntityId> {
        if new.name.trim().is_empty() {
            return Err(Error::Invalid("entity name is empty".into()));
        }
        let mut s = self.lock();
        for ident in &new.identifiers {
            let n = ident.normalized();
            if s.entities.values().any(|e| e.identifiers.contains(&n)) {
                return Err(Error::Invalid(format!(
                    "identifier {}:{} already belongs to another entity",
                    n.scheme, n.value
                )));
            }
        }
        s.next_entity += 1;
        let id = EntityId::new(format!("ent_{:06}", s.next_entity));
        let now = OffsetDateTime::now_utc();
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
        s.entities.insert(
            id.clone(),
            Entity {
                id: id.clone(),
                kind: new.kind,
                name: new.name.trim().to_owned(),
                aliases,
                identifiers,
                attributes,
                created_at: now,
                updated_at: now,
                merged_ids: Vec::new(),
            },
        );
        s.touch(&id);
        Ok(id)
    }

    fn get_entity(&self, id: &EntityId) -> Result<Option<Entity>> {
        let s = self.lock();
        Ok(s.resolve(id).and_then(|r| s.entities.get(&r).cloned()))
    }

    fn update_entity(&self, id: &EntityId, patch: &EntityPatch) -> Result<Entity> {
        let mut s = self.lock();
        let id = s.live(id)?;
        {
            let e = s.entities.get_mut(&id).expect("live entity");
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
        s.touch(&id);
        Ok(s.entities[&id].clone())
    }

    fn add_alias(&self, id: &EntityId, alias: &str) -> Result<()> {
        let mut s = self.lock();
        let id = s.live(id)?;
        let e = s.entities.get_mut(&id).expect("live entity");
        let a = alias.trim();
        if !a.is_empty() && normalize_name(a) != normalize_name(&e.name) {
            push_unique(&mut e.aliases, a.to_owned());
        }
        s.touch(&id);
        Ok(())
    }

    fn add_identifier(&self, id: &EntityId, ident: &Identifier) -> Result<()> {
        let mut s = self.lock();
        let id = s.live(id)?;
        let n = ident.normalized();
        if let Some(other) = s
            .entities
            .values()
            .find(|e| e.id != id && e.identifiers.contains(&n))
        {
            return Err(Error::Invalid(format!(
                "identifier {}:{} already belongs to {}",
                n.scheme, n.value, other.id
            )));
        }
        push_unique(
            &mut s.entities.get_mut(&id).expect("live entity").identifiers,
            n,
        );
        s.touch(&id);
        Ok(())
    }

    fn find_by_identifier(&self, ident: &Identifier) -> Result<Option<EntityId>> {
        let n = ident.normalized();
        let s = self.lock();
        Ok(s.entities
            .values()
            .find(|e| e.identifiers.contains(&n))
            .map(|e| e.id.clone()))
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
        let mut s = self.lock();
        let into = s.live(into)?;
        let from = s.live(from)?;
        if into == from {
            return Err(Error::Invalid("cannot merge an entity into itself".into()));
        }
        let gone = s.entities.remove(&from).expect("live entity");
        {
            let e = s.entities.get_mut(&into).expect("live entity");
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
        s.merged.insert(from.clone(), into.clone());
        // Re-point claims and mentions, folding duplicate triples together.
        let ids: Vec<ClaimId> = s.claims.keys().copied().collect();
        for id in ids {
            let mut c = s.claims.remove(&id).expect("claim");
            if c.subject == from {
                c.subject = into.clone();
            }
            if c.object == ClaimObject::Entity(from.clone()) {
                c.object = ClaimObject::Entity(into.clone());
            }
            let dup = s
                .claims
                .values()
                .find(|o| {
                    o.status.is_active()
                        && c.status.is_active()
                        && o.subject == c.subject
                        && o.predicate == c.predicate
                        && o.object == c.object
                })
                .map(|o| o.id);
            match dup {
                Some(d) => {
                    let target = s.claims.get_mut(&d).expect("claim");
                    for ev in c.evidence {
                        push_unique(&mut target.evidence, ev);
                    }
                    target.confidence = target.confidence.max(c.confidence);
                }
                None => {
                    s.claims.insert(id, c);
                }
            }
        }
        for m in &mut s.mentions {
            if m.entity == from {
                m.entity = into.clone();
            }
        }
        s.touch(&into);
        s.seq += 1;
        let seq = s.seq;
        s.changed.insert(from, seq);
        Ok(())
    }

    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId> {
        validate_claim(new)?;
        let mut s = self.lock();
        let subject = s.live(&new.subject)?;
        let object = match &new.object {
            ClaimObject::Entity(o) => ClaimObject::Entity(s.live(o)?),
            lit => lit.clone(),
        };
        if let Some(existing) = s
            .claims
            .values()
            .find(|c| {
                c.status.is_active()
                    && c.subject == subject
                    && c.predicate == new.predicate
                    && c.object == object
            })
            .map(|c| c.id)
        {
            let c = s.claims.get_mut(&existing).expect("claim");
            for ev in &new.evidence {
                push_unique(&mut c.evidence, ev.clone());
            }
            c.confidence = c.confidence.max(new.confidence);
            if new.status == ClaimStatus::UserAsserted {
                c.status = ClaimStatus::UserAsserted;
            }
            c.updated_at = OffsetDateTime::now_utc();
            let c = c.clone();
            s.touch_claim(&c);
            return Ok(existing);
        }
        s.next_claim += 1;
        let id = s.next_claim;
        let now = OffsetDateTime::now_utc();
        let mut evidence = Vec::new();
        for ev in &new.evidence {
            push_unique(&mut evidence, ev.clone());
        }
        let c = Claim {
            id,
            subject,
            predicate: new.predicate.clone(),
            object,
            text: new.text.clone(),
            confidence: new.confidence,
            status: new.status,
            evidence,
            valid_from: new.valid_from,
            valid_to: new.valid_to,
            created_at: now,
            updated_at: now,
        };
        s.touch_claim(&c);
        s.claims.insert(id, c);
        Ok(id)
    }

    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>> {
        Ok(self.lock().claims.get(&id).cloned())
    }

    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()> {
        let mut s = self.lock();
        let c = s
            .claims
            .get_mut(&claim)
            .ok_or_else(|| Error::NotFound(format!("claim {claim}")))?;
        push_unique(&mut c.evidence, ev.clone());
        if c.status == ClaimStatus::Retracted {
            c.status = ClaimStatus::Active;
        }
        c.updated_at = OffsetDateTime::now_utc();
        let c = c.clone();
        s.touch_claim(&c);
        Ok(())
    }

    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()> {
        let mut s = self.lock();
        let c = s
            .claims
            .get_mut(&claim)
            .ok_or_else(|| Error::NotFound(format!("claim {claim}")))?;
        if status == ClaimStatus::Active && c.evidence.is_empty() {
            return Err(Error::Invalid(
                "a claim without evidence cannot be active".into(),
            ));
        }
        c.status = status;
        c.updated_at = OffsetDateTime::now_utc();
        let c = c.clone();
        s.touch_claim(&c);
        Ok(())
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
        let mut s = self.lock();
        let entity = s.live(&m.entity)?;
        let m = Mention {
            entity: entity.clone(),
            ..m.clone()
        };
        if !s.mentions.contains(&m) {
            s.mentions.push(m);
        }
        s.touch(&entity);
        Ok(())
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
        let mut s = self.lock();
        let mut affected = Vec::new();
        let mut touched = Vec::new();
        for c in s.claims.values_mut() {
            let before = c.evidence.len();
            c.evidence.retain(|e| &e.item != item);
            if c.evidence.len() != before {
                affected.push(c.id);
                if c.evidence.is_empty() && c.status != ClaimStatus::UserAsserted {
                    c.status = ClaimStatus::Retracted;
                }
                c.updated_at = OffsetDateTime::now_utc();
                touched.push(c.clone());
            }
        }
        let gone: HashSet<EntityId> = s
            .mentions
            .iter()
            .filter(|m| &m.item == item)
            .map(|m| m.entity.clone())
            .collect();
        s.mentions.retain(|m| &m.item != item);
        for c in &touched {
            s.touch_claim(c);
        }
        for e in gone {
            s.touch(&e);
        }
        Ok(affected)
    }

    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView> {
        let s = self.lock();
        let start = s.live(center)?;
        let limit = limit.max(1);
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
}
