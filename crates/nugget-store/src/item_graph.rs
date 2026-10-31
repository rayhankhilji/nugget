//! Knowledge-graph reads keyed by item: the live entities each item mentions
//! ([`Store::entities_in_items`]) and the claims an item is evidence for
//! ([`Store::claims_with_evidence_in`]). Both run on a read connection, so search pages that show
//! them never wait on, or hold up, ingest and extraction.
//!
//! `mentions` and `claim_evidence` reference items by `(source, key)`; both lookups go through
//! the `mentions_item` / `claim_evidence_item` indexes.

use std::collections::HashMap;

use nugget_core::ids::{EntityId, ItemRef};
use nugget_core::{Error, Result};
use nugget_graph::{ClaimId, EntityKind};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::DbExt;
use crate::items::{LOOKUP_BATCH, refs_json};

/// A live (merge-resolved, not pruned) entity mentioned in an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityLinkRow {
    pub id: EntityId,
    pub name: String,
    pub kind: EntityKind,
    /// Mentions of the entity in the item (after merges fold ids together).
    pub mentions: u32,
}

/// Distinct live entities mentioned in each item of `?1` (a `refs_json` array), as
/// `(index in the array, id, name, kind, mentions)`, most-mentioned first within an item.
/// `CROSS JOIN`s fix the join order (refs, then mentions, then entities by primary key), so
/// each ref is a `mentions_item` range and the planner never scans `entities`; a
/// mention of a merged-away id resolves to its survivor (merges keep chains one hop long).
pub(crate) const ENTITIES_IN_ITEMS_SQL: &str =
    "SELECT want.key, l.id, l.name, l.kind, count(*) AS n
     FROM json_each(?1) AS want
     CROSS JOIN mentions m ON m.source = json_extract(want.value, '$[0]')
                          AND m.key = json_extract(want.value, '$[1]')
     CROSS JOIN entities e ON e.id = m.entity_id
     CROSS JOIN entities l ON l.id = coalesce(e.merged_into, e.id)
     WHERE l.merged_into IS NULL AND l.deleted_at IS NULL
     GROUP BY want.key, l.id
     ORDER BY want.key, n DESC, l.name_lower, l.id";

/// Claims with evidence in the item `(?1, ?2)`, active ones only when `?3` is 1, oldest first,
/// at most `?4` (-1 = all).
pub(crate) const CLAIMS_IN_ITEM_SQL: &str = "SELECT DISTINCT c.id
     FROM claim_evidence ev CROSS JOIN claims c ON c.id = ev.claim_id
     WHERE ev.source = ?1 AND ev.key = ?2
       AND (?3 = 0 OR c.status IN ('active', 'user_asserted'))
     ORDER BY c.id
     LIMIT ?4";

fn kind_from_str(s: &str) -> Result<EntityKind> {
    EntityKind::ALL
        .into_iter()
        .find(|k| k.as_str() == s)
        .ok_or_else(|| Error::Storage(format!("unknown entity kind {s:?} in store")))
}

impl Store {
    /// The distinct live entities mentioned in each of `items`, most-mentioned first (ties by
    /// name). Mentions of merged-away ids count for the surviving entity; pruned entities are
    /// left out. Items without mentions (or unknown) are absent from the map. Looked up in
    /// batches of `LOOKUP_BATCH` items, one query each, on a read connection.
    pub fn entities_in_items(
        &self,
        items: &[ItemRef],
    ) -> Result<HashMap<ItemRef, Vec<EntityLinkRow>>> {
        type Row = (usize, String, String, String, i64);
        let mut out: HashMap<ItemRef, Vec<EntityLinkRow>> = HashMap::new();
        for batch in items.chunks(LOOKUP_BATCH) {
            let refs = refs_json(batch)?;
            let rows: Vec<Row> = self.with_read_conn(|c| {
                let mut stmt = c
                    .prepare_cached(ENTITIES_IN_ITEMS_SQL)
                    .db("prepare entities_in_items")?;
                let rows = stmt
                    .query_map([&refs], |r| {
                        Ok((
                            r.get::<_, i64>(0)? as usize,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                        ))
                    })
                    .db("query entities_in_items")?;
                rows.collect::<rusqlite::Result<_>>()
                    .db("read entities_in_items")
            })?;
            for (idx, id, name, kind, n) in rows {
                let Some(item) = batch.get(idx) else {
                    continue;
                };
                let links = out.entry(item.clone()).or_default();
                // Duplicate refs in `items` give the same rows twice; keep one.
                if links.iter().any(|l| l.id.as_str() == id) {
                    continue;
                }
                links.push(EntityLinkRow {
                    id: EntityId::new(id),
                    name,
                    kind: kind_from_str(&kind)?,
                    mentions: u32::try_from(n).unwrap_or(u32::MAX),
                });
            }
        }
        Ok(out)
    }

    /// Ids of the claims that `item` is evidence for, oldest first: only active ones
    /// (`active`, `user_asserted`) when `active_only`, at most `limit` (0 = all). Read on a read
    /// connection through the `claim_evidence_item` index.
    pub fn claims_with_evidence_in(
        &self,
        item: &ItemRef,
        active_only: bool,
        limit: usize,
    ) -> Result<Vec<ClaimId>> {
        let limit = if limit == 0 {
            -1
        } else {
            i64::try_from(limit).unwrap_or(i64::MAX)
        };
        self.with_read_conn(|c| {
            let mut stmt = c
                .prepare_cached(CLAIMS_IN_ITEM_SQL)
                .db("prepare claims_with_evidence_in")?;
            let rows = stmt
                .query_map(
                    rusqlite::params![
                        item.source.as_str(),
                        item.key.as_str(),
                        i64::from(active_only),
                        limit
                    ],
                    |r| r.get::<_, ClaimId>(0),
                )
                .db("query claims_with_evidence_in")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read claims_with_evidence_in")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewItem, content_hash};
    use nugget_core::ids::{ItemKey, SourceId};
    use nugget_graph::{
        ClaimObject, ClaimStatus, Evidence, GraphStore, Mention, NewClaim, NewEntity,
    };

    fn iref(source: &str, key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new(source),
            key: ItemKey::new(key),
        }
    }

    fn put(s: &Store, item: &ItemRef) {
        let body = format!("body of {}", item.key.as_str());
        s.upsert_item(
            &NewItem::new(item.source.as_str(), item.key.as_str(), content_hash(&body))
                .with_text(body),
        )
        .unwrap();
    }

    fn mention(entity: &EntityId, item: &ItemRef, surface: &str) -> Mention {
        Mention {
            entity: entity.clone(),
            item: item.clone(),
            chunk_id: None,
            surface: surface.to_owned(),
            confidence: 0.9,
        }
    }

    fn claim(subject: &EntityId, lit: &str, items: &[&ItemRef]) -> NewClaim {
        NewClaim {
            subject: subject.clone(),
            predicate: "works_at".into(),
            object: ClaimObject::Literal(lit.into()),
            text: format!("works at {lit}"),
            confidence: 0.8,
            status: ClaimStatus::Active,
            evidence: items
                .iter()
                .map(|i| Evidence {
                    item: (*i).clone(),
                    chunk_id: None,
                    quote: Some(format!("quote {lit}")),
                })
                .collect(),
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn entities_in_items_are_distinct_live_and_merge_resolved() {
        let s = Store::open_in_memory().unwrap();
        let g = s.graph();
        let (a, b, c) = (iref("mail", "a"), iref("mail", "b"), iref("files", "c"));
        for i in [&a, &b, &c] {
            put(&s, i);
        }
        let sam = g
            .create_entity(&NewEntity::new(EntityKind::Person, "Sam Lee"))
            .unwrap();
        let samuel = g
            .create_entity(&NewEntity::new(EntityKind::Person, "Samuel"))
            .unwrap();
        let acme = g
            .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
            .unwrap();
        g.add_mention(&mention(&sam, &a, "Sam")).unwrap();
        g.add_mention(&mention(&sam, &a, "Sam Lee")).unwrap();
        g.add_mention(&mention(&samuel, &a, "Samuel")).unwrap();
        g.add_mention(&mention(&acme, &a, "Acme")).unwrap();
        g.add_mention(&mention(&acme, &b, "Acme")).unwrap();
        // Mentions written under an id that is merged afterwards resolve to the survivor.
        g.merge(&sam, &samuel).unwrap();

        let got = s
            .entities_in_items(&[a.clone(), b.clone(), c.clone(), a.clone()])
            .unwrap();
        let in_a = &got[&a];
        assert_eq!(in_a.len(), 2, "{in_a:?}");
        assert_eq!(in_a[0].id, sam);
        assert_eq!(in_a[0].name, "Sam Lee");
        assert_eq!(in_a[0].kind, EntityKind::Person);
        assert_eq!(in_a[0].mentions, 3);
        assert_eq!(in_a[1].id, acme);
        assert_eq!(in_a[1].kind, EntityKind::Organization);
        assert_eq!(got[&b].len(), 1);
        assert!(!got.contains_key(&c), "no mentions: absent");
        assert!(s.entities_in_items(&[]).unwrap().is_empty());
        // Unknown items are absent too.
        assert!(
            s.entities_in_items(&[iref("nope", "x")])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entities_in_items_batches_large_inputs() {
        let s = Store::open_in_memory().unwrap();
        let g = s.graph();
        let e = g
            .create_entity(&NewEntity::new(EntityKind::Topic, "Budget"))
            .unwrap();
        let refs: Vec<ItemRef> = (0..LOOKUP_BATCH + 3)
            .map(|i| iref("files", &format!("k{i}")))
            .collect();
        let last = refs.last().unwrap().clone();
        g.add_mention(&mention(&e, &refs[0], "budget")).unwrap();
        g.add_mention(&mention(&e, &last, "budget")).unwrap();
        let got = s.entities_in_items(&refs).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[&last][0].id, e);
    }

    #[test]
    fn claims_with_evidence_in_filters_status_and_limits() {
        let s = Store::open_in_memory().unwrap();
        let g = s.graph();
        let (a, b) = (iref("mail", "a"), iref("mail", "b"));
        put(&s, &a);
        put(&s, &b);
        let sam = g
            .create_entity(&NewEntity::new(EntityKind::Person, "Sam"))
            .unwrap();
        let c1 = g.add_claim(&claim(&sam, "Acme", &[&a, &b])).unwrap();
        let c2 = g.add_claim(&claim(&sam, "Globex", &[&a])).unwrap();
        let c3 = g.add_claim(&claim(&sam, "Initech", &[&b])).unwrap();
        g.set_claim_status(c2, ClaimStatus::Rejected).unwrap();

        assert_eq!(s.claims_with_evidence_in(&a, false, 0).unwrap(), [c1, c2]);
        assert_eq!(s.claims_with_evidence_in(&a, true, 0).unwrap(), [c1]);
        assert_eq!(s.claims_with_evidence_in(&b, true, 0).unwrap(), [c1, c3]);
        assert_eq!(s.claims_with_evidence_in(&b, true, 1).unwrap(), [c1]);
        assert!(
            s.claims_with_evidence_in(&iref("mail", "zzz"), false, 0)
                .unwrap()
                .is_empty()
        );
    }

    /// Both statements reach `mentions`, `claim_evidence`, `claims` and `entities` through an
    /// index or the primary key, never a full scan (the same check as `query_plans.rs`).
    #[test]
    fn item_graph_statements_use_indexes() {
        let s = Store::open_in_memory().unwrap();
        let g = s.graph();
        for i in 0..50 {
            let it = iref("mail", &format!("k{i}"));
            put(&s, &it);
            let e = g
                .create_entity(&NewEntity::new(EntityKind::Person, format!("P{i}")))
                .unwrap();
            g.add_mention(&mention(&e, &it, "P")).unwrap();
            g.add_claim(&claim(&e, &format!("Org{i}"), &[&it])).unwrap();
        }
        let check = |label: &str| {
            s.with_conn(|c| {
                for sql in [ENTITIES_IN_ITEMS_SQL, CLAIMS_IN_ITEM_SQL] {
                    let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                    let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
                    let steps: Vec<String> = stmt
                        .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap();
                    let scans: Vec<&String> = steps
                        .iter()
                        .filter(|d| d.starts_with("SCAN ") && !d.contains("VIRTUAL TABLE"))
                        .collect();
                    assert!(scans.is_empty(), "{label}: {sql}\n{steps:#?}");
                }
                Ok(())
            })
            .unwrap();
        };
        check("populated");
        s.with_conn(|c| {
            c.execute_batch("ANALYZE;").unwrap();
            Ok(())
        })
        .unwrap();
        check("after ANALYZE");
    }
}
