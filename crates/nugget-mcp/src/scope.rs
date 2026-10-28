//! Per-client scope enforcement, applied to every result whatever the backend did.
//!
//! Rules:
//! - **Sources.** Content from an item is returned only if its source is on the client's
//!   allow-list (`McpScopes::sources`, `None` = all sources).
//! - **Sensitive items** are returned only with `include_sensitive`. This covers search hits,
//!   context passages, item reads, fact evidence, mentions and graph provenance.
//! - **Facts** keep only the evidence the client may see; a fact left without evidence is
//!   dropped, so a fact backed only by a sensitive item never reaches the client.
//! - **Entities** are visible when at least one of their origins is allowed. Their aliases,
//!   identifiers and attributes have no per-field provenance, so they are shown only when
//!   *every* origin is allowed; otherwise they are withheld and the result says so.
//! - **Vault-only content** (user-asserted facts and entities with no source evidence) counts as
//!   the pseudo-source [`VAULT_SOURCE`]: visible with all sources, or when the allow-list names it.
//! - **Tools**: only allow-listed tools are listed and callable (`McpScopes::tools`, empty = all).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use nugget_core::ids::{EntityId, SourceId};
use nugget_core::ipc::McpScopes;
use serde::{Deserialize, Serialize};

use crate::backend::{
    ContextPack, EntityRecord, EntitySummaryRecord, FactRecord, NeighbourhoodRecord, Origin,
    RecentChanges, SearchRecord,
};
use crate::tools;

/// Pseudo-source for content that exists only in the user's vault (user-asserted facts).
pub const VAULT_SOURCE: &str = "vault";

/// The data part of a client's scopes, handed to the backend with every query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeFilter {
    /// Allowed sources; `None` means all.
    pub sources: Option<Vec<SourceId>>,
    pub include_sensitive: bool,
}

impl ScopeFilter {
    /// May content from an item in `source` with this sensitivity be returned?
    pub fn allows(&self, source: &SourceId, sensitive: bool) -> bool {
        if sensitive && !self.include_sensitive {
            return false;
        }
        match &self.sources {
            None => true,
            Some(list) => list.iter().any(|s| s == source),
        }
    }

    pub fn allows_origin(&self, o: &Origin) -> bool {
        self.allows(&o.source, o.sensitive)
    }

    /// Vault-only content (no source evidence).
    pub fn allows_vault(&self) -> bool {
        self.allows(&SourceId::new(VAULT_SOURCE), false)
    }

    /// At least one origin allowed (vault rule for no origins).
    pub fn allows_any(&self, origins: &[Origin]) -> bool {
        if origins.is_empty() {
            return self.allows_vault();
        }
        origins.iter().any(|o| self.allows_origin(o))
    }

    /// Every origin allowed (vault rule for no origins).
    pub fn allows_all(&self, origins: &[Origin]) -> bool {
        if origins.is_empty() {
            return self.allows_vault();
        }
        origins.iter().all(|o| self.allows_origin(o))
    }
}

/// A client's effective scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    filter: ScopeFilter,
    /// `None` = every read-only tool.
    tools: Option<HashSet<String>>,
}

impl Scope {
    pub fn new(scopes: &McpScopes) -> Self {
        let tools = if scopes.tools.is_empty() {
            None
        } else {
            Some(scopes.tools.iter().cloned().collect())
        };
        Self {
            filter: ScopeFilter {
                sources: scopes.sources.clone(),
                include_sensitive: scopes.include_sensitive,
            },
            tools,
        }
    }

    pub fn filter(&self) -> &ScopeFilter {
        &self.filter
    }

    /// Known tool and on the allow-list.
    pub fn allows_tool(&self, name: &str) -> bool {
        tools::ALL_TOOLS.contains(&name) && self.tools.as_ref().is_none_or(|set| set.contains(name))
    }

    pub fn search(&self, hits: Vec<SearchRecord>) -> Vec<SearchRecord> {
        hits.into_iter()
            .filter(|h| self.filter.allows(&h.item.source, h.sensitive))
            .collect()
    }

    /// Drops disallowed passages, counting dropped sensitive ones in `excluded_sensitive`.
    pub fn context(&self, mut pack: ContextPack) -> ContextPack {
        let mut excluded = pack.excluded_sensitive;
        pack.passages.retain(|p| {
            let ok = self.filter.allows(&p.item.source, p.sensitive);
            if !ok && p.sensitive && self.source_allowed(&p.item.source) {
                excluded += 1;
            }
            ok
        });
        pack.excluded_sensitive = excluded;
        pack
    }

    fn source_allowed(&self, source: &SourceId) -> bool {
        self.filter
            .sources
            .as_ref()
            .is_none_or(|l| l.iter().any(|s| s == source))
    }

    pub fn entity_visible(&self, origins: &[Origin]) -> bool {
        self.filter.allows_any(origins)
    }

    pub fn entities(&self, list: Vec<EntitySummaryRecord>) -> Vec<EntitySummaryRecord> {
        list.into_iter()
            .filter(|e| self.entity_visible(&e.origins))
            .collect()
    }

    /// Keep only the evidence the client may see; `None` if nothing is left to support it.
    pub fn fact(&self, mut fact: FactRecord) -> Option<FactRecord> {
        if !matches!(fact.status.as_str(), "active" | "user_asserted") {
            return None;
        }
        if fact.evidence.is_empty() {
            // Only a vault edit can stand without evidence.
            return (fact.status == "user_asserted" && self.filter.allows_vault()).then_some(fact);
        }
        fact.evidence
            .retain(|e| self.filter.allows(&e.item.source, e.sensitive));
        (!fact.evidence.is_empty()).then_some(fact)
    }

    /// The scoped entity, or `None` when the client may not see it at all.
    pub fn entity(&self, mut e: EntityRecord) -> Option<ScopedEntity> {
        if !self.entity_visible(&e.origins) {
            return None;
        }
        let complete = self.filter.allows_all(&e.origins);
        let facts = std::mem::take(&mut e.facts)
            .into_iter()
            .filter_map(|f| self.fact(f))
            .collect();
        e.facts = facts;
        e.mentions
            .retain(|m| self.filter.allows(&m.item.source, m.sensitive));
        if !complete {
            e.aliases.clear();
            e.identifiers.clear();
            e.attributes = serde_json::Value::Object(Default::default());
        }
        e.origins.clear();
        Some(ScopedEntity {
            entity: e,
            details_withheld: !complete,
        })
    }

    /// Filters nodes and edges, then re-walks from the centre so nothing reachable only through
    /// a hidden node or edge stays in the view, and recomputes depths.
    pub fn neighbourhood(
        &self,
        center: &EntityId,
        view: NeighbourhoodRecord,
    ) -> NeighbourhoodRecord {
        let visible: HashMap<EntityId, _> = view
            .nodes
            .into_iter()
            .filter(|n| self.entity_visible(&n.origins))
            .map(|n| (n.id.clone(), n))
            .collect();
        if !visible.contains_key(center) {
            return NeighbourhoodRecord::default();
        }
        let edges: Vec<_> = view
            .edges
            .into_iter()
            .filter(|e| {
                visible.contains_key(&e.from)
                    && visible.contains_key(&e.to)
                    && self.filter.allows_any(&e.origins)
            })
            .collect();
        let mut depth: HashMap<EntityId, u32> = HashMap::from([(center.clone(), 0)]);
        let mut queue = VecDeque::from([center.clone()]);
        while let Some(id) = queue.pop_front() {
            let d = depth[&id];
            for e in &edges {
                let other = if e.from == id {
                    &e.to
                } else if e.to == id {
                    &e.from
                } else {
                    continue;
                };
                if !depth.contains_key(other) {
                    depth.insert(other.clone(), d + 1);
                    queue.push_back(other.clone());
                }
            }
        }
        let mut nodes: Vec<_> = visible
            .into_values()
            .filter_map(|mut n| {
                let d = *depth.get(&n.id)?;
                n.depth = d;
                n.origins.clear();
                Some(n)
            })
            .collect();
        nodes.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));
        let edges = edges
            .into_iter()
            .filter(|e| depth.contains_key(&e.from) && depth.contains_key(&e.to))
            .map(|mut e| {
                e.origins.clear();
                e
            })
            .collect();
        NeighbourhoodRecord {
            nodes,
            edges,
            truncated: view.truncated,
        }
    }

    pub fn recent(&self, mut changes: RecentChanges) -> RecentChanges {
        changes.entities = self.entities(std::mem::take(&mut changes.entities));
        changes
            .items
            .retain(|i| self.filter.allows(&i.item.source, i.sensitive));
        changes
    }
}

/// An entity after scoping.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedEntity {
    pub entity: EntityRecord,
    /// Aliases, identifiers and attributes were withheld because some of the entity's sources are
    /// outside the client's scope.
    pub details_withheld: bool,
}

/// Distinct origins, sorted (helper for backends).
pub fn distinct_origins(origins: impl IntoIterator<Item = Origin>) -> Vec<Origin> {
    origins
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
