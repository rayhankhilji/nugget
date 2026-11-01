//! Graph types → IPC views and MCP records, with evidence titles, urls and sensitivity read from
//! the store. Everything here is synchronous (run it on the blocking pool).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use nugget_core::ids::{EntityId, ItemRef, SourceId};
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
    /// `nugget_retrieve::is_sensitive` of the item (what MCP and cloud paths use: unsorted items
    /// count); missing items count as sensitive.
    pub sensitive: bool,
    /// `nugget_retrieve::is_confirmed_sensitive`: what the graph and entity views hide or badge
    /// (unsorted items are shown); missing items count as sensitive.
    pub confirmed_sensitive: bool,
    /// Source modification time, else when the item was indexed.
    pub observed_at: Option<time::OffsetDateTime>,
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
                confirmed_sensitive: nugget_retrieve::is_confirmed_sensitive(&it.metadata),
                observed_at: Some(it.source_modified_at.unwrap_or(it.created_at)),
                title: it.title,
            },
            None => ItemInfo {
                sensitive: true,
                confirmed_sensitive: true,
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

/// Whether an evidence or mention item is sensitive for the graph and entity views. [`Lookups`]
/// answers from the store (`nugget_retrieve::is_confirmed_sensitive`: items triage has not
/// sorted yet are shown; a missing item counts as sensitive).
pub(crate) trait ItemSensitivity {
    fn item_sensitive(&mut self, item: &ItemRef) -> Result<bool>;
}

impl ItemSensitivity for Lookups<'_> {
    fn item_sensitive(&mut self, item: &ItemRef) -> Result<bool> {
        Ok(self.item(item)?.confirmed_sensitive)
    }
}

/// How the graph views treat sensitive-only entities and claims: left out, unless `include`
/// (then nodes are marked).
pub(crate) struct Sensitivity<'a> {
    pub items: &'a mut dyn ItemSensitivity,
    pub include: bool,
}

/// Sensitive-only claim: it has evidence and every piece of it is from a sensitive item. A claim
/// without evidence (the user's own) is not.
pub(crate) fn claim_sensitive(sens: &mut dyn ItemSensitivity, c: &Claim) -> Result<bool> {
    if c.evidence.is_empty() {
        return Ok(false);
    }
    for ev in &c.evidence {
        if !sens.item_sensitive(&ev.item)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Sensitive-only entity: at least one active claim (as subject or object) or mention, every
/// such claim sensitive-only and every mention from a sensitive item. The vault gives such an
/// entity no page and default-scope MCP clients cannot see it (same rule as
/// [`Lookups::entity_origins`]).
pub(crate) fn entity_sensitive(
    graph: &dyn GraphStore,
    sens: &mut dyn ItemSensitivity,
    id: &EntityId,
) -> Result<bool> {
    let claims = graph.claims_for(id, false)?;
    for c in &claims {
        if !claim_sensitive(sens, c)? {
            return Ok(false);
        }
    }
    let mentions = graph.mentions_for(id, ORIGIN_MENTIONS)?;
    for m in &mentions {
        if !sens.item_sensitive(&m.item)? {
            return Ok(false);
        }
    }
    Ok(!claims.is_empty() || !mentions.is_empty())
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
    let mut sensitive = !c.evidence.is_empty();
    for ev in &c.evidence {
        let info = lk.item(&ev.item)?;
        sensitive &= info.confirmed_sensitive;
        evidence.push(EvidenceView {
            item: ev.item.clone(),
            chunk_id: ev.chunk_id,
            quote: ev.quote.clone(),
            title: info.title,
            url: info.url,
            observed_at: info.observed_at,
        });
    }
    Ok(ClaimView {
        id: c.id,
        subject: lk.link(&c.subject)?,
        predicate: c.predicate.clone(),
        object: lk.object(&c.object)?,
        text: c.text.clone(),
        confidence: c.confidence,
        status: status_str(c.status),
        evidence,
        updated_at: Some(c.updated_at),
        sensitive,
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

/// Graph overview: the `limit` most connected entities (by facts, then mentions, then name) of
/// `kinds`, with the active entity-to-entity claims between them. Without `include_sensitive`,
/// sensitive-only entities and claims (see [`entity_sensitive`]) are left out; with it, nodes
/// are marked. `truncated`: more entities (supported by `source`, when given) exist than were
/// considered for the `limit` shown.
///
/// Entities come most connected first from [`GraphStore::top_entities`] (an index range in
/// `SqliteGraph`), a page at a time, until `limit` are kept.
pub(crate) fn overview(
    graph: &dyn GraphStore,
    kinds: &[EntityKind],
    limit: usize,
    source: Option<&SourceId>,
    sensitivity: Sensitivity<'_>,
) -> Result<GraphViewIpc> {
    let Sensitivity {
        items: sens,
        include: include_sensitive,
    } = sensitivity;
    // One query for the supported set.
    let supported: Option<HashSet<EntityId>> = match source {
        Some(src) => Some(graph.entities_supported_by(src)?.into_iter().collect()),
        None => None,
    };
    let mut page = limit.saturating_mul(2).max(OVERVIEW_PAGE);
    let mut kept = Vec::with_capacity(limit.min(page));
    let mut seen = HashSet::new();
    let mut cut = false;
    let mut offset = 0;
    'pages: loop {
        let batch = graph.top_entities(kinds, page, offset)?;
        let n = batch.len();
        offset += n;
        for e in batch {
            // Pages are separate reads: a write in between may shift an entity across a page
            // boundary, so never show one twice.
            if !seen.insert(e.id.clone()) {
                continue;
            }
            if supported.as_ref().is_some_and(|s| !s.contains(&e.id)) {
                continue;
            }
            if kept.len() >= limit {
                cut = true;
                break 'pages;
            }
            let sensitive = entity_sensitive(graph, sens, &e.id)?;
            if sensitive && !include_sensitive {
                continue;
            }
            kept.push((e, sensitive));
        }
        if n < page {
            break;
        }
        // Each page is an OFFSET read that re-walks the rows before it: grow the pages, so
        // walking far (a selective source or kind filter, a mostly sensitive graph) takes a
        // few reads instead of one per hundred entities.
        page = page.saturating_mul(4);
    }
    overview_view(graph, kept, cut, source, sens, include_sensitive)
}

/// Smallest page [`overview`] reads from [`GraphStore::top_entities`].
const OVERVIEW_PAGE: usize = 100;

/// The overview's nodes (`kept`, most connected first, each with whether it is sensitive-only)
/// and the active entity-to-entity claims between them.
fn overview_view(
    graph: &dyn GraphStore,
    kept: Vec<(EntitySummary, bool)>,
    truncated: bool,
    source: Option<&SourceId>,
    sens: &mut dyn ItemSensitivity,
    include_sensitive: bool,
) -> Result<GraphViewIpc> {
    let ids: HashSet<EntityId> = kept.iter().map(|(e, _)| e.id.clone()).collect();
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for (e, _) in &kept {
        for c in graph.claims_for(&e.id, false)? {
            let ClaimObject::Entity(to) = &c.object else {
                continue;
            };
            if source.is_some_and(|s| !claim_from(&c, s)) {
                continue;
            }
            if !(ids.contains(&c.subject) && ids.contains(to)) || seen.contains(&c.id) {
                continue;
            }
            if !include_sensitive && claim_sensitive(sens, &c)? {
                continue;
            }
            if seen.insert(c.id) {
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
        nodes: kept
            .into_iter()
            .map(|(e, sensitive)| GraphNodeView {
                id: e.id,
                kind: e.kind.as_str().to_owned(),
                name: e.name,
                depth: 0,
                sensitive,
            })
            .collect(),
        edges,
        truncated,
    })
}

/// Whether a claim has evidence from `source`.
pub(crate) fn claim_from(c: &Claim, source: &SourceId) -> bool {
    c.evidence.iter().any(|e| &e.item.source == source)
}

/// Whether items of `source` support the entity: a mention from it, or an active claim with
/// evidence from it.
pub(crate) fn supported_by(
    graph: &dyn GraphStore,
    id: &EntityId,
    source: &SourceId,
) -> Result<bool> {
    if graph
        .claims_for(id, false)?
        .iter()
        .any(|c| claim_from(c, source))
    {
        return Ok(true);
    }
    Ok(graph
        .mentions_for(id, ORIGIN_MENTIONS)?
        .iter()
        .any(|m| &m.item.source == source))
}

/// Neighbourhood of `center`, keeping only nodes of `kinds` (and, with `source`, nodes supported
/// by that source; the centre always stays) and the edges between kept nodes (with `source`,
/// only edges whose claim has evidence from it). Without `include_sensitive`, sensitive-only
/// entities (except the centre, which is marked) and claims are left out, and so is whatever
/// was reachable only through them; with it, nodes are marked.
pub(crate) fn neighbourhood(
    graph: &dyn GraphStore,
    center: &EntityId,
    depth: u32,
    limit: usize,
    kinds: &[EntityKind],
    source: Option<&SourceId>,
    sensitivity: Sensitivity<'_>,
) -> Result<GraphViewIpc> {
    let Sensitivity {
        items: sens,
        include: include_sensitive,
    } = sensitivity;
    let center = graph
        .get_entity(center)?
        .ok_or_else(|| Error::NotFound(format!("entity {center}")))?
        .id;
    let mut view = graph.neighbourhood(&center, depth, limit)?;
    let mut keep: HashMap<EntityId, bool> = HashMap::new();
    // Sensitive-only nodes (never the centre) and claims left out.
    let mut hidden: HashSet<EntityId> = HashSet::new();
    for n in &view.nodes {
        let sensitive = entity_sensitive(graph, sens, &n.id)?;
        if sensitive && !include_sensitive && n.id != center {
            hidden.insert(n.id.clone());
            continue;
        }
        let wanted = n.id == center
            || ((kinds.is_empty() || kinds.contains(&n.kind))
                && match source {
                    Some(s) => supported_by(graph, &n.id, s)?,
                    None => true,
                });
        if wanted {
            keep.insert(n.id.clone(), sensitive);
        }
    }
    let mut hidden_claims = HashSet::new();
    if !include_sensitive {
        for e in &view.edges {
            if let Some(c) = graph.get_claim(e.claim)?
                && claim_sensitive(sens, &c)?
            {
                hidden_claims.insert(e.claim);
            }
        }
    }
    if !hidden.is_empty() || !hidden_claims.is_empty() {
        // Re-walk from the centre without what is hidden, so nothing reachable only through a
        // sensitive node or claim stays, and recompute depths.
        view.edges.retain(|e| !hidden_claims.contains(&e.claim));
        let mut depths: HashMap<EntityId, u32> = HashMap::from([(center.clone(), 0)]);
        let mut queue = VecDeque::from([center.clone()]);
        while let Some(id) = queue.pop_front() {
            let d = depths[&id];
            for e in &view.edges {
                let other = if e.from == id {
                    &e.to
                } else if e.to == id {
                    &e.from
                } else {
                    continue;
                };
                if !hidden.contains(other) && !depths.contains_key(other) {
                    depths.insert(other.clone(), d + 1);
                    queue.push_back(other.clone());
                }
            }
        }
        keep.retain(|id, _| depths.contains_key(id));
        for n in &mut view.nodes {
            if let Some(d) = depths.get(&n.id) {
                n.depth = *d;
            }
        }
    }
    if let Some(s) = source {
        let mut edges = Vec::with_capacity(view.edges.len());
        for e in view.edges {
            if graph.get_claim(e.claim)?.is_some_and(|c| claim_from(&c, s)) {
                edges.push(e);
            }
        }
        view.edges = edges;
    }
    Ok(GraphViewIpc {
        nodes: view
            .nodes
            .into_iter()
            .filter_map(|n| {
                let sensitive = *keep.get(&n.id)?;
                Some(GraphNodeView {
                    id: n.id,
                    kind: n.kind.as_str().to_owned(),
                    name: n.name,
                    depth: n.depth,
                    sensitive,
                })
            })
            .collect(),
        edges: view
            .edges
            .into_iter()
            .filter(|e| keep.contains_key(&e.from) && keep.contains_key(&e.to))
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
    use nugget_graph::{MemGraph, NewEntity};

    /// Items in the set are sensitive.
    struct Sens(HashSet<ItemRef>);

    impl ItemSensitivity for Sens {
        fn item_sensitive(&mut self, item: &ItemRef) -> Result<bool> {
            Ok(self.0.contains(item))
        }
    }

    fn item(source: &str, key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new(source),
            key: nugget_core::ids::ItemKey::new(key),
        }
    }

    fn entity(g: &MemGraph, kind: EntityKind, name: &str) -> EntityId {
        g.create_entity(&NewEntity::new(kind, name)).unwrap()
    }

    fn link(g: &dyn GraphStore, a: &EntityId, b: &EntityId, pred: &str, it: &ItemRef) -> i64 {
        g.add_claim(&nugget_graph::NewClaim {
            subject: a.clone(),
            predicate: pred.into(),
            object: ClaimObject::Entity(b.clone()),
            text: pred.into(),
            confidence: 0.9,
            status: ClaimStatus::Active,
            evidence: vec![nugget_graph::Evidence {
                item: it.clone(),
                chunk_id: None,
                quote: None,
            }],
            valid_from: None,
            valid_to: None,
        })
        .unwrap()
    }

    fn mention(g: &MemGraph, e: &EntityId, it: ItemRef) {
        g.add_mention(&Mention {
            entity: e.clone(),
            item: it,
            chunk_id: None,
            surface: "s".into(),
            confidence: 1.0,
        })
        .unwrap();
    }

    fn ov(
        g: &MemGraph,
        limit: usize,
        src: Option<&str>,
        sens: &mut Sens,
        include: bool,
    ) -> GraphViewIpc {
        let src = src.map(SourceId::new);
        let items: &mut dyn ItemSensitivity = sens;
        overview(g, &[], limit, src.as_ref(), Sensitivity { items, include }).unwrap()
    }

    fn nb(
        g: &MemGraph,
        centre: &EntityId,
        src: Option<&str>,
        sens: &mut Sens,
        include: bool,
    ) -> GraphViewIpc {
        let src = src.map(SourceId::new);
        let items: &mut dyn ItemSensitivity = sens;
        neighbourhood(
            g,
            centre,
            2,
            50,
            &[],
            src.as_ref(),
            Sensitivity { items, include },
        )
        .unwrap()
    }

    fn names(v: &GraphViewIpc) -> BTreeSet<&str> {
        v.nodes.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn urls_are_sanitised() {
        assert_eq!(
            item_url(&serde_json::json!({"url": " file:///a.md "})).as_deref(),
            Some("file:///a.md")
        );
        assert_eq!(item_url(&serde_json::json!({"url": "a\nb"})), None);
    }

    #[test]
    fn sensitive_only_nodes_and_edges_are_hidden_unless_asked_for() {
        let g = MemGraph::new();
        let mara = entity(&g, EntityKind::Person, "Mara");
        let acme = entity(&g, EntityKind::Organization, "Acme");
        let session = entity(&g, EntityKind::Event, "Counselling session");
        let clinic = entity(&g, EntityKind::Place, "Clinic");
        let bob = entity(&g, EntityKind::Person, "Bob");
        let (mail, invite) = (item("mail", "m1"), item("files", "invite.ics"));
        let works_at = link(&g, &mara, &acme, "works_at", &mail);
        link(&g, &mara, &session, "attended", &invite);
        link(&g, &session, &clinic, "located_at", &invite);
        // A sensitive-only edge between two entities that stay visible.
        link(&g, &mara, &bob, "knows", &invite);
        link(&g, &bob, &acme, "works_at", &mail);
        let sens = &mut Sens(HashSet::from([invite.clone()]));
        let shown = BTreeSet::from(["Acme", "Bob", "Mara"]);

        let hidden = ov(&g, 10, None, sens, false);
        assert_eq!(names(&hidden), shown);
        assert!(hidden.nodes.iter().all(|n| !n.sensitive));
        assert_eq!(hidden.edges.len(), 2, "{:?}", hidden.edges);
        assert!(hidden.edges.iter().all(|e| e.predicate == "works_at"));
        let all = ov(&g, 10, None, sens, true);
        assert_eq!((all.nodes.len(), all.edges.len()), (5, 5));
        let marked: BTreeSet<&str> = all
            .nodes
            .iter()
            .filter(|n| n.sensitive)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(marked, BTreeSet::from(["Clinic", "Counselling session"]));
        // `limit` counts shown nodes only.
        let two = ov(&g, 2, None, sens, false);
        assert!(two.nodes.len() == 2 && two.nodes.iter().all(|n| !n.sensitive));

        let n = nb(&g, &mara, None, sens, false);
        assert_eq!(names(&n), shown);
        // Bob is two hops away (through Acme), not one (through the hidden edge).
        assert_eq!(n.nodes.iter().find(|x| x.id == bob).unwrap().depth, 2);
        assert!(n.edges.iter().any(|e| e.claim == works_at));
        assert!(n.edges.iter().all(|e| e.predicate == "works_at"));
        assert_eq!(nb(&g, &mara, None, sens, true).nodes.len(), 5);
        // A sensitive centre stays, marked, alone.
        let c = nb(&g, &session, None, sens, false);
        assert_eq!(c.nodes.len(), 1);
        assert!(c.nodes[0].sensitive && c.nodes[0].id == session);
        assert!(c.edges.is_empty());

        // The rule: an evidence-free (user) claim is not sensitive; a mention counts.
        assert!(entity_sensitive(&g, sens, &clinic).unwrap());
        mention(&g, &clinic, mail);
        assert!(!entity_sensitive(&g, sens, &clinic).unwrap());
        let lonely = entity(&g, EntityKind::Topic, "Nothing yet");
        assert!(!entity_sensitive(&g, sens, &lonely).unwrap());
    }
}
