//! An in-memory [`KnowledgeBackend`] for tests (here, in `nuggetd` and in `bench`).

use std::sync::Mutex;

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use time::OffsetDateTime;

use crate::backend::{
    ContextPack, ContextPassage, EntityRecord, EntitySummaryRecord, ItemRecord, ItemSummaryRecord,
    KnowledgeBackend, NeighbourhoodRecord, RecentChanges, SearchRecord,
};
use crate::scope::{DetailOrigins, ScopeFilter};

/// Holds items, entities and graph views. Search matches items whose title or text contains any
/// query word (case-insensitive), scored by matching words.
///
/// By default it honours the [`ScopeFilter`] like a real backend; [`FakeBackend::leaky`] makes it
/// ignore the filter, to prove the MCP layer enforces scopes on its own.
#[derive(Debug, Default)]
pub struct FakeBackend {
    items: Mutex<Vec<ItemRecord>>,
    entities: Mutex<Vec<EntityRecord>>,
    graphs: Mutex<Vec<(EntityId, NeighbourhoodRecord)>>,
    details: Mutex<Vec<(EntityId, DetailOrigins)>>,
    ignore_filter: bool,
    calls: Mutex<Vec<String>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend that ignores the scope filter entirely.
    pub fn leaky() -> Self {
        Self {
            ignore_filter: true,
            ..Self::default()
        }
    }

    pub fn add_item(&self, item: ItemRecord) {
        let mut items = lock(&self.items);
        items.retain(|i| i.item != item.item);
        items.push(item);
    }

    pub fn add_entity(&self, entity: EntityRecord) {
        let mut v = lock(&self.entities);
        v.retain(|e| e.id != entity.id);
        v.push(entity);
    }

    /// Per-detail provenance returned for `id` by `entity_detail_origins`.
    pub fn set_detail_origins(&self, id: EntityId, details: DetailOrigins) {
        let mut d = lock(&self.details);
        d.retain(|(i, _)| i != &id);
        d.push((id, details));
    }

    /// The neighbourhood returned for `center` (at any depth).
    pub fn set_neighbourhood(&self, center: EntityId, view: NeighbourhoodRecord) {
        let mut g = lock(&self.graphs);
        g.retain(|(c, _)| c != &center);
        g.push((center, view));
    }

    /// Backend methods called so far, in order.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }

    fn log(&self, call: &str) {
        lock(&self.calls).push(call.to_owned());
    }

    fn pass(&self, f: &ScopeFilter, source: &nugget_core::ids::SourceId, sensitive: bool) -> bool {
        self.ignore_filter || f.allows(source, sensitive)
    }

    fn ranked(&self, query: &str, f: &ScopeFilter) -> Vec<(f32, ItemRecord)> {
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let mut out: Vec<(f32, ItemRecord)> = lock(&self.items)
            .iter()
            .filter(|i| self.pass(f, &i.item.source, i.sensitive))
            .filter_map(|i| {
                let hay = format!(
                    "{} {}",
                    i.title.as_deref().unwrap_or(""),
                    i.text.as_deref().unwrap_or("")
                )
                .to_lowercase();
                let n = words.iter().filter(|w| hay.contains(w.as_str())).count();
                (n > 0).then(|| (n as f32 / words.len().max(1) as f32, i.clone()))
            })
            .collect();
        out.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.item.key.cmp(&b.1.item.key))
        });
        out
    }

    fn summary(e: &EntityRecord) -> EntitySummaryRecord {
        EntitySummaryRecord {
            id: e.id.clone(),
            kind: e.kind.clone(),
            name: e.name.clone(),
            claims: e.facts.len() as u64,
            mentions: e.mentions.len() as u64,
            updated_at: e.updated_at,
            origins: e.origins.clone(),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[async_trait]
impl KnowledgeBackend for FakeBackend {
    async fn search(
        &self,
        query: &str,
        limit: usize,
        f: &ScopeFilter,
    ) -> Result<Vec<SearchRecord>> {
        self.log("search");
        Ok(self
            .ranked(query, f)
            .into_iter()
            .take(limit)
            .map(|(score, i)| SearchRecord {
                snippet: i.text.as_deref().unwrap_or("").chars().take(200).collect(),
                chunk_id: Some(1),
                item: i.item,
                title: i.title,
                url: i.url,
                score,
                sensitive: i.sensitive,
            })
            .collect())
    }

    async fn context(
        &self,
        query: &str,
        budget_tokens: u32,
        f: &ScopeFilter,
    ) -> Result<ContextPack> {
        self.log("context");
        let mut excluded = 0;
        if !self.ignore_filter && !f.include_sensitive {
            let all = ScopeFilter {
                include_sensitive: true,
                ..f.clone()
            };
            excluded = self
                .ranked(query, &all)
                .iter()
                .filter(|(_, i)| i.sensitive)
                .count() as u32;
        }
        let mut left = budget_tokens as usize * 4;
        let mut passages = Vec::new();
        for (_, i) in self.ranked(query, f) {
            let text: String = i
                .text
                .clone()
                .unwrap_or_default()
                .chars()
                .take(left)
                .collect();
            if text.is_empty() {
                break;
            }
            left -= text.chars().count();
            passages.push(ContextPassage {
                item: i.item,
                chunk_id: 1,
                title: i.title,
                url: i.url,
                sensitive: i.sensitive,
                text,
            });
        }
        Ok(ContextPack {
            passages,
            excluded_sensitive: excluded,
        })
    }

    async fn find_entities(
        &self,
        text: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
        f: &ScopeFilter,
    ) -> Result<Vec<EntitySummaryRecord>> {
        self.log("find_entities");
        let needle = text.map(str::to_lowercase);
        let mut v: Vec<_> = lock(&self.entities)
            .iter()
            .filter(|e| kind.is_none_or(|k| e.kind == k))
            .filter(|e| {
                needle.as_ref().is_none_or(|n| {
                    e.name.to_lowercase().contains(n)
                        || e.aliases.iter().any(|a| a.to_lowercase().contains(n))
                })
            })
            .filter(|e| self.ignore_filter || f.allows_any(&e.origins))
            .map(Self::summary)
            .collect();
        v.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(v.into_iter().skip(offset).take(limit).collect())
    }

    async fn entity_detail_origins(
        &self,
        id: &EntityId,
        _f: &ScopeFilter,
    ) -> Result<Option<DetailOrigins>> {
        Ok(lock(&self.details)
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, d)| d.clone()))
    }

    async fn get_entity(&self, id: &EntityId, _f: &ScopeFilter) -> Result<Option<EntityRecord>> {
        self.log("get_entity");
        // Returns everything: per-fact filtering is the MCP layer's job.
        Ok(lock(&self.entities).iter().find(|e| &e.id == id).cloned())
    }

    async fn neighbourhood(
        &self,
        id: &EntityId,
        _depth: u32,
        _limit: usize,
        _f: &ScopeFilter,
    ) -> Result<NeighbourhoodRecord> {
        self.log("neighbourhood");
        Ok(lock(&self.graphs)
            .iter()
            .find(|(c, _)| c == id)
            .map(|(_, g)| g.clone())
            .unwrap_or_default())
    }

    async fn get_item(&self, item: &ItemRef, f: &ScopeFilter) -> Result<Option<ItemRecord>> {
        self.log("get_item");
        Ok(lock(&self.items)
            .iter()
            .find(|i| &i.item == item && self.pass(f, &i.item.source, i.sensitive))
            .cloned())
    }

    async fn recent_changes(
        &self,
        since: OffsetDateTime,
        limit: usize,
        f: &ScopeFilter,
    ) -> Result<RecentChanges> {
        self.log("recent_changes");
        let mut entities: Vec<_> = lock(&self.entities)
            .iter()
            .filter(|e| e.updated_at > since)
            .map(Self::summary)
            .collect();
        entities.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        entities.truncate(limit);
        let mut items: Vec<_> = lock(&self.items)
            .iter()
            .filter(|i| i.updated_at > since && self.pass(f, &i.item.source, i.sensitive))
            .map(|i| ItemSummaryRecord {
                item: i.item.clone(),
                title: i.title.clone(),
                url: i.url.clone(),
                updated_at: i.updated_at,
                sensitive: i.sensitive,
            })
            .collect();
        items.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        items.truncate(limit);
        Ok(RecentChanges { entities, items })
    }
}
