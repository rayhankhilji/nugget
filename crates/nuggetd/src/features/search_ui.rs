//! II2: faceted search, entity timelines and item previews for the desktop's Search page.
//!
//! - `search.faceted`: the retriever's ranking (untouched) over a pool of candidates, filtered
//!   by source, kind, people and a local date range, with disjunctive facet counts.
//! - `search.timeline`: every item that mentions an entity or is evidence for one of its active
//!   facts, plus the facts themselves, in time order.
//! - `search.preview`: an item's text, the passage a hit pointed at, the people it mentions and
//!   the facts it supports.
//!
//! Everything is local: no model calls, nothing leaves the device. Sensitive items are flagged
//! for the (local) UI, never filtered or sent anywhere. Shapes are documented on the result
//! types below; the pure parts (kinds, dates, facets, timeline assembly) live in [`logic`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_core::ipc::SearchHit;
use nugget_core::{Error, Result};
use nugget_graph::{ClaimStatus, Entity, GraphStore};
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_store::{EntityLinkRow, Item, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{OffsetDateTime, UtcOffset};

use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};
use crate::graphview::{item_url, status_str};

mod logic;

use logic::{
    Candidate, DateSource, Dated, DayRange, FacetCount, FacetFilters, FactDateSource, ItemKind,
    PersonFacet, SelectedPerson,
};

const FACETED: &str = "search.faceted";
const TIMELINE: &str = "search.timeline";
const PREVIEW: &str = "search.preview";

/// Candidates asked of the retriever for one faceted search.
const POOL: usize = 300;
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;
/// Values per filter list at most.
const MAX_FILTER_VALUES: usize = 50;
/// Longest filter value, entity id or item key part accepted (bytes).
const MAX_VALUE_LEN: usize = 1024;
/// Longest query accepted (bytes).
const MAX_QUERY_LEN: usize = 4096;
/// People in the people facet (by count), besides the selected ones.
const TOP_PEOPLE: usize = 20;
/// People listed on a hit.
const HIT_PEOPLE: usize = 5;

const DEFAULT_TIMELINE_LIMIT: usize = 300;
const MAX_TIMELINE_LIMIT: usize = 1000;
/// Mentions read for one timeline.
const TIMELINE_MENTIONS: usize = 5000;
/// Evidence entries shown per fact.
const FACT_EVIDENCE: usize = 10;
/// Snippet length of a timeline item (characters).
const SNIPPET_CHARS: usize = 240;

/// Preview text length (characters).
const PREVIEW_CHARS: usize = 20_000;
const PREVIEW_PEOPLE: usize = 50;
const PREVIEW_FACTS: usize = 50;

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    Ok(Arc::new(SearchUi {
        state: cx.state.clone(),
    }))
}

struct SearchUi {
    state: Arc<State>,
}

#[async_trait]
impl Feature for SearchUi {
    fn name(&self) -> &'static str {
        "search_ui"
    }

    fn methods(&self) -> &'static [&'static str] {
        &[FACETED, TIMELINE, PREVIEW]
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        let params = if params.is_null() {
            Value::Object(Default::default())
        } else {
            params
        };
        match method {
            FACETED => to_result(self.faceted(parse_params(params)?).await?),
            TIMELINE => to_result(self.timeline(parse_params(params)?).await?),
            PREVIEW => to_result(self.preview(parse_params(params)?).await?),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

// ---------------------------------------------------------------- shared views

/// `{id, name, kind}` of a live entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EntityRefView {
    id: String,
    name: String,
    kind: String,
}

impl From<&EntityLinkRow> for EntityRefView {
    fn from(r: &EntityLinkRow) -> Self {
        Self {
            id: r.id.to_string(),
            name: r.name.clone(),
            kind: r.kind.as_str().to_owned(),
        }
    }
}

impl From<&Entity> for EntityRefView {
    fn from(e: &Entity) -> Self {
        Self {
            id: e.id.to_string(),
            name: e.name.clone(),
            kind: e.kind.as_str().to_owned(),
        }
    }
}

fn check_len(field: &str, s: &str) -> Result<()> {
    if s.len() > MAX_VALUE_LEN {
        return Err(Error::Invalid(format!(
            "{field}: longer than {MAX_VALUE_LEN} bytes"
        )));
    }
    Ok(())
}

fn check_list(field: &str, list: &[String]) -> Result<()> {
    if list.len() > MAX_FILTER_VALUES {
        return Err(Error::Invalid(format!(
            "{field}: at most {MAX_FILTER_VALUES} values"
        )));
    }
    for v in list {
        check_len(field, v)?;
    }
    Ok(())
}

fn check_item(field: &str, item: &ItemRef) -> Result<()> {
    if item.source.as_str().is_empty() || item.key.as_str().is_empty() {
        return Err(Error::Invalid(format!(
            "{field}: source and key are required"
        )));
    }
    check_len(field, item.source.as_str())?;
    check_len(field, item.key.as_str())
}

/// `limit` clamped to `1..=max` (0 or unset: `default`).
fn clamp_limit(limit: Option<u32>, default: usize, max: usize) -> usize {
    match limit {
        None | Some(0) => default,
        Some(n) => (n as usize).min(max),
    }
}

/// Run a future that does blocking store work on the blocking pool.
async fn on_blocking_thread<T, F>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    blocking(move || handle.block_on(fut)).await
}

// ---------------------------------------------------------------- search.faceted

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FacetedParams {
    query: String,
    limit: Option<u32>,
    offset: Option<u32>,
    filters: Filters,
    utc_offset_minutes: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Filters {
    sources: Vec<String>,
    kinds: Vec<String>,
    people: Vec<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Debug, Serialize)]
struct FacetedHit {
    #[serde(flatten)]
    hit: SearchHit,
    date: Option<String>,
    date_source: DateSource,
    kind: ItemKind,
    sensitive: bool,
    people: Vec<EntityRefView>,
}

#[derive(Debug, Default, Serialize)]
struct Facets {
    sources: Vec<FacetCount>,
    kinds: Vec<FacetCount>,
    people: Vec<PersonFacet>,
    months: Vec<FacetCount>,
}

#[derive(Debug, Serialize)]
struct FacetedResult {
    query: String,
    hits: Vec<FacetedHit>,
    total: usize,
    pool: usize,
    pool_limited: bool,
    facets: Facets,
}

/// One pool item, loaded.
struct PoolItem {
    hit: SearchHit,
    at: OffsetDateTime,
    date_source: DateSource,
    kind: ItemKind,
    sensitive: bool,
    people: Vec<EntityLinkRow>,
}

impl SearchUi {
    fn offset(&self, minutes: Option<i32>) -> Result<UtcOffset> {
        logic::offset_from_minutes(
            minutes.unwrap_or(self.state.retriever.config().date_utc_offset_minutes),
        )
    }

    /// Source id → connector kind, for kind classification. Best effort: a failing list
    /// classifies by metadata and mime only.
    async fn source_kinds(&self) -> HashMap<String, String> {
        match self.state.ingest.list().await {
            Ok(list) => list
                .into_iter()
                .map(|s| (s.id.as_str().to_owned(), s.kind))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "search_ui: could not list sources");
                HashMap::new()
            }
        }
    }

    async fn faceted(&self, p: FacetedParams) -> Result<FacetedResult> {
        if p.query.len() > MAX_QUERY_LEN {
            return Err(Error::Invalid(format!(
                "query: longer than {MAX_QUERY_LEN} bytes"
            )));
        }
        let f = &p.filters;
        check_list("filters.sources", &f.sources)?;
        check_list("filters.kinds", &f.kinds)?;
        check_list("filters.people", &f.people)?;
        let kinds: HashSet<ItemKind> = f
            .kinds
            .iter()
            .map(|k| {
                ItemKind::parse(k).ok_or_else(|| {
                    Error::Invalid(format!(
                        "filters.kinds: unknown kind {k:?} (mail, event, chat or file)"
                    ))
                })
            })
            .collect::<Result<_>>()?;
        let offset = self.offset(p.utc_offset_minutes)?;
        let range = logic::day_range("filters.", f.from.as_deref(), f.to.as_deref(), offset)?;
        let limit = clamp_limit(p.limit, DEFAULT_LIMIT, MAX_LIMIT);
        let skip = p.offset.unwrap_or(0) as usize;

        if p.query.trim().is_empty() {
            return Ok(FacetedResult {
                query: p.query,
                hits: Vec::new(),
                total: 0,
                pool: 0,
                pool_limited: false,
                facets: Facets::default(),
            });
        }

        let retriever = self.state.retriever.clone();
        let query = p.query.clone();
        let results =
            on_blocking_thread(async move { retriever.search(&query, POOL).await }).await?;
        let pool_limited = results.items.len() >= POOL;
        let hits = results.hits();
        let source_kinds = self.source_kinds().await;

        let store = self.state.store.clone();
        let graph = self.state.knowledge.graph.clone();
        let wanted_people = f.people.clone();
        let (items, selected, selected_names) = blocking(move || {
            let items = load_pool(&store, hits, &source_kinds, offset)?;
            let (selected, names) = resolve_people(graph.as_ref(), &wanted_people)?;
            Ok((items, selected, names))
        })
        .await?;

        let filters = FacetFilters {
            sources: f.sources.iter().cloned().collect(),
            kinds,
            // An unknown person stays in the filter and matches nothing (rather than being
            // ignored).
            people: selected
                .iter()
                .map(|s| s.live.clone().unwrap_or_else(|| s.given.clone()))
                .collect(),
            range,
        };
        let cands: Vec<Candidate> = items
            .iter()
            .map(|i| Candidate {
                source: i.hit.item.source.as_str().to_owned(),
                kind: i.kind,
                people: i.people.iter().map(|p| p.id.to_string()).collect(),
                at: i.at,
            })
            .collect();
        let (matching, tally) = logic::facet(&cands, &filters, offset);

        let mut names: HashMap<String, (String, String)> = selected_names;
        for i in &items {
            for p in &i.people {
                names
                    .entry(p.id.to_string())
                    .or_insert_with(|| (p.name.clone(), p.kind.as_str().to_owned()));
            }
        }
        let facets = Facets {
            sources: logic::with_selected(tally.sources, &f.sources),
            kinds: logic::with_selected(tally.kinds, &f.kinds),
            people: logic::people_facet(&tally.people, &selected, &names, TOP_PEOPLE),
            months: tally.months,
        };

        let total = matching.len();
        let pool = items.len();
        let mut items: Vec<Option<PoolItem>> = items.into_iter().map(Some).collect();
        let hits = matching
            .into_iter()
            .skip(skip)
            .take(limit)
            .filter_map(|i| items.get_mut(i).and_then(Option::take))
            .map(|i| FacetedHit {
                date: Some(logic::rfc3339(i.at)),
                date_source: i.date_source,
                kind: i.kind,
                sensitive: i.sensitive,
                people: i
                    .people
                    .iter()
                    .take(HIT_PEOPLE)
                    .map(EntityRefView::from)
                    .collect(),
                hit: i.hit,
            })
            .collect();
        Ok(FacetedResult {
            query: p.query,
            hits,
            total,
            pool,
            pool_limited,
            facets,
        })
    }
}

/// Dates, items and mentioned entities of the pool, in retriever order. Hits whose item is gone
/// (deleted since the search) are dropped.
fn load_pool(
    store: &Store,
    hits: Vec<SearchHit>,
    source_kinds: &HashMap<String, String>,
    offset: UtcOffset,
) -> Result<Vec<PoolItem>> {
    let refs: Vec<ItemRef> = hits.iter().map(|h| h.item.clone()).collect();
    let dates = store.item_dates(&refs)?;
    let items = store.get_items(&refs)?;
    let mut people = store.entities_in_items(&refs)?;
    let mut out = Vec::with_capacity(hits.len());
    for (hit, item) in hits.into_iter().zip(items) {
        let (Some(item), Some(d)) = (item, dates.get(&hit.item)) else {
            continue;
        };
        let (at, date_source) = logic::own_date(d, offset);
        let kind = logic::classify_kind(
            &item.metadata,
            item.mime.as_deref(),
            source_kinds
                .get(hit.item.source.as_str())
                .map(String::as_str),
        );
        out.push(PoolItem {
            people: people.remove(&hit.item).unwrap_or_default(),
            sensitive: nugget_retrieve::is_sensitive(&item.metadata),
            hit,
            at,
            date_source,
            kind,
        });
    }
    Ok(out)
}

/// The selected people resolved to live ids, and `(name, kind)` of each known one (under both
/// the given and the live id).
#[allow(clippy::type_complexity)]
fn resolve_people(
    graph: &dyn GraphStore,
    given: &[String],
) -> Result<(Vec<SelectedPerson>, HashMap<String, (String, String)>)> {
    let mut selected = Vec::with_capacity(given.len());
    let mut names = HashMap::new();
    for g in given {
        let e = graph.get_entity(&EntityId::new(g.clone()))?;
        if let Some(e) = &e {
            let label = (e.name.clone(), e.kind.as_str().to_owned());
            names.insert(e.id.to_string(), label.clone());
            names.insert(g.clone(), label);
        }
        selected.push(SelectedPerson {
            given: g.clone(),
            live: e.map(|e| e.id.to_string()),
        });
    }
    Ok((selected, names))
}

// ---------------------------------------------------------------- search.timeline

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TimelineParams {
    entity: String,
    from: Option<String>,
    to: Option<String>,
    limit: Option<u32>,
    utc_offset_minutes: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TimelineEvent {
    Item {
        at: String,
        date_source: DateSource,
        item: ItemRef,
        title: Option<String>,
        snippet: String,
        kind: ItemKind,
        url: Option<String>,
        sensitive: bool,
    },
    Fact {
        at: String,
        date_source: FactDateSource,
        claim: FactView,
    },
}

#[derive(Debug, Serialize)]
struct FactView {
    id: i64,
    predicate: String,
    text: String,
    status: String,
    confidence: f32,
    evidence: Vec<EvidenceItemView>,
}

#[derive(Debug, Serialize)]
struct EvidenceItemView {
    item: ItemRef,
    title: Option<String>,
    quote: Option<String>,
}

#[derive(Debug, Serialize)]
struct TimelineResult {
    entity: EntityRefView,
    events: Vec<TimelineEvent>,
    months: Vec<FacetCount>,
    total: usize,
    truncated: bool,
}

/// A timeline event before rendering.
#[derive(Debug, Clone)]
enum Pending {
    Item {
        item: ItemRef,
        date_source: DateSource,
        /// Surface of the first mention, for items without text.
        surface: Option<String>,
    },
    Fact {
        claim: nugget_graph::Claim,
        date_source: FactDateSource,
    },
}

impl SearchUi {
    async fn timeline(&self, p: TimelineParams) -> Result<TimelineResult> {
        let id = p.entity.trim().to_owned();
        if id.is_empty() {
            return Err(Error::Invalid("entity: required".into()));
        }
        check_len("entity", &id)?;
        let offset = self.offset(p.utc_offset_minutes)?;
        let range = logic::day_range("", p.from.as_deref(), p.to.as_deref(), offset)?;
        let limit = clamp_limit(p.limit, DEFAULT_TIMELINE_LIMIT, MAX_TIMELINE_LIMIT);
        let source_kinds = self.source_kinds().await;
        let store = self.state.store.clone();
        let graph = self.state.knowledge.graph.clone();
        blocking(move || {
            build_timeline(
                &store,
                graph.as_ref(),
                &id,
                &range,
                limit,
                offset,
                &source_kinds,
            )
        })
        .await
    }
}

fn build_timeline(
    store: &Store,
    graph: &dyn GraphStore,
    id: &str,
    range: &DayRange,
    limit: usize,
    offset: UtcOffset,
    source_kinds: &HashMap<String, String>,
) -> Result<TimelineResult> {
    let entity = graph
        .get_entity(&EntityId::new(id))?
        .ok_or_else(|| Error::NotFound(format!("entity {id}")))?;
    let mentions = graph.mentions_for(&entity.id, TIMELINE_MENTIONS)?;
    let claims: Vec<_> = graph
        .claims_for(&entity.id, false)?
        .into_iter()
        .filter(|c| c.status.is_active())
        .collect();

    // Every item involved, with the first mention's surface.
    let mut order: Vec<ItemRef> = Vec::new();
    let mut surfaces: HashMap<ItemRef, Option<String>> = HashMap::new();
    for m in &mentions {
        if !surfaces.contains_key(&m.item) {
            order.push(m.item.clone());
            surfaces.insert(m.item.clone(), Some(m.surface.clone()));
        }
    }
    for c in &claims {
        for ev in &c.evidence {
            if !surfaces.contains_key(&ev.item) {
                order.push(ev.item.clone());
                surfaces.insert(ev.item.clone(), None);
            }
        }
    }
    let dates = store.item_dates(&order)?;
    let own = |item: &ItemRef| dates.get(item).map(|d| logic::own_date(d, offset));

    let mut events: Vec<Dated<Pending>> = Vec::new();
    for item in &order {
        // Items gone from the store (deleted, provenance not yet dropped) are left out.
        let Some((at, date_source)) = own(item) else {
            continue;
        };
        events.push(Dated {
            at,
            is_fact: false,
            // Titles are loaded for the kept events only; the key orders ties until then.
            tie: format!("{}\u{0}{}", item.source.as_str(), item.key.as_str()),
            payload: Pending::Item {
                item: item.clone(),
                date_source,
                surface: surfaces.get(item).cloned().flatten(),
            },
        });
    }
    for c in claims {
        let (at, date_source) = logic::fact_date(
            c.valid_from,
            c.evidence.iter().filter_map(|e| own(&e.item).map(|d| d.0)),
            c.created_at,
        );
        events.push(Dated {
            at,
            is_fact: true,
            tie: c.text.clone(),
            payload: Pending::Fact {
                claim: c,
                date_source,
            },
        });
    }
    let cut = logic::assemble(events, range, limit, offset);

    // Load the kept items and the evidence items of the kept facts.
    let mut wanted: Vec<ItemRef> = Vec::new();
    let mut seen: HashSet<ItemRef> = HashSet::new();
    for e in &cut.kept {
        match &e.payload {
            Pending::Item { item, .. } => {
                if seen.insert(item.clone()) {
                    wanted.push(item.clone());
                }
            }
            Pending::Fact { claim, .. } => {
                for ev in claim.evidence.iter().take(FACT_EVIDENCE) {
                    if seen.insert(ev.item.clone()) {
                        wanted.push(ev.item.clone());
                    }
                }
            }
        }
    }
    let loaded: HashMap<ItemRef, Item> = wanted
        .iter()
        .cloned()
        .zip(store.get_items(&wanted)?)
        .filter_map(|(r, i)| i.map(|i| (r, i)))
        .collect();

    let mut kept = cut.kept;
    // Final order with titles as the tie-break among items.
    for e in &mut kept {
        if let Pending::Item { item, .. } = &e.payload
            && let Some(title) = loaded.get(item).and_then(|i| i.title.as_deref())
        {
            e.tie = format!("{title}\u{0}{}", e.tie);
        }
    }
    kept.sort_by(logic::timeline_order);

    let events = kept
        .into_iter()
        .map(|e| {
            let at = logic::rfc3339(e.at);
            match e.payload {
                Pending::Item {
                    item,
                    date_source,
                    surface,
                } => {
                    let it = loaded.get(&item);
                    let text = it.and_then(|i| i.text.as_deref()).unwrap_or("");
                    let snippet = match logic::snippet(text, SNIPPET_CHARS) {
                        s if s.is_empty() => surface.unwrap_or_default(),
                        s => s,
                    };
                    TimelineEvent::Item {
                        at,
                        date_source,
                        title: it.and_then(|i| i.title.clone()),
                        snippet,
                        kind: it.map_or(ItemKind::File, |i| {
                            logic::classify_kind(
                                &i.metadata,
                                i.mime.as_deref(),
                                source_kinds.get(item.source.as_str()).map(String::as_str),
                            )
                        }),
                        url: it.and_then(|i| item_url(&i.metadata)),
                        sensitive: it.is_none_or(|i| nugget_retrieve::is_sensitive(&i.metadata)),
                        item,
                    }
                }
                Pending::Fact { claim, date_source } => TimelineEvent::Fact {
                    at,
                    date_source,
                    claim: FactView {
                        id: claim.id,
                        predicate: claim.predicate,
                        text: claim.text,
                        status: status_str(claim.status),
                        confidence: claim.confidence,
                        evidence: claim
                            .evidence
                            .into_iter()
                            .take(FACT_EVIDENCE)
                            .map(|ev| EvidenceItemView {
                                title: loaded.get(&ev.item).and_then(|i| i.title.clone()),
                                item: ev.item,
                                quote: ev.quote,
                            })
                            .collect(),
                    },
                },
            }
        })
        .collect();
    Ok(TimelineResult {
        entity: EntityRefView::from(&entity),
        events,
        months: cut.months,
        total: cut.total,
        truncated: cut.truncated,
    })
}

// ---------------------------------------------------------------- search.preview

#[derive(Debug, Deserialize)]
struct PreviewParams {
    item: ItemRef,
    #[serde(default)]
    chunk_id: Option<i64>,
}

#[derive(Debug, Serialize)]
struct PreviewFact {
    id: i64,
    subject: EntityRefView,
    predicate: String,
    text: String,
    status: String,
    confidence: f32,
    quote: Option<String>,
}

#[derive(Debug, Serialize)]
struct PreviewResult {
    item: ItemRef,
    title: Option<String>,
    kind: ItemKind,
    mime: Option<String>,
    url: Option<String>,
    date: Option<String>,
    date_source: DateSource,
    sensitive: bool,
    text: String,
    truncated: bool,
    passage: Option<String>,
    people: Vec<EntityRefView>,
    facts: Vec<PreviewFact>,
}

impl SearchUi {
    async fn preview(&self, p: PreviewParams) -> Result<PreviewResult> {
        check_item("item", &p.item)?;
        let source_kind = self.source_kinds().await.remove(p.item.source.as_str());
        let offset = self.offset(None)?;
        let store = self.state.store.clone();
        let graph = self.state.knowledge.graph.clone();
        blocking(move || build_preview(&store, graph.as_ref(), p, source_kind.as_deref(), offset))
            .await
    }
}

fn build_preview(
    store: &Store,
    graph: &dyn GraphStore,
    p: PreviewParams,
    source_kind: Option<&str>,
    offset: UtcOffset,
) -> Result<PreviewResult> {
    let item = store
        .get_item(&p.item)?
        .ok_or_else(|| Error::NotFound(format!("item {}:{}", p.item.source, p.item.key)))?;
    let (at, date_source) = logic::own_date(&logic::dates_of(&item), offset);
    let full = item.text.as_deref().unwrap_or("");
    let (text, truncated) = logic::cut_chars(full, PREVIEW_CHARS);
    let passage = match p.chunk_id {
        Some(id) => store
            .get_chunks(&[id])?
            .into_iter()
            .find(|c| c.id == id && c.item == p.item)
            .map(|c| c.text),
        None => None,
    };
    let people: Vec<EntityRefView> = store
        .entities_in_items(std::slice::from_ref(&p.item))?
        .remove(&p.item)
        .unwrap_or_default()
        .iter()
        .take(PREVIEW_PEOPLE)
        .map(EntityRefView::from)
        .collect();
    let mut facts = Vec::new();
    let mut subjects: HashMap<EntityId, EntityRefView> = HashMap::new();
    for id in store.claims_with_evidence_in(&p.item, true, PREVIEW_FACTS)? {
        let Some(c) = graph.get_claim(id)? else {
            continue;
        };
        if !matches!(c.status, ClaimStatus::Active | ClaimStatus::UserAsserted) {
            continue;
        }
        let subject = match subjects.get(&c.subject) {
            Some(s) => s.clone(),
            None => {
                let view = match graph.get_entity(&c.subject)? {
                    Some(e) => EntityRefView::from(&e),
                    None => EntityRefView {
                        id: c.subject.to_string(),
                        name: c.subject.to_string(),
                        kind: "unknown".into(),
                    },
                };
                subjects.insert(c.subject.clone(), view.clone());
                view
            }
        };
        let quote = c
            .evidence
            .iter()
            .filter(|e| e.item == p.item)
            .find_map(|e| e.quote.clone());
        facts.push(PreviewFact {
            id: c.id,
            subject,
            predicate: c.predicate,
            text: c.text,
            status: status_str(c.status),
            confidence: c.confidence,
            quote,
        });
    }
    Ok(PreviewResult {
        kind: logic::classify_kind(&item.metadata, item.mime.as_deref(), source_kind),
        url: item_url(&item.metadata),
        sensitive: nugget_retrieve::is_sensitive(&item.metadata),
        date: Some(logic::rfc3339(at)),
        date_source,
        text: text.to_owned(),
        truncated,
        passage,
        people,
        facts,
        title: item.title,
        mime: item.mime,
        item: p.item,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::SourceId;

    #[test]
    fn limits_clamp() {
        assert_eq!(clamp_limit(None, 25, 100), 25);
        assert_eq!(clamp_limit(Some(0), 25, 100), 25);
        assert_eq!(clamp_limit(Some(7), 25, 100), 7);
        assert_eq!(clamp_limit(Some(10_000), 25, 100), 100);
    }

    #[test]
    fn lists_and_values_are_bounded() {
        let many: Vec<String> = (0..=MAX_FILTER_VALUES).map(|i| i.to_string()).collect();
        let err = check_list("filters.sources", &many)
            .unwrap_err()
            .to_string();
        assert!(err.contains("filters.sources"), "{err}");
        assert!(check_list("x", &many[..MAX_FILTER_VALUES]).is_ok());
        let long = vec!["a".repeat(MAX_VALUE_LEN + 1)];
        assert!(check_list("filters.people", &long).is_err());
        let empty = ItemRef {
            source: SourceId::new(""),
            key: nugget_core::ids::ItemKey::new("k"),
        };
        assert!(check_item("item", &empty).is_err());
    }
}
