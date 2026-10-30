//! The read-only tools: definitions (JSON schemas, agent-facing descriptions) and execution.

use std::fmt::Write as _;
use std::sync::Arc;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{Citation, ContextResponse};
use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::audit::Outcome;
use crate::backend::{EntityRecord, EvidenceRecord, KnowledgeBackend};
use crate::render::{self, RESULT_PREAMBLE, SAFETY_NOTE, inline, item_attrs, wrap};
use crate::scope::{Scope, ScopedEntity};

pub const SEARCH: &str = "search";
pub const GET_CONTEXT: &str = "get_context";
pub const FIND_ENTITIES: &str = "find_entities";
pub const GET_ENTITY: &str = "get_entity";
pub const GET_NEIGHBOURHOOD: &str = "get_neighbourhood";
pub const GET_ITEM: &str = "get_item";
pub const RECENT_CHANGES: &str = "recent_changes";

/// Every tool, all read-only.
pub const ALL_TOOLS: [&str; 7] = [
    SEARCH,
    GET_CONTEXT,
    FIND_ENTITIES,
    GET_ENTITY,
    GET_NEIGHBOURHOOD,
    GET_ITEM,
    RECENT_CHANGES,
];

/// Entity kinds (mirrors `nugget_graph::EntityKind`).
pub const ENTITY_KINDS: [&str; 8] = [
    "person",
    "organization",
    "project",
    "commitment",
    "preference",
    "event",
    "place",
    "topic",
];

pub const DEFAULT_SEARCH_LIMIT: usize = 10;
pub const MAX_SEARCH_LIMIT: usize = 50;
pub const DEFAULT_BUDGET_TOKENS: u32 = 2_000;
pub const MIN_BUDGET_TOKENS: u32 = 100;
pub const MAX_BUDGET_TOKENS: u32 = 8_000;
pub const DEFAULT_ENTITY_LIMIT: usize = 20;
pub const MAX_ENTITY_LIMIT: usize = 100;
pub const MAX_DEPTH: u32 = 3;
pub const NEIGHBOURHOOD_NODE_LIMIT: usize = 100;
pub const DEFAULT_ITEM_CHARS: usize = 20_000;
pub const MAX_ITEM_CHARS: usize = 100_000;
pub const DEFAULT_RECENT_LIMIT: usize = 50;
pub const MAX_RECENT_LIMIT: usize = 200;
pub const DEFAULT_RECENT_DAYS: i64 = 7;
const MAX_QUERY_CHARS: usize = 2_000;
/// Characters per token used to enforce context budgets.
const CHARS_PER_TOKEN: usize = 4;

fn schema(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(o) => Arc::new(o),
        _ => unreachable!("tool schemas are objects"),
    }
}

fn describe(text: &str) -> String {
    format!("{text}\n\n{SAFETY_NOTE}")
}

/// Definitions of every tool, in [`ALL_TOOLS`] order.
pub fn definitions() -> Vec<Tool> {
    let ro = |title: &str| {
        ToolAnnotations::with_title(title)
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false)
    };
    let kinds: Vec<Value> = ENTITY_KINDS.iter().map(|k| json!(k)).collect();
    vec![
        Tool::new(
            SEARCH,
            describe(
                "Search the user's personal knowledge (files, mail, calendar, notes indexed by \
                 Nugget) by keywords or a natural-language question. Returns the best-matching \
                 passage of each item with its source, item key and relevance score, best first. \
                 Use get_item to read a whole item, or get_context for a citable bundle of \
                 passages to answer from.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS,
                              "description": "Keywords or a question."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT,
                              "default": DEFAULT_SEARCH_LIMIT,
                              "description": "Maximum number of results."}
                },
                "required": ["query"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Search personal knowledge")),
        Tool::new(
            GET_CONTEXT,
            describe(
                "Get the most relevant, diverse passages for a question, packed under a token \
                 budget and numbered [1], [2], ... Cite these markers when answering. Prefer \
                 this over search when you need material to answer from.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS,
                              "description": "The question to gather context for."},
                    "budget_tokens": {"type": "integer", "minimum": MIN_BUDGET_TOKENS,
                                      "maximum": MAX_BUDGET_TOKENS,
                                      "default": DEFAULT_BUDGET_TOKENS,
                                      "description": "Approximate size of the returned passages, in tokens."}
                },
                "required": ["query"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Get cited context")),
        Tool::new(
            FIND_ENTITIES,
            describe(
                "Find people, organizations, projects, commitments, preferences, events, places \
                 and topics in the user's knowledge graph whose name or alias contains the given \
                 text (an empty text lists the most recently updated). Returns entity ids for \
                 get_entity and get_neighbourhood.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "maxLength": 200,
                             "description": "Name or alias fragment; empty for all."},
                    "kind": {"type": "string", "enum": kinds,
                             "description": "Only entities of this kind."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_ENTITY_LIMIT,
                              "default": DEFAULT_ENTITY_LIMIT}
                },
                "required": ["text"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Find entities")),
        Tool::new(
            GET_ENTITY,
            describe(
                "Get one entity from the knowledge graph: its kind, aliases and identifiers, the \
                 facts known about it, each with numbered citations to the source items and \
                 quotes it came from, and where it is mentioned.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1, "maxLength": 200,
                           "description": "Entity id from find_entities, search or get_neighbourhood."}
                },
                "required": ["id"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Get entity")),
        Tool::new(
            GET_NEIGHBOURHOOD,
            describe(
                "Get the entities connected to an entity (people they work with, projects, \
                 events, ...) up to a number of hops, with the relations between them.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1, "maxLength": 200,
                           "description": "Centre entity id."},
                    "depth": {"type": "integer", "minimum": 1, "maximum": MAX_DEPTH, "default": 1,
                              "description": "Number of hops."}
                },
                "required": ["id"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Get entity neighbourhood")),
        Tool::new(
            GET_ITEM,
            describe(
                "Read one source item (a file, mail, event or note) by its source and item key, \
                 as returned by search or get_context. Returns its text and metadata. Long items \
                 are returned in windows: call again with offset_chars to continue.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string", "minLength": 1, "description": "Source id, e.g. `gmail:me@example.com`."},
                    "key": {"type": "string", "minLength": 1, "description": "Item key within the source."},
                    "offset_chars": {"type": "integer", "minimum": 0, "default": 0,
                                     "description": "Character offset to start reading at."},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": MAX_ITEM_CHARS,
                                  "default": DEFAULT_ITEM_CHARS,
                                  "description": "Maximum characters of text to return."}
                },
                "required": ["source", "key"],
                "additionalProperties": false
            })),
        )
        .annotate(ro("Read item")),
        Tool::new(
            RECENT_CHANGES,
            describe(
                "List entities and source items that changed recently, newest first. Useful to \
                 catch up on what is new since a point in time.",
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "since": {"type": "string", "format": "date-time",
                              "description": "RFC 3339 timestamp; defaults to 7 days ago."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_RECENT_LIMIT,
                              "default": DEFAULT_RECENT_LIMIT,
                              "description": "Maximum entities and maximum items to return."}
                },
                "additionalProperties": false
            })),
        )
        .annotate(ro("Recent changes")),
    ]
}

/// A successful tool result.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub structured: Value,
    /// Number of results (hits, passages, entities, ...).
    pub results: usize,
    /// Items whose content was returned (for the audit log).
    pub items: Vec<ItemRef>,
}

/// A failed call, shown to the agent as a tool error.
#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub outcome: Outcome,
    pub message: String,
}

impl ToolFailure {
    fn rejected(msg: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Rejected,
            message: msg.into(),
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            outcome: Outcome::Cancelled,
            message: "request cancelled by the client".into(),
        }
    }

    fn backend(e: nugget_core::Error) -> Self {
        tracing::warn!(error = %e, "mcp backend error");
        Self {
            outcome: Outcome::Error,
            message: "Nugget could not complete the request; try again later.".into(),
        }
    }
}

type ToolResult = Result<ToolOutput, ToolFailure>;

fn args<T: for<'de> Deserialize<'de>>(a: &JsonObject) -> Result<T, ToolFailure> {
    serde_json::from_value(Value::Object(a.clone()))
        .map_err(|e| ToolFailure::rejected(format!("invalid arguments: {e}")))
}

fn query_arg(q: &str) -> Result<&str, ToolFailure> {
    let q = q.trim();
    if q.is_empty() {
        return Err(ToolFailure::rejected("query must not be empty"));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(ToolFailure::rejected(format!(
            "query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    Ok(q)
}

const NOT_AVAILABLE_ENTITY: &str = "entity not found (or not available to this client's scopes)";
const NOT_AVAILABLE_ITEM: &str = "item not found (or not available to this client's scopes)";

/// Run a tool the client is allowed to call.
pub(crate) async fn run(
    backend: &dyn KnowledgeBackend,
    scope: &Scope,
    name: &str,
    a: &JsonObject,
) -> ToolResult {
    match name {
        SEARCH => search(backend, scope, a).await,
        GET_CONTEXT => get_context(backend, scope, a).await,
        FIND_ENTITIES => find_entities(backend, scope, a).await,
        GET_ENTITY => get_entity(backend, scope, a).await,
        GET_NEIGHBOURHOOD => get_neighbourhood(backend, scope, a).await,
        GET_ITEM => get_item(backend, scope, a).await,
        RECENT_CHANGES => recent_changes(backend, scope, a).await,
        other => Err(ToolFailure::rejected(format!("unknown tool: {other}"))),
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
}

async fn search(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: SearchArgs = args(a)?;
    let q = query_arg(&p.query)?;
    let limit = p
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let mut hits = scope.search(
        b.search(q, limit, scope.filter())
            .await
            .map_err(ToolFailure::backend)?,
    );
    hits.truncate(limit);
    let mut text = format!("{RESULT_PREAMBLE}\n\n{} result(s).\n", hits.len());
    let mut structured = Vec::with_capacity(hits.len());
    for (i, h) in hits.iter().enumerate() {
        let n = i + 1;
        let _ = write!(text, "\n[{n}] score {:.2}\n", h.score);
        text.push_str(&wrap(
            &item_attrs(&h.item, h.title.as_deref(), h.url.as_deref()),
            &h.snippet,
        ));
        text.push('\n');
        structured.push(json!({
            "n": n,
            "source": h.item.source,
            "key": h.item.key,
            "uri": render::item_uri(&h.item),
            "chunk_id": h.chunk_id,
            "title": h.title,
            "url": h.url,
            "score": h.score,
            "snippet": h.snippet,
        }));
    }
    Ok(ToolOutput {
        results: hits.len(),
        items: hits.into_iter().map(|h| h.item).collect(),
        text,
        structured: json!({"untrusted_content": true, "hits": structured}),
    })
}

#[derive(Deserialize)]
struct ContextArgs {
    query: String,
    budget_tokens: Option<u32>,
}

async fn get_context(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: ContextArgs = args(a)?;
    let q = query_arg(&p.query)?;
    let budget = p
        .budget_tokens
        .unwrap_or(DEFAULT_BUDGET_TOKENS)
        .clamp(MIN_BUDGET_TOKENS, MAX_BUDGET_TOKENS);
    let pack = scope.context(
        b.context(q, budget, scope.filter())
            .await
            .map_err(ToolFailure::backend)?,
    );
    // Enforce the budget here too (the backend's estimate is authoritative only for itself).
    let mut remaining = budget as usize * CHARS_PER_TOKEN;
    let mut body = String::new();
    let mut citations = Vec::new();
    for p in &pack.passages {
        if remaining < 200 {
            break;
        }
        let (passage, total) = render::char_window(&p.text, 0, remaining);
        remaining -= passage.chars().count();
        let n = citations.len() as u32 + 1;
        let _ = write!(body, "[{n}] ");
        body.push_str(&wrap(
            &item_attrs(&p.item, p.title.as_deref(), p.url.as_deref()),
            passage,
        ));
        if passage.chars().count() < total {
            body.push_str("\n(passage truncated to fit the budget)");
        }
        body.push_str("\n\n");
        citations.push(Citation {
            n,
            item: p.item.clone(),
            chunk_id: p.chunk_id,
            title: p.title.clone(),
            url: p.url.clone(),
            sensitive: p.sensitive,
            date: None,
        });
    }
    let mut text = format!(
        "{RESULT_PREAMBLE}\nCite passages by their [n] markers.\n\n{}",
        if body.is_empty() {
            "No relevant passages.\n".to_owned()
        } else {
            body.clone()
        }
    );
    if pack.excluded_sensitive > 0 {
        let _ = writeln!(
            text,
            "({} relevant sensitive item(s) were left out by this client's scopes.)",
            pack.excluded_sensitive
        );
    }
    let response = ContextResponse {
        text: body,
        citations,
        excluded_sensitive: pack.excluded_sensitive,
    };
    let items = response.citations.iter().map(|c| c.item.clone()).collect();
    let mut structured = serde_json::to_value(&response).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut structured {
        o.insert("untrusted_content".into(), Value::Bool(true));
    }
    Ok(ToolOutput {
        results: response.citations.len(),
        items,
        text,
        structured,
    })
}

#[derive(Deserialize)]
struct FindArgs {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    limit: Option<usize>,
}

async fn find_entities(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: FindArgs = args(a)?;
    if let Some(k) = &p.kind
        && !ENTITY_KINDS.contains(&k.as_str())
    {
        return Err(ToolFailure::rejected(format!(
            "unknown kind {k:?}; expected one of {}",
            ENTITY_KINDS.join(", ")
        )));
    }
    let text = p.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if text.is_some_and(|t| t.chars().count() > 200) {
        return Err(ToolFailure::rejected("text is longer than 200 characters"));
    }
    let limit = p
        .limit
        .unwrap_or(DEFAULT_ENTITY_LIMIT)
        .clamp(1, MAX_ENTITY_LIMIT);
    let mut list = scope.entities(
        b.find_entities(text, p.kind.as_deref(), limit, 0, scope.filter())
            .await
            .map_err(ToolFailure::backend)?,
    );
    list.truncate(limit);
    let mut lines = String::new();
    let mut structured = Vec::new();
    for e in &list {
        let _ = writeln!(
            lines,
            "- {} ({}) id={} facts={} mentions={}",
            inline(&e.name, 200),
            e.kind,
            inline(e.id.as_str(), 200),
            e.claims,
            e.mentions
        );
        structured.push(json!({
            "id": e.id, "kind": e.kind, "name": e.name, "facts": e.claims,
            "mentions": e.mentions, "uri": render::entity_uri(e.id.as_str()),
            "updated_at": e.updated_at.format(&Rfc3339).ok(),
        }));
    }
    let text = format!(
        "{RESULT_PREAMBLE}\n\n{} entit(ies).\n{}",
        list.len(),
        if lines.is_empty() {
            String::new()
        } else {
            wrap(&[("kind", Some("entity_list"))], &lines)
        }
    );
    Ok(ToolOutput {
        results: list.len(),
        items: Vec::new(),
        text,
        structured: json!({"untrusted_content": true, "entities": structured}),
    })
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
    depth: Option<u32>,
}

/// The scoped entity, or a not-available failure.
pub(crate) async fn load_entity(
    b: &dyn KnowledgeBackend,
    scope: &Scope,
    id: &str,
) -> Result<ScopedEntity, ToolFailure> {
    let id = id.trim();
    if id.is_empty() || id.chars().count() > 200 {
        return Err(ToolFailure::rejected("invalid entity id"));
    }
    let Some(e) = b
        .get_entity(&EntityId::new(id), scope.filter())
        .await
        .map_err(ToolFailure::backend)?
    else {
        return Err(ToolFailure::rejected(NOT_AVAILABLE_ENTITY));
    };
    // Per-detail provenance only matters when some origin is out of scope.
    let details = if scope.filter().allows_all(&e.origins) {
        None
    } else {
        b.entity_detail_origins(&e.id, scope.filter())
            .await
            .map_err(ToolFailure::backend)?
    };
    scope
        .entity_with_details(e, details.as_ref())
        .ok_or_else(|| ToolFailure::rejected(NOT_AVAILABLE_ENTITY))
}

async fn get_entity(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: IdArgs = args(a)?;
    let se = load_entity(b, scope, &p.id).await?;
    let (page, evidence_items) = entity_markdown(&se);
    let text = format!("{RESULT_PREAMBLE}\n\n{page}");
    Ok(ToolOutput {
        results: se.entity.facts.len(),
        items: evidence_items,
        text,
        structured: entity_json(&se),
    })
}

/// Markdown page for a scoped entity (also the `nugget://entity/<id>` resource), wrapped as
/// untrusted content, and the items its citations point at.
pub(crate) fn entity_markdown(se: &ScopedEntity) -> (String, Vec<ItemRef>) {
    let e: &EntityRecord = &se.entity;
    let mut md = String::new();
    let _ = writeln!(md, "# {} ({})\n", inline(&e.name, 300), e.kind);
    let _ = writeln!(md, "id: {}", inline(e.id.as_str(), 200));
    if !e.aliases.is_empty() {
        let aliases: Vec<_> = e.aliases.iter().map(|a| inline(a, 200)).collect();
        let _ = writeln!(md, "aliases: {}", aliases.join(", "));
    }
    for (scheme, value) in &e.identifiers {
        let _ = writeln!(md, "{}: {}", inline(scheme, 50), inline(value, 300));
    }
    if e.attributes.as_object().is_some_and(|o| !o.is_empty()) {
        let _ = writeln!(
            md,
            "attributes: {}",
            inline(&e.attributes.to_string(), 2000)
        );
    }
    if se.details_withheld {
        md.push_str(
            "(some aliases, identifiers or attributes withheld: they come from sources outside \
             this client's scopes)\n",
        );
    }
    let mut cites: Vec<&EvidenceRecord> = Vec::new();
    md.push_str("\n## Facts\n");
    if e.facts.is_empty() {
        md.push_str("(none available)\n");
    }
    for f in &e.facts {
        let mut marks = String::new();
        for ev in &f.evidence {
            cites.push(ev);
            let _ = write!(marks, "[{}]", cites.len());
        }
        if f.evidence.is_empty() {
            marks.push_str("[vault]");
        }
        let _ = writeln!(md, "- {} {marks}", inline(&f.text, 1000));
    }
    if !cites.is_empty() {
        md.push_str("\n## Sources\n");
        for (i, ev) in cites.iter().enumerate() {
            let _ = write!(
                md,
                "[{}] {} / {}",
                i + 1,
                inline(ev.item.source.as_str(), 200),
                inline(ev.item.key.as_str(), 300)
            );
            if let Some(t) = &ev.title {
                let _ = write!(md, " \"{}\"", inline(t, 200));
            }
            if let Some(u) = &ev.url {
                let _ = write!(md, " <{}>", inline(u, 300));
            }
            if let Some(q) = &ev.quote {
                let _ = write!(md, "\n    > {}", inline(q, 400));
            }
            md.push('\n');
        }
    }
    if !e.mentions.is_empty() {
        md.push_str("\n## Mentions\n");
        for m in &e.mentions {
            let _ = writeln!(
                md,
                "- \"{}\" in {} / {}{}",
                inline(&m.surface, 100),
                inline(m.item.source.as_str(), 200),
                inline(m.item.key.as_str(), 300),
                m.title
                    .as_deref()
                    .map(|t| format!(" \"{}\"", inline(t, 200)))
                    .unwrap_or_default()
            );
        }
    }
    let uri = render::entity_uri(e.id.as_str());
    let page = wrap(
        &[
            ("kind", Some("entity")),
            ("id", Some(e.id.as_str())),
            ("uri", Some(&uri)),
        ],
        &md,
    );
    let mut items: Vec<ItemRef> = Vec::new();
    for item in cites
        .iter()
        .map(|c| &c.item)
        .chain(e.mentions.iter().map(|m| &m.item))
    {
        if !items.contains(item) {
            items.push(item.clone());
        }
    }
    (page, items)
}

fn entity_json(se: &ScopedEntity) -> Value {
    let e = &se.entity;
    json!({
        "untrusted_content": true,
        "id": e.id,
        "kind": e.kind,
        "name": e.name,
        "uri": render::entity_uri(e.id.as_str()),
        "aliases": e.aliases,
        "identifiers": e.identifiers,
        "attributes": e.attributes,
        "details_withheld": se.details_withheld,
        "facts": e.facts,
        "mentions": e.mentions,
        "updated_at": e.updated_at.format(&Rfc3339).ok(),
    })
}

async fn get_neighbourhood(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: IdArgs = args(a)?;
    let depth = p.depth.unwrap_or(1).clamp(1, MAX_DEPTH);
    // The centre itself must be visible (same answer as for an unknown id otherwise).
    let center = load_entity(b, scope, &p.id).await?.entity.id;
    let view = scope.neighbourhood(
        &center,
        b.neighbourhood(&center, depth, NEIGHBOURHOOD_NODE_LIMIT, scope.filter())
            .await
            .map_err(ToolFailure::backend)?,
    );
    let mut md = String::from("Entities:\n");
    let name_of = |id: &EntityId| {
        view.nodes
            .iter()
            .find(|n| &n.id == id)
            .map_or_else(|| id.to_string(), |n| inline(&n.name, 200))
    };
    for n in &view.nodes {
        let _ = writeln!(
            md,
            "- {} ({}) id={} depth={}",
            inline(&n.name, 200),
            n.kind,
            inline(n.id.as_str(), 200),
            n.depth
        );
    }
    md.push_str("Relations:\n");
    for e in &view.edges {
        let _ = writeln!(
            md,
            "- {} --{}--> {}",
            name_of(&e.from),
            inline(&e.predicate, 100),
            name_of(&e.to)
        );
    }
    if view.truncated {
        md.push_str("(truncated: more entities are connected)\n");
    }
    let text = format!(
        "{RESULT_PREAMBLE}\n\n{}",
        wrap(
            &[
                ("kind", Some("neighbourhood")),
                ("center", Some(center.as_str()))
            ],
            &md
        )
    );
    Ok(ToolOutput {
        results: view.nodes.len(),
        items: Vec::new(),
        structured: json!({
            "untrusted_content": true,
            "center": center,
            "nodes": view.nodes.iter().map(|n| json!({"id": n.id, "kind": n.kind, "name": n.name, "depth": n.depth})).collect::<Vec<_>>(),
            "edges": view.edges.iter().map(|e| json!({"claim": e.claim, "from": e.from, "to": e.to, "predicate": e.predicate})).collect::<Vec<_>>(),
            "truncated": view.truncated,
        }),
        text,
    })
}

#[derive(Deserialize)]
struct ItemArgs {
    source: String,
    key: String,
    offset_chars: Option<usize>,
    max_chars: Option<usize>,
}

/// Read an item within the client's scopes (shared with `nugget://item/...` resources).
pub(crate) async fn load_item(
    b: &dyn KnowledgeBackend,
    scope: &Scope,
    source: &str,
    key: &str,
    offset: usize,
    max_chars: usize,
) -> ToolResult {
    if source.trim().is_empty() || key.is_empty() {
        return Err(ToolFailure::rejected("source and key are required"));
    }
    let item = ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    };
    // Check the source scope before touching the backend.
    if !scope.filter().allows(&item.source, false) {
        return Err(ToolFailure::rejected(NOT_AVAILABLE_ITEM));
    }
    let rec = b
        .get_item(&item, scope.filter())
        .await
        .map_err(ToolFailure::backend)?
        .filter(|r| r.item == item && scope.filter().allows(&r.item.source, r.sensitive))
        .ok_or_else(|| ToolFailure::rejected(NOT_AVAILABLE_ITEM))?;
    let full = rec.text.as_deref().unwrap_or("");
    let max = max_chars.clamp(1, MAX_ITEM_CHARS);
    let (window, total) = render::char_window(full, offset, max);
    let shown = window.chars().count();
    let end = offset.min(total) + shown;
    let mut text = format!(
        "{RESULT_PREAMBLE}\n\n{}",
        wrap(
            &[
                ("source", Some(rec.item.source.as_str())),
                ("item", Some(rec.item.key.as_str())),
                ("title", rec.title.as_deref()),
                ("url", rec.url.as_deref()),
                ("mime", rec.mime.as_deref()),
            ],
            window
        )
    );
    if end < total {
        let _ = write!(
            text,
            "\n(showing characters {}-{end} of {total}; call get_item again with offset_chars={end} to continue)",
            offset.min(total)
        );
    } else if rec.text.is_none() {
        text.push_str("\n(no text is stored for this item)");
    }
    Ok(ToolOutput {
        results: 1,
        items: vec![rec.item.clone()],
        structured: json!({
            "untrusted_content": true,
            "source": rec.item.source,
            "key": rec.item.key,
            "uri": render::item_uri(&rec.item),
            "title": rec.title,
            "url": rec.url,
            "mime": rec.mime,
            "sensitive": rec.sensitive,
            "source_modified_at": rec.source_modified_at.and_then(|t| t.format(&Rfc3339).ok()),
            "updated_at": rec.updated_at.format(&Rfc3339).ok(),
            "offset_chars": offset.min(total),
            "total_chars": total,
            "truncated": end < total,
            "text": window,
        }),
        text,
    })
}

async fn get_item(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: ItemArgs = args(a)?;
    load_item(
        b,
        scope,
        &p.source,
        &p.key,
        p.offset_chars.unwrap_or(0),
        p.max_chars.unwrap_or(DEFAULT_ITEM_CHARS),
    )
    .await
}

#[derive(Deserialize)]
struct RecentArgs {
    #[serde(default)]
    since: Option<String>,
    limit: Option<usize>,
}

async fn recent_changes(b: &dyn KnowledgeBackend, scope: &Scope, a: &JsonObject) -> ToolResult {
    let p: RecentArgs = args(a)?;
    let now = OffsetDateTime::now_utc();
    let since = match p.since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => now - time::Duration::days(DEFAULT_RECENT_DAYS),
        Some(s) => OffsetDateTime::parse(s, &Rfc3339).map_err(|_| {
            ToolFailure::rejected("since must be an RFC 3339 timestamp, e.g. 2026-10-01T00:00:00Z")
        })?,
    };
    let limit = p
        .limit
        .unwrap_or(DEFAULT_RECENT_LIMIT)
        .clamp(1, MAX_RECENT_LIMIT);
    let mut ch = scope.recent(
        b.recent_changes(since, limit, scope.filter())
            .await
            .map_err(ToolFailure::backend)?,
    );
    ch.entities.truncate(limit);
    ch.items.truncate(limit);
    let fmt = |t: OffsetDateTime| t.format(&Rfc3339).unwrap_or_default();
    let mut md = String::from("Entities:\n");
    for e in &ch.entities {
        let _ = writeln!(
            md,
            "- {} ({}) id={} updated {}",
            inline(&e.name, 200),
            e.kind,
            inline(e.id.as_str(), 200),
            fmt(e.updated_at)
        );
    }
    md.push_str("Items:\n");
    for i in &ch.items {
        let _ = writeln!(
            md,
            "- {} / {}{} updated {}",
            inline(i.item.source.as_str(), 200),
            inline(i.item.key.as_str(), 300),
            i.title
                .as_deref()
                .map(|t| format!(" \"{}\"", inline(t, 200)))
                .unwrap_or_default(),
            fmt(i.updated_at)
        );
    }
    let text = format!(
        "{RESULT_PREAMBLE}\n\nChanges since {}:\n{}",
        fmt(since),
        wrap(&[("kind", Some("recent_changes"))], &md)
    );
    Ok(ToolOutput {
        results: ch.entities.len() + ch.items.len(),
        items: ch.items.iter().map(|i| i.item.clone()).collect(),
        structured: json!({
            "untrusted_content": true,
            "since": fmt(since),
            "entities": ch.entities.iter().map(|e| json!({"id": e.id, "kind": e.kind, "name": e.name, "updated_at": fmt(e.updated_at), "uri": render::entity_uri(e.id.as_str())})).collect::<Vec<_>>(),
            "items": ch.items.iter().map(|i| json!({"source": i.item.source, "key": i.item.key, "title": i.title, "url": i.url, "updated_at": fmt(i.updated_at), "uri": render::item_uri(&i.item)})).collect::<Vec<_>>(),
        }),
        text,
    })
}
