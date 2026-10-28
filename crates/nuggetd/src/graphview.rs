//! Graph types → IPC views and MCP records, with evidence titles, urls and sensitivity read from
//! the store. Everything here is synchronous (run it on the blocking pool).

use std::collections::{BTreeSet, HashMap, HashSet};

use nugget_core::ids::{EntityId, ItemRef};
use nugget_core::ipc::{
    ClaimObjectView, ClaimView, EntityLink, EntitySummaryView, EvidenceView, GraphEdgeView,
    GraphNodeView, GraphViewIpc, MentionView,
};
use nugget_core::{Error, Result};
use nugget_graph::{
    Claim, ClaimObject, ClaimStatus, Entity, EntityKind, EntityQuery, EntitySummary, GraphStore,
    Mention,
};
use nugget_mcp::Origin;
use nugget_store::Store;
use serde_json::Value;

/// Mentions read per entity to compute its origins.
pub(crate) const ORIGIN_MENTIONS: usize = 5000;
/// Entities scanned for the graph overview and other whole-graph listings.
pub(crate) const MAX_SCAN: usize = 5000;
const SCAN_PAGE: usize = 500;

/// What a view shows about an evidence item.
#[derive(Debug, Clone, Default)]
pub(crate) struct ItemInfo {
    pub title: Option<String>,
    pub url: Option<String>,
    /// `nugget_retrieve::is_sensitive` of the item; missing items count as sensitive.
    pub sensitive: bool,
}

/// Per-request cache of item and entity lookups.
pub(crate) struct Lookups<'a> {
    pub store: &'a Store,
    pub graph: &'a dyn GraphStore,
    items: HashMap<ItemRef, ItemInfo>,
    names: HashMap<EntityId, Option<Entity>>,
}

impl<'a> Lookups<'a> {
    pub fn new(store: &'a Store, graph: &'a dyn GraphStore) -> Self {
        Self {
            store,
            graph,
            items: HashMap::new(),
            names: HashMap::new(),
        }
    }

    pub fn item(&mut self, item: &ItemRef) -> Result<ItemInfo> {
        if let Some(i) = self.items.get(item) {
            return Ok(i.clone());
        }
        let info = match self.store.get_item(item)? {
            Some(it) => ItemInfo {
                url: item_url(&it.metadata),
                sensitive: nugget_retrieve::is_sensitive(&it.metadata),
                title: it.title,
            },
            None => ItemInfo {
                sensitive: true,
                ..Default::default()
            },
        };
        self.items.insert(item.clone(), info.clone());
        Ok(info)
    }

    pub fn entity(&mut self, id: &EntityId) -> Result<Option<Entity>> {
        if let Some(e) = self.names.get(id) {
            return Ok(e.clone());
        }
        let e = self.graph.get_entity(id)?;
        self.names.insert(id.clone(), e.clone());
        Ok(e)
    }

    /// `{id, name}` of an entity (the surviving one after merges).
    pub fn link(&mut self, id: &EntityId) -> Result<EntityLink> {
        Ok(match self.entity(id)? {
            Some(e) => EntityLink {
                id: e.id,
                name: e.name,
            },
            None => EntityLink {
                id: id.clone(),
                name: id.to_string(),
            },
        })
    }

    pub fn object(&mut self, o: &ClaimObject) -> Result<ClaimObjectView> {
        Ok(match o {
            ClaimObject::Entity(id) => ClaimObjectView {
                entity: Some(self.link(id)?),
                literal: None,
            },
            ClaimObject::Literal(l) => ClaimObjectView {
                entity: None,
                literal: Some(l.clone()),
            },
        })
    }

    /// Distinct `(source, sensitive)` over a claim's evidence.
    pub fn claim_origins(&mut self, c: &Claim) -> Result<Vec<Origin>> {
        let mut out = BTreeSet::new();
        for ev in &c.evidence {
            let info = self.item(&ev.item)?;
            out.insert(Origin {
                source: ev.item.source.clone(),
                sensitive: info.sensitive,
            });
        }
        Ok(out.into_iter().collect())
    }

    /// Distinct origins over every active claim's evidence and the entity's mentions.
    pub fn entity_origins(&mut self, id: &EntityId) -> Result<Vec<Origin>> {
        let claims = self.graph.claims_for(id, false)?;
        let mentions = self.graph.mentions_for(id, ORIGIN_MENTIONS)?;
        self.origins_of(&claims, &mentions)
    }

    pub fn origins_of(&mut self, claims: &[Claim], mentions: &[Mention]) -> Result<Vec<Origin>> {
        let mut items: HashSet<&ItemRef> = HashSet::new();
        for c in claims {
            items.extend(c.evidence.iter().map(|e| &e.item));
        }
        items.extend(mentions.iter().map(|m| &m.item));
        let mut out = BTreeSet::new();
        for item in items {
            let info = self.item(item)?;
            out.insert(Origin {
                source: item.source.clone(),
                sensitive: info.sensitive,
            });
        }
        Ok(out.into_iter().collect())
    }
}

/// The item's `url` metadata, if it is a sane single-line string.
pub(crate) fn item_url(metadata: &Value) -> Option<String> {
    let url = metadata.get("url")?.as_str()?.trim();
    (!url.is_empty() && url.len() <= 2048 && !url.chars().any(char::is_control))
        .then(|| url.to_owned())
}

/// `active`, `superseded`, ... (serde names, so new statuses map without code changes).
pub(crate) fn status_str(s: ClaimStatus) -> String {
    match serde_json::to_value(s) {
        Ok(Value::String(s)) => s,
        _ => format!("{s:?}").to_lowercase(),
    }
}

/// The status as IPC shows it: the four statuses of the IPC contract, with a user-rejected
/// claim (`rejected`, where the graph has that status) shown as `retracted`.
pub(crate) fn ipc_status(s: ClaimStatus) -> String {
    match status_str(s).as_str() {
        "rejected" => "retracted".to_owned(),
        other => other.to_owned(),
    }
}

/// The status a user's "reject this fact" sets: the sticky `rejected` status when the graph
/// has it (so re-extraction never revives the fact), else `retracted`.
pub(crate) fn user_rejected() -> ClaimStatus {
    parse_status("rejected").unwrap_or(ClaimStatus::Retracted)
}

pub(crate) fn parse_status(s: &str) -> Result<ClaimStatus> {
    serde_json::from_value(Value::String(s.to_owned()))
        .map_err(|_| Error::Invalid(format!("unknown claim status `{s}`")))
}

pub(crate) fn parse_kind(s: &str) -> Result<EntityKind> {
    serde_json::from_value(Value::String(s.trim().to_ascii_lowercase()))
        .map_err(|_| Error::Invalid(format!("unknown entity kind `{s}`")))
}

pub(crate) fn summary_view(s: EntitySummary) -> EntitySummaryView {
    EntitySummaryView {
        id: s.id,
        kind: s.kind.as_str().to_owned(),
        name: s.name,
        claims: s.claims,
        mentions: s.mentions,
        updated_at: s.updated_at,
    }
}

pub(crate) fn claim_view(lk: &mut Lookups<'_>, c: &Claim) -> Result<ClaimView> {
    let mut evidence = Vec::with_capacity(c.evidence.len());
    for ev in &c.evidence {
        let info = lk.item(&ev.item)?;
        evidence.push(EvidenceView {
            item: ev.item.clone(),
            chunk_id: ev.chunk_id,
            quote: ev.quote.clone(),
            title: info.title,
            url: info.url,
        });
    }
    Ok(ClaimView {
        id: c.id,
        subject: lk.link(&c.subject)?,
        predicate: c.predicate.clone(),
        object: lk.object(&c.object)?,
        text: c.text.clone(),
        confidence: c.confidence,
        status: ipc_status(c.status),
        evidence,
    })
}

pub(crate) fn mention_view(lk: &mut Lookups<'_>, m: &Mention) -> Result<MentionView> {
    let info = lk.item(&m.item)?;
    Ok(MentionView {
        item: m.item.clone(),
        chunk_id: m.chunk_id,
        surface: m.surface.clone(),
        title: info.title,
        url: info.url,
    })
}

/// Every live entity matching `kinds` (empty = all), up to [`MAX_SCAN`].
pub(crate) fn scan_entities(
    graph: &dyn GraphStore,
    text: Option<&str>,
    kinds: &[EntityKind],
) -> Result<(Vec<EntitySummary>, bool)> {
    let mut out = Vec::new();
    let kind = (kinds.len() == 1).then(|| kinds[0]);
    let mut offset = 0;
    loop {
        #[allow(clippy::needless_update)] // future fields keep their defaults
        let batch = graph.list_entities(&EntityQuery {
            text: text.map(str::to_owned),
            kind,
            limit: SCAN_PAGE,
            offset,
            ..Default::default()
        })?;
        let n = batch.len();
        out.extend(
            batch
                .into_iter()
                .filter(|e| kinds.is_empty() || kinds.contains(&e.kind)),
        );
        offset += n;
        if n < SCAN_PAGE {
            return Ok((out, false));
        }
        if offset >= MAX_SCAN {
            return Ok((out, true));
        }
    }
}

/// Graph overview: the `limit` most connected entities (by facts, then mentions) of `kinds`,
/// with the active entity-to-entity claims between them.
pub(crate) fn overview(
    graph: &dyn GraphStore,
    kinds: &[EntityKind],
    limit: usize,
) -> Result<GraphViewIpc> {
    let (mut all, scan_cut) = scan_entities(graph, None, kinds)?;
    all.sort_by(|a, b| {
        b.claims
            .cmp(&a.claims)
            .then(b.mentions.cmp(&a.mentions))
            .then(a.name.cmp(&b.name))
    });
    let truncated = scan_cut || all.len() > limit;
    all.truncate(limit);
    let ids: HashSet<EntityId> = all.iter().map(|e| e.id.clone()).collect();
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for e in &all {
        for c in graph.claims_for(&e.id, false)? {
            let ClaimObject::Entity(to) = &c.object else {
                continue;
            };
            if ids.contains(&c.subject) && ids.contains(to) && seen.insert(c.id) {
                edges.push(GraphEdgeView {
                    claim: c.id,
                    from: c.subject.clone(),
                    to: to.clone(),
                    predicate: c.predicate.clone(),
                });
            }
        }
    }
    edges.sort_by_key(|e| e.claim);
    Ok(GraphViewIpc {
        nodes: all
            .into_iter()
            .map(|e| GraphNodeView {
                id: e.id,
                kind: e.kind.as_str().to_owned(),
                name: e.name,
                depth: 0,
            })
            .collect(),
        edges,
        truncated,
    })
}

/// Neighbourhood of `center`, keeping only nodes of `kinds` (the centre always stays) and the
/// edges between kept nodes.
pub(crate) fn neighbourhood(
    graph: &dyn GraphStore,
    center: &EntityId,
    depth: u32,
    limit: usize,
    kinds: &[EntityKind],
) -> Result<GraphViewIpc> {
    let center = graph
        .get_entity(center)?
        .ok_or_else(|| Error::NotFound(format!("entity {center}")))?
        .id;
    let view = graph.neighbourhood(&center, depth, limit)?;
    let keep: HashSet<EntityId> = view
        .nodes
        .iter()
        .filter(|n| n.id == center || kinds.is_empty() || kinds.contains(&n.kind))
        .map(|n| n.id.clone())
        .collect();
    Ok(GraphViewIpc {
        nodes: view
            .nodes
            .into_iter()
            .filter(|n| keep.contains(&n.id))
            .map(|n| GraphNodeView {
                id: n.id,
                kind: n.kind.as_str().to_owned(),
                name: n.name,
                depth: n.depth,
            })
            .collect(),
        edges: view
            .edges
            .into_iter()
            .filter(|e| keep.contains(&e.from) && keep.contains(&e.to))
            .map(|e| GraphEdgeView {
                claim: e.claim,
                from: e.from,
                to: e.to,
                predicate: e.predicate,
            })
            .collect(),
        truncated: view.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_kinds_round_trip_by_name() {
        for s in ["active", "superseded", "retracted", "user_asserted"] {
            assert_eq!(status_str(parse_status(s).unwrap()), s);
        }
        assert!(parse_status("bogus").is_err());
        assert_eq!(parse_kind("Person").unwrap(), EntityKind::Person);
        assert!(parse_kind("planet").is_err());
    }

    #[test]
    fn urls_are_sanitised() {
        assert_eq!(
            item_url(&serde_json::json!({"url": " file:///a.md "})).as_deref(),
            Some("file:///a.md")
        );
        assert_eq!(item_url(&serde_json::json!({"url": "a\nb"})), None);
        assert_eq!(item_url(&serde_json::json!({})), None);
    }
}
