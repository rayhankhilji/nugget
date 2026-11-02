//! Knowledge in the cloud: `nugget-knowledge` (deterministic extractors, the quote-gated LLM
//! extractor on the gateway's fast model, people/project resolution, the confirmed-sensitive
//! gate) runs on a user's snapshot, and the resulting graph is projected into `entities` and
//! `claims`. Entity rows keep their id across runs (keyed by a stable canonical: the "me" entity,
//! then the strongest identifier, then kind and normalised name).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use nugget_core::ids::EntityId;
use nugget_graph::{
    ClaimObject, ClaimStatus, Entity, EntityQuery, GraphStore, Identifier, normalize_name,
};
use nugget_knowledge::{Knowledge, KnowledgeConfig, RunBudget, RunStats};
use nugget_store::Store;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{Db, DbError};
use crate::llm_bridge::GatewayProvider;

/// Uncached extraction requests per run at most (the rest wait for later runs).
pub const MAX_FRESH_LLM: usize = 40;
/// A run stops between items after this long.
const RUN_MILLIS: u64 = 120_000;

/// One graph entity ready for Postgres.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityRow {
    pub graph_id: String,
    pub kind: String,
    pub name: String,
    pub canonical: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRow {
    pub subject: String,
    pub predicate: String,
    pub object_text: Option<String>,
    pub object: Option<String>,
    pub item_id: Uuid,
    pub confidence: f32,
    pub valid_from: Option<OffsetDateTime>,
    pub valid_to: Option<OffsetDateTime>,
    pub status: &'static str,
    pub evidence: Value,
}

fn identifier_rank(i: &Identifier) -> u8 {
    match i.scheme.as_str() {
        "email" => 0,
        "phone" => 1,
        "domain" => 2,
        "url" => 3,
        _ => 4,
    }
}

/// The stable key of an entity: `me`, else its strongest identifier, else kind and name.
pub fn canonical(e: &Entity) -> String {
    if e.attributes.get("is_me") == Some(&Value::Bool(true)) {
        return "me".into();
    }
    let mut ids: Vec<Identifier> = e.identifiers.iter().map(Identifier::normalized).collect();
    ids.sort_by(|a, b| {
        identifier_rank(a)
            .cmp(&identifier_rank(b))
            .then_with(|| a.value.cmp(&b.value))
    });
    match ids.first() {
        Some(i) => format!("{}:{}", i.scheme, i.value),
        None => format!("name:{}", normalize_name(&e.name)),
    }
}

fn status_str(s: ClaimStatus) -> &'static str {
    match s {
        ClaimStatus::Active => "active",
        ClaimStatus::Superseded => "superseded",
        ClaimStatus::Retracted => "retracted",
        ClaimStatus::UserAsserted => "user_asserted",
        ClaimStatus::Rejected => "rejected",
    }
}

/// Live entities and their current claims (as subject). Claims without evidence from a stored
/// item are left out: every row must cite its source.
pub fn project(graph: &dyn GraphStore) -> nugget_core::Result<(Vec<EntityRow>, Vec<ClaimRow>)> {
    let mut entities = Vec::new();
    let mut claims = Vec::new();
    let mut taken: HashSet<(String, String)> = HashSet::new();
    let mut live: Vec<Entity> = Vec::new();
    for s in graph.list_entities(&EntityQuery::default())? {
        if let Some(e) = graph.get_entity(&s.id)? {
            live.push(e);
        }
    }
    // Stable order so collisions get the same suffix every run.
    live.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    for e in &live {
        let kind = e.kind.as_str().to_owned();
        let base = canonical(e);
        let mut c = base.clone();
        let mut n = 2;
        while !taken.insert((kind.clone(), c.clone())) {
            c = format!("{base}#{n}");
            n += 1;
        }
        entities.push(EntityRow {
            graph_id: e.id.to_string(),
            kind,
            name: e.name.clone(),
            canonical: c,
            metadata: json!({
                "aliases": e.aliases,
                "identifiers": e.identifiers,
                "attributes": e.attributes,
            }),
        });
    }
    for e in &live {
        for c in graph.claims_for(&e.id, false)? {
            if c.subject != e.id || !c.status.is_active() {
                continue;
            }
            let evidence: Vec<(Uuid, Option<String>)> = c
                .evidence
                .iter()
                .filter_map(|ev| Some((ev.item.key.as_str().parse().ok()?, ev.quote.clone())))
                .collect();
            let Some((item_id, _)) = evidence.first().cloned() else {
                continue;
            };
            let (object, object_text) = match &c.object {
                ClaimObject::Entity(id) => (Some(id.to_string()), Some(c.text.clone())),
                ClaimObject::Literal(s) => (None, Some(s.clone())),
            };
            claims.push(ClaimRow {
                subject: e.id.to_string(),
                predicate: c.predicate.clone(),
                object_text,
                object: object.filter(|o| live.iter().any(|x| x.id.to_string() == *o)),
                item_id,
                confidence: c.confidence.clamp(0.0, 1.0),
                valid_from: c.valid_from,
                valid_to: c.valid_to,
                status: status_str(c.status),
                evidence: json!(
                    evidence
                        .iter()
                        .map(|(i, q)| json!({"item_id": i, "quote": q, "text": c.text}))
                        .collect::<Vec<_>>()
                ),
            });
        }
    }
    Ok((entities, claims))
}

/// Replaces the user's entities and claims with the projection, keeping ids of entities whose
/// canonical key survives. Claims citing items that no longer exist are skipped.
pub async fn store(
    db: &Db,
    user: Uuid,
    entities: &[EntityRow],
    claims: &[ClaimRow],
) -> Result<(usize, usize), DbError> {
    db.scoped(Some(user), async |tx| {
        let upsert = tx
            .prepare(
                "insert into public.entities (user_id, kind, name, canonical, metadata, graph_id)
                 values ($1, $2, $3, $4, $5, $6)
                 on conflict (user_id, kind, canonical) do update set
                   name = excluded.name, metadata = excluded.metadata, graph_id = excluded.graph_id
                 returning id",
            )
            .await?;
        let mut ids: BTreeMap<&str, Uuid> = BTreeMap::new();
        for e in entities {
            let id: Uuid = tx
                .query_one(
                    &upsert,
                    &[&user, &e.kind, &e.name, &e.canonical, &e.metadata, &e.graph_id],
                )
                .await?
                .get(0);
            ids.insert(&e.graph_id, id);
        }
        let keep: Vec<Uuid> = ids.values().copied().collect();
        tx.execute(
            "delete from public.entities where user_id = $1 and not (id = any($2))",
            &[&user, &keep],
        )
        .await?;
        tx.execute("delete from public.claims where user_id = $1", &[&user])
            .await?;
        let items: HashSet<Uuid> = tx
            .query(
                "select id from public.items where user_id = $1 and id = any($2)",
                &[&user, &claims.iter().map(|c| c.item_id).collect::<Vec<_>>()],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        let insert = tx
            .prepare(
                "insert into public.claims (user_id, subject_id, predicate, object_text, object_id, item_id,
                   confidence, valid_from, valid_to, status, evidence)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .await?;
        let mut n = 0;
        for c in claims {
            let Some(subject) = ids.get(c.subject.as_str()) else {
                continue;
            };
            if !items.contains(&c.item_id) {
                continue;
            }
            let object = c.object.as_deref().and_then(|o| ids.get(o)).copied();
            tx.execute(
                &insert,
                &[
                    &user, subject, &c.predicate, &c.object_text, &object, &c.item_id,
                    &c.confidence, &c.valid_from, &c.valid_to, &c.status, &c.evidence,
                ],
            )
            .await?;
            n += 1;
        }
        Ok((entities.len(), n))
    })
    .await
}

/// The pipeline configuration for a cloud run: the LLM extractor on when allowed, for every
/// source the snapshot holds (the confirmed-sensitive gate still keeps flagged items away).
pub fn config(llm: bool, sources: &[String]) -> KnowledgeConfig {
    let mut cfg = KnowledgeConfig::default();
    cfg.llm.enabled = llm;
    cfg.llm.cloud_sources = sources.to_vec();
    cfg
}

/// Runs extraction on `store` (a fresh snapshot) with `provider` as the only model.
pub async fn run(
    store: &Store,
    graph: Arc<dyn GraphStore>,
    provider: Option<Arc<GatewayProvider>>,
    sources: &[String],
) -> nugget_core::Result<(RunStats, Option<EntityId>)> {
    let router = provider.as_ref().map(|p| Arc::new(p.router()));
    let k = Knowledge::new(
        store.clone(),
        graph,
        router,
        config(provider.is_some(), sources),
    );
    let stats = k
        .run_once(RunBudget {
            max_items: usize::MAX,
            max_llm_requests: usize::MAX,
            max_millis: Some(RUN_MILLIS),
        })
        .await?;
    Ok((stats, k.status().me))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_graph::{EntityKind, MemGraph, NewEntity};

    fn entity(g: &MemGraph, kind: EntityKind, name: &str, ids: &[(&str, &str)]) -> EntityId {
        g.create_entity(&NewEntity {
            kind,
            name: name.into(),
            aliases: vec![],
            identifiers: ids.iter().map(|(s, v)| Identifier::new(*s, *v)).collect(),
            attributes: json!({}),
        })
        .unwrap()
    }

    #[test]
    fn canonical_keys_prefer_identifiers_and_dedupe() {
        let g = MemGraph::new();
        entity(
            &g,
            EntityKind::Person,
            "Sam Lee",
            &[("email", "Sam@Acme.example")],
        );
        entity(&g, EntityKind::Person, "Alex", &[]);
        entity(&g, EntityKind::Person, "alex", &[]);
        let (rows, claims) = project(&g).unwrap();
        let keys: Vec<&str> = rows.iter().map(|r| r.canonical.as_str()).collect();
        assert_eq!(
            keys,
            vec!["email:sam@acme.example", "name:alex", "name:alex#2"]
        );
        assert!(claims.is_empty());
    }
}
