//! MCP wiring: the [`KnowledgeBackend`] over the store, graph and retriever, the encrypted
//! client store, the server key, the loopback HTTP listener and the stdio bridge sessions.
//!
//! - **Clients** are kept as one JSON object under the store cursor key [`CLIENTS_KEY`] (inside
//!   the encrypted store; only keyed token hashes, never tokens).
//! - **Server key** (the HMAC key for token hashes) lives under [`SERVER_KEY_KEY`] in the same
//!   encrypted store, generated on first use. Keeping it next to the hashes costs nothing:
//!   tokens are 256-bit random values, so hashes cannot be brute-forced either way, and the store
//!   itself is encrypted with a keychain-held key.
//! - **Audit**: every MCP call becomes a store audit entry (actor `mcp:<client id>`), written in
//!   order by one background writer.
//! - **Bridge**: `nuggetd mcp --client <id>` opens a session with [`BRIDGE_OPEN`] over the
//!   normal IPC socket; MCP lines travel as [`BRIDGE_SEND`] calls (in) and [`BRIDGE_RECV`]
//!   notifications (out). The daemon authenticates the client with its token and serves it with
//!   `McpServer::serve_io` on an in-memory pipe, so scopes, revocation, rate limits and audit are
//!   all enforced here, never in the launched process.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use nugget_core::ids::{EntityId, ItemRef, SourceId};
use nugget_core::ipc::McpClientCreated;
use nugget_core::{Error, Result};
use nugget_graph::GraphStore;
use nugget_mcp::scope::distinct_origins;
use nugget_mcp::{
    ClientRecord, ClientRegistry, ClientStore, ContextPack, ContextPassage, DetailOrigins,
    EdgeRecord, EntityRecord, EntitySummaryRecord, EvidenceRecord, FactRecord, HttpConfig,
    HttpHandle, ItemRecord, ItemSummaryRecord, KnowledgeBackend, McpServer, MentionRecord,
    NeighbourhoodRecord, NodeRecord, Origin, RecentChanges, ScopeFilter, SearchRecord, ServerKey,
    SnippetOptions, Transport,
};
use nugget_retrieve::{ContextOptions, Retriever};
use nugget_store::{AuditEntry, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::McpSection;
use crate::daemon::blocking;
use crate::graphview::{self, Lookups, status_str};

/// Store cursor key holding MCP client records.
pub const CLIENTS_KEY: &str = "mcp/clients";
/// Store cursor key holding the MCP server key.
pub const SERVER_KEY_KEY: &str = "mcp/server_key";

/// Environment variable naming the data root; setup snippets pass it on a non-default home.
pub const HOME_ENV: &str = "NUGGET_HOME";

/// Bridge IPC methods (daemon-private; used only by `nuggetd mcp --client`).
pub const BRIDGE_OPEN: &str = "mcp.bridge.open";
pub const BRIDGE_SEND: &str = "mcp.bridge.send";
pub const BRIDGE_CLOSE: &str = "mcp.bridge.close";
/// Bridge notifications: one MCP message line for the session, and the session's end.
pub const BRIDGE_RECV: &str = "mcp.bridge.recv";
pub const BRIDGE_CLOSED: &str = "mcp.bridge.closed";
/// Longest MCP message line accepted or forwarded through the bridge.
pub const MAX_BRIDGE_LINE: usize = 8 * 1024 * 1024;
/// Mentions returned per entity by `get_entity`.
const ENTITY_MENTIONS: usize = 50;
/// Items scanned by `recent_changes` at most (until the store has a change feed for items).
const RECENT_ITEM_SCAN: usize = 20_000;

/// `mcp.bridge.open` params.
#[derive(Clone, Serialize, Deserialize)]
pub struct BridgeOpen {
    pub client: String,
    pub token: String,
}

impl std::fmt::Debug for BridgeOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeOpen")
            .field("client", &self.client)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// `mcp.bridge.open` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSession {
    pub session: String,
}

/// `mcp.bridge.send` params (one MCP message, without the newline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeLine {
    pub session: String,
    pub line: String,
}

// ---- client store ----

/// [`ClientStore`] in the encrypted store, cached in memory (this daemon is the only writer).
pub(crate) struct StoreClientStore {
    store: Store,
    cache: Mutex<Option<BTreeMap<String, ClientRecord>>>,
}

impl StoreClientStore {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Mutex::new(None),
        }
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, ClientRecord>) -> (T, bool),
    ) -> Result<T> {
        let mut guard: MutexGuard<'_, _> =
            self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            let map = match self.store.get_cursor(&SourceId::new(CLIENTS_KEY))? {
                Some(v) => serde_json::from_value(v)
                    .map_err(|e| Error::Integrity(format!("unreadable MCP client records: {e}")))?,
                None => BTreeMap::new(),
            };
            *guard = Some(map);
        }
        let map = guard.as_mut().expect("loaded above");
        let mut next = map.clone();
        let (out, dirty) = f(&mut next);
        if dirty {
            self.store
                .set_cursor(&SourceId::new(CLIENTS_KEY), serde_json::to_value(&next)?)?;
            *map = next;
        }
        Ok(out)
    }
}

impl ClientStore for StoreClientStore {
    fn put(&self, record: &ClientRecord) -> Result<()> {
        self.with(|m| {
            m.insert(record.info.id.clone(), record.clone());
            ((), true)
        })
    }

    fn get(&self, id: &str) -> Result<Option<ClientRecord>> {
        self.with(|m| (m.get(id).cloned(), false))
    }

    fn find_by_token_hash(&self, token_hash: &str) -> Result<Option<ClientRecord>> {
        self.with(|m| {
            (
                m.values().find(|r| r.token_hash == token_hash).cloned(),
                false,
            )
        })
    }

    fn list(&self) -> Result<Vec<ClientRecord>> {
        self.with(|m| (m.values().cloned().collect(), false))
    }

    fn remove(&self, id: &str) -> Result<bool> {
        self.with(|m| {
            let removed = m.remove(id).is_some();
            (removed, removed)
        })
    }

    fn set_last_used(&self, id: &str, at: OffsetDateTime) -> Result<()> {
        self.with(|m| match m.get_mut(id) {
            Some(r) => {
                r.info.last_used = Some(at);
                ((), true)
            }
            None => ((), false),
        })
    }
}

/// The server key from the store, generated (and saved) on first use.
pub(crate) fn load_server_key(store: &Store) -> Result<ServerKey> {
    let key = SourceId::new(SERVER_KEY_KEY);
    if let Some(v) = store.get_cursor(&key)? {
        let hex = v.get("key").and_then(Value::as_str).unwrap_or_default();
        return decode_key(hex)
            .map(ServerKey::from_bytes)
            .ok_or_else(|| Error::Integrity("unreadable MCP server key".into()));
    }
    let k = ServerKey::generate()?;
    store.set_cursor(&key, json!({"v": 1, "key": encode_hex(k.expose_bytes())}))?;
    Ok(k)
}

fn encode_hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

fn decode_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

// ---- backend ----

/// [`KnowledgeBackend`] over the daemon's store, graph and retriever.
pub(crate) struct DaemonBackend {
    pub store: Store,
    pub graph: Arc<dyn GraphStore>,
    pub retriever: Retriever,
}

/// Run `f` with a fresh [`Lookups`] on the blocking pool.
async fn with_lookups<T, F>(store: &Store, graph: &Arc<dyn GraphStore>, f: F) -> Result<T>
where
    F: FnOnce(&mut Lookups<'_>) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let store = store.clone();
    let graph = graph.clone();
    blocking(move || {
        let mut lk = Lookups::new(&store, graph.as_ref());
        f(&mut lk)
    })
    .await
}

/// Run a future that makes synchronous store calls (retrieval) on the blocking pool.
async fn off_workers<T, F>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    blocking(move || handle.block_on(fut)).await
}

fn summary_record(
    lk: &mut Lookups<'_>,
    s: nugget_graph::EntitySummary,
) -> Result<EntitySummaryRecord> {
    let origins = lk.entity_origins(&s.id)?;
    Ok(EntitySummaryRecord {
        id: s.id,
        kind: s.kind.as_str().to_owned(),
        name: s.name,
        claims: s.claims,
        mentions: s.mentions,
        updated_at: s.updated_at,
        origins,
    })
}

#[async_trait]
impl KnowledgeBackend for DaemonBackend {
    async fn search(
        &self,
        query: &str,
        limit: usize,
        filter: &ScopeFilter,
    ) -> Result<Vec<SearchRecord>> {
        let restricted = filter.sources.is_some() || !filter.include_sensitive;
        let fetch = if restricted {
            limit.saturating_mul(4).min(200)
        } else {
            limit
        };
        let retriever = self.retriever.clone();
        let q = query.to_owned();
        let results = off_workers(async move { retriever.search(&q, fetch).await }).await?;
        let filter = filter.clone();
        let store = self.store.clone();
        blocking(move || {
            let mut out = Vec::new();
            for r in results.items {
                let sensitive = match store.get_item(&r.item)? {
                    Some(it) => nugget_retrieve::is_sensitive(&it.metadata),
                    None => continue,
                };
                if !filter.allows(&r.item.source, sensitive) {
                    continue;
                }
                out.push(SearchRecord {
                    item: r.item,
                    chunk_id: Some(r.chunk_id),
                    title: r.title,
                    url: r.url,
                    snippet: r.snippet,
                    score: r.score,
                    sensitive,
                });
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
        .await
    }

    async fn context(
        &self,
        query: &str,
        budget_tokens: u32,
        filter: &ScopeFilter,
    ) -> Result<ContextPack> {
        let retriever = self.retriever.clone();
        let q = query.to_owned();
        let opts = ContextOptions {
            exclude_sensitive: !filter.include_sensitive,
        };
        let packed = off_workers(async move {
            retriever
                .pack_context_with(&q, budget_tokens as usize, opts)
                .await
        })
        .await?;
        let filter = filter.clone();
        let store = self.store.clone();
        blocking(move || {
            let mut passages = Vec::new();
            for p in &packed.passages {
                let cite = packed.response.citations.iter().find(|c| c.n == p.n);
                let sensitive = cite.is_none_or(|c| c.sensitive);
                if !filter.allows(&p.item.source, sensitive) {
                    continue;
                }
                let mut chunks = store.get_chunks(&p.chunk_ids)?;
                chunks.sort_by_key(|c| c.ord);
                let text = chunks
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if text.trim().is_empty() {
                    continue;
                }
                passages.push(ContextPassage {
                    item: p.item.clone(),
                    chunk_id: cite
                        .map(|c| c.chunk_id)
                        .or_else(|| p.chunk_ids.first().copied())
                        .unwrap_or_default(),
                    title: cite.and_then(|c| c.title.clone()),
                    url: cite.and_then(|c| c.url.clone()),
                    sensitive,
                    text,
                });
            }
            Ok(ContextPack {
                passages,
                excluded_sensitive: packed.response.excluded_sensitive,
            })
        })
        .await
    }

    async fn find_entities(
        &self,
        text: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
        filter: &ScopeFilter,
    ) -> Result<Vec<EntitySummaryRecord>> {
        let kinds = match kind {
            Some(k) => vec![graphview::parse_kind(k)?],
            None => Vec::new(),
        };
        let text = text.map(str::to_owned);
        let filter = filter.clone();
        with_lookups(&self.store, &self.graph, move |lk| {
            let (mut all, _) = graphview::scan_entities(lk.graph, text.as_deref(), &kinds)?;
            all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            let mut out = Vec::new();
            let mut skipped = 0;
            for s in all {
                let rec = summary_record(lk, s)?;
                if !filter.allows_any(&rec.origins) {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                out.push(rec);
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
        .await
    }

    async fn get_entity(
        &self,
        id: &EntityId,
        _filter: &ScopeFilter,
    ) -> Result<Option<EntityRecord>> {
        let id = id.clone();
        with_lookups(&self.store, &self.graph, move |lk| {
            let Some(e) = lk.entity(&id)? else {
                return Ok(None);
            };
            let claims = lk.graph.claims_for(&e.id, false)?;
            let mentions = lk.graph.mentions_for(&e.id, graphview::ORIGIN_MENTIONS)?;
            let origins = lk.origins_of(&claims, &mentions)?;
            let mut facts = Vec::with_capacity(claims.len());
            for c in &claims {
                if !c.status.is_active() {
                    continue;
                }
                let mut evidence = Vec::with_capacity(c.evidence.len());
                for ev in &c.evidence {
                    let info = lk.item(&ev.item)?;
                    evidence.push(EvidenceRecord {
                        item: ev.item.clone(),
                        chunk_id: ev.chunk_id,
                        quote: ev.quote.clone(),
                        title: info.title,
                        url: info.url,
                        sensitive: info.sensitive,
                    });
                }
                facts.push(FactRecord {
                    id: c.id,
                    subject: lk.link(&c.subject)?,
                    predicate: c.predicate.clone(),
                    object: lk.object(&c.object)?,
                    text: c.text.clone(),
                    confidence: c.confidence,
                    status: status_str(c.status),
                    evidence,
                    valid_from: c.valid_from,
                    valid_to: c.valid_to,
                });
            }
            let mut mention_records = Vec::new();
            for m in mentions.iter().take(ENTITY_MENTIONS) {
                let info = lk.item(&m.item)?;
                mention_records.push(MentionRecord {
                    item: m.item.clone(),
                    chunk_id: m.chunk_id,
                    surface: m.surface.clone(),
                    title: info.title,
                    url: info.url,
                    sensitive: info.sensitive,
                });
            }
            Ok(Some(EntityRecord {
                id: e.id.clone(),
                kind: e.kind.as_str().to_owned(),
                name: e.name.clone(),
                aliases: e.aliases.clone(),
                identifiers: e
                    .identifiers
                    .iter()
                    .map(|i| (i.scheme.clone(), i.value.clone()))
                    .collect(),
                attributes: e.attributes.clone(),
                facts,
                mentions: mention_records,
                updated_at: e.updated_at,
                origins,
            }))
        })
        .await
    }

    async fn neighbourhood(
        &self,
        id: &EntityId,
        depth: u32,
        limit: usize,
        _filter: &ScopeFilter,
    ) -> Result<NeighbourhoodRecord> {
        let id = id.clone();
        with_lookups(&self.store, &self.graph, move |lk| {
            let Some(center) = lk.entity(&id)? else {
                return Ok(NeighbourhoodRecord::default());
            };
            let view = lk.graph.neighbourhood(&center.id, depth, limit)?;
            let mut nodes = Vec::with_capacity(view.nodes.len());
            for n in view.nodes {
                let origins = lk.entity_origins(&n.id)?;
                nodes.push(NodeRecord {
                    id: n.id,
                    kind: n.kind.as_str().to_owned(),
                    name: n.name,
                    depth: n.depth,
                    origins,
                });
            }
            let mut edges = Vec::with_capacity(view.edges.len());
            for e in view.edges {
                let origins = match lk.graph.get_claim(e.claim)? {
                    Some(c) => lk.claim_origins(&c)?,
                    None => continue,
                };
                edges.push(EdgeRecord {
                    claim: e.claim,
                    from: e.from,
                    to: e.to,
                    predicate: e.predicate,
                    origins,
                });
            }
            Ok(NeighbourhoodRecord {
                nodes,
                edges,
                truncated: view.truncated,
            })
        })
        .await
    }

    async fn get_item(&self, item: &ItemRef, filter: &ScopeFilter) -> Result<Option<ItemRecord>> {
        let item = item.clone();
        let filter = filter.clone();
        let store = self.store.clone();
        blocking(move || {
            let Some(it) = store.get_item(&item)? else {
                return Ok(None);
            };
            let sensitive = nugget_retrieve::is_sensitive(&it.metadata);
            if !filter.allows(&it.item.source, sensitive) {
                return Ok(None);
            }
            let text = match it.text.clone() {
                Some(t) => Some(t),
                None => {
                    let ords: Vec<u32> = (0..512).collect();
                    let chunks = store.get_chunks_by_ord(&it.item, &ords)?;
                    (!chunks.is_empty()).then(|| {
                        chunks
                            .iter()
                            .map(|c| c.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                }
            };
            Ok(Some(ItemRecord {
                url: graphview::item_url(&it.metadata),
                item: it.item,
                title: it.title,
                mime: it.mime,
                text,
                sensitive,
                source_modified_at: it.source_modified_at,
                updated_at: it.updated_at,
            }))
        })
        .await
    }

    async fn recent_changes(
        &self,
        since: OffsetDateTime,
        limit: usize,
        filter: &ScopeFilter,
    ) -> Result<RecentChanges> {
        let filter = filter.clone();
        with_lookups(&self.store, &self.graph, move |lk| {
            let (mut all, _) = graphview::scan_entities(lk.graph, None, &[])?;
            all.retain(|e| e.updated_at > since);
            all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            let mut entities = Vec::new();
            for s in all {
                let rec = summary_record(lk, s)?;
                if filter.allows_any(&rec.origins) {
                    entities.push(rec);
                }
                if entities.len() >= limit {
                    break;
                }
            }
            // No item change feed in the store yet: scan item keys (bounded).
            let mut items = Vec::new();
            let mut scanned = 0usize;
            'sources: for sc in lk.store.source_counts()? {
                if filter
                    .sources
                    .as_ref()
                    .is_some_and(|l| !l.contains(&sc.source))
                {
                    continue;
                }
                for key in lk.store.item_versions(&sc.source)?.into_keys() {
                    scanned += 1;
                    if scanned > RECENT_ITEM_SCAN {
                        break 'sources;
                    }
                    let r = ItemRef {
                        source: sc.source.clone(),
                        key,
                    };
                    let Some(it) = lk.store.get_item(&r)? else {
                        continue;
                    };
                    if it.updated_at <= since {
                        continue;
                    }
                    let sensitive = nugget_retrieve::is_sensitive(&it.metadata);
                    if !filter.allows(&r.source, sensitive) {
                        continue;
                    }
                    items.push(ItemSummaryRecord {
                        url: graphview::item_url(&it.metadata),
                        item: it.item,
                        title: it.title,
                        updated_at: it.updated_at,
                        sensitive,
                    });
                }
            }
            items.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
            items.truncate(limit);
            Ok(RecentChanges { entities, items })
        })
        .await
    }

    /// `GraphStore::entity_provenance` with each supplying item mapped to its [`Origin`]
    /// (source plus the item's sensitivity from the store; a missing item counts as sensitive).
    /// The MCP layer applies the client's scope to these, so nothing is filtered here.
    async fn entity_detail_origins(
        &self,
        id: &EntityId,
        _filter: &ScopeFilter,
    ) -> Result<Option<DetailOrigins>> {
        let id = id.clone();
        with_lookups(&self.store, &self.graph, move |lk| {
            let Some(p) = lk.graph.entity_provenance(&id)? else {
                return Ok(None);
            };
            let mut origins = |items: &[ItemRef]| -> Result<Vec<Origin>> {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(Origin {
                        source: item.source.clone(),
                        sensitive: lk.item(item)?.sensitive,
                    });
                }
                Ok(distinct_origins(out))
            };
            let mut details = DetailOrigins::default();
            for (alias, items) in &p.aliases {
                details.aliases.push((alias.clone(), origins(items)?));
            }
            for (ident, items) in &p.identifiers {
                details
                    .identifiers
                    .push(((ident.scheme.clone(), ident.value.clone()), origins(items)?));
            }
            Ok(Some(details))
        })
        .await
    }
}

// ---- service ----

/// Bridge sessions by id: the sender feeding MCP lines into the session's pipe.
type Sessions = Mutex<HashMap<String, mpsc::Sender<String>>>;

/// The daemon's MCP server and its clients.
pub(crate) struct McpService {
    pub server: McpServer,
    pub registry: Arc<ClientRegistry>,
    http: Mutex<Option<HttpHandle>>,
    http_port: Option<u16>,
    /// `nuggetd` as setup snippets launch it.
    nuggetd: String,
    /// `NUGGET_HOME` the snippets must pass, when the daemon runs on a non-default home.
    home: Option<PathBuf>,
    sessions: Arc<Sessions>,
}

impl McpService {
    /// Build the server, and start the HTTP listener when `[mcp] http` is on. A listener that
    /// cannot bind (port taken) is logged; stdio keeps working.
    pub async fn start(
        cfg: &McpSection,
        backend: Arc<dyn KnowledgeBackend>,
        store: Store,
        nuggetd: String,
        home: Option<PathBuf>,
        tasks: &TaskTracker,
        stop: CancellationToken,
    ) -> Result<Self> {
        let key = {
            let store = store.clone();
            blocking(move || load_server_key(&store)).await?
        };
        let clients: Arc<dyn ClientStore> = Arc::new(StoreClientStore::new(store.clone()));
        let registry = Arc::new(ClientRegistry::new(clients, key));
        let audit = spawn_audit_writer(store, tasks, stop);
        let server = McpServer::builder(backend)
            .registry(registry.clone())
            .audit(move |e| {
                let _ = audit.send(AuditEntry {
                    actor: e.actor,
                    action: e.action,
                    target: e.target,
                    detail: e.detail,
                });
            })
            .build();
        // Fail closed: bind the configured port or nothing. Setup snippets (and every other
        // pointer at the listener) use the port actually held, so when the bind fails (say the
        // port is taken by another program) no client is ever pointed at it; stdio still works.
        let mut http = None;
        if cfg.http {
            match server
                .serve_http(HttpConfig {
                    port: cfg.port,
                    allowed_origins: cfg.allowed_origins.clone(),
                    max_connections: cfg.max_connections,
                    header_timeout_ms: cfg.header_timeout_ms,
                    body_timeout_ms: cfg.body_timeout_ms,
                })
                .await
            {
                Ok(h) => {
                    tracing::info!(url = %h.url(), "mcp http listening");
                    http = Some(h);
                }
                Err(e) => tracing::error!(
                    port = cfg.port,
                    error = %e,
                    "mcp http listener not started (is the port taken?); HTTP setup is not \
                     offered, stdio clients still work"
                ),
            }
        }
        Ok(Self {
            server,
            registry,
            // Only ever a port this daemon holds.
            http_port: http.as_ref().map(|h| h.local_addr().port()),
            http: Mutex::new(http),
            nuggetd,
            home,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn http_addr(&self) -> Option<std::net::SocketAddr> {
        self.http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|h| h.local_addr())
    }

    pub async fn shutdown(&self) {
        let h = self
            .http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(h) = h {
            h.shutdown().await;
        }
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The `mcp.clients.create` result, with snippets that launch this `nuggetd` on this home.
    pub fn created(
        &self,
        info: nugget_core::ipc::McpClientInfo,
        token: &nugget_mcp::ClientToken,
    ) -> McpClientCreated {
        nugget_mcp::client_created(info, token, &self.snippet_options())
    }

    /// Snippet options for this daemon: its `nuggetd`, its HTTP port and, on a non-default
    /// home, `NUGGET_HOME` so the launched bridge finds this daemon.
    pub fn snippet_options(&self) -> SnippetOptions {
        snippet_options(self.nuggetd.clone(), self.http_port, self.home.as_ref())
    }

    /// Open a bridge session for an authenticated client; MCP output goes to `notify`.
    pub async fn bridge_open(
        &self,
        req: BridgeOpen,
        notify: nugget_ipc::Notifier,
        tasks: &TaskTracker,
        stop: CancellationToken,
    ) -> Result<BridgeSession> {
        let registry = self.registry.clone();
        let client = blocking(move || registry.authenticate_client(&req.client, &req.token))
            .await?
            .ok_or_else(|| {
                Error::Unauthorized("unknown or revoked MCP client, or wrong token".into())
            })?;
        let session = format!("mcpb_{}", uuid::Uuid::new_v4().simple());
        let (daemon_io, bridge_io) = tokio::io::duplex(256 * 1024);
        let (dr, dw) = tokio::io::split(daemon_io);
        let (br, mut bw) = tokio::io::split(bridge_io);
        let (tx, mut rx) = mpsc::channel::<String>(64);
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session.clone(), tx);
        tracing::info!(client = %client.id, %session, "mcp bridge session opened");

        // The MCP session itself.
        let server = self.server.clone();
        let stop_srv = stop.clone();
        let client_id = client.id.clone();
        tasks.spawn(async move {
            tokio::select! {
                r = server.serve_io(client, Transport::Stdio, dr, dw) => if let Err(e) = r {
                    tracing::debug!(client = %client_id, error = %e, "mcp bridge session ended");
                },
                () = stop_srv.cancelled() => {}
            }
        });
        // Bridge → MCP.
        tasks.spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if bw.write_all(line.as_bytes()).await.is_err() || bw.flush().await.is_err() {
                    break;
                }
            }
            let _ = bw.shutdown().await;
        });
        // MCP → bridge, until either side ends.
        let sessions = self.sessions.clone();
        let id = session.clone();
        tasks.spawn(async move {
            let mut reader = BufReader::new(br);
            loop {
                let mut buf = Vec::new();
                let mut limited = (&mut reader).take(MAX_BRIDGE_LINE as u64 + 1);
                let read = tokio::select! {
                    r = limited.read_until(b'\n', &mut buf) => r,
                    () = notify.closed() => break,
                    () = stop.cancelled() => break,
                };
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if buf.len() > MAX_BRIDGE_LINE {
                    tracing::warn!(session = %id, "mcp message too large for the bridge; closing");
                    break;
                }
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                let Ok(line) = String::from_utf8(buf) else {
                    break;
                };
                if line.is_empty() {
                    continue;
                }
                if notify
                    .send(BRIDGE_RECV, &json!({"session": id, "line": line}))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            let _ = notify.send(BRIDGE_CLOSED, &json!({"session": id})).await;
            tracing::info!(session = %id, "mcp bridge session closed");
        });
        Ok(BridgeSession { session })
    }

    pub async fn bridge_send(&self, req: BridgeLine) -> Result<()> {
        if req.line.len() > MAX_BRIDGE_LINE || req.line.contains('\n') {
            return Err(Error::Invalid(
                "bridge lines must be single MCP messages".into(),
            ));
        }
        let tx = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&req.session)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("bridge session {}", req.session)))?;
        tx.send(req.line)
            .await
            .map_err(|_| Error::NotFound(format!("bridge session {}", req.session)))
    }

    pub fn bridge_close(&self, session: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session)
            .is_some()
    }
}

/// Setup snippet options: launch `nuggetd`, point HTTP clients at `http_port`, and pass
/// [`HOME_ENV`] when the daemon runs on a non-default `home`.
fn snippet_options(
    nuggetd: String,
    http_port: Option<u16>,
    home: Option<&PathBuf>,
) -> SnippetOptions {
    SnippetOptions {
        nuggetd,
        http_port,
        env: home
            .map(|h| (HOME_ENV.to_owned(), h.display().to_string()))
            .into_iter()
            .collect(),
        ..SnippetOptions::default()
    }
}

/// One writer for MCP audit entries, so they land in call order without blocking requests.
fn spawn_audit_writer(
    store: Store,
    tasks: &TaskTracker,
    stop: CancellationToken,
) -> mpsc::UnboundedSender<AuditEntry> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AuditEntry>();
    tasks.spawn(async move {
        loop {
            let first = tokio::select! {
                e = rx.recv() => e,
                () = stop.cancelled() => None,
            };
            let Some(first) = first else {
                break;
            };
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            let store = store.clone();
            let _ = blocking(move || {
                for e in &batch {
                    if let Err(err) = store.append_audit(e) {
                        tracing::warn!(action = %e.action, error = %err, "cannot append mcp audit entry");
                    }
                }
                Ok(())
            })
            .await;
        }
        // Flush what is left at shutdown.
        let mut rest = Vec::new();
        while let Ok(e) = rx.try_recv() {
            rest.push(e);
        }
        if !rest.is_empty() {
            let _ = blocking(move || {
                for e in &rest {
                    let _ = store.append_audit(e);
                }
                Ok(())
            })
            .await;
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_records_persist_in_the_store() {
        let store = Store::open_in_memory().unwrap();
        let reg = ClientRegistry::new(
            Arc::new(StoreClientStore::new(store.clone())),
            load_server_key(&store).unwrap(),
        );
        let (info, token) = reg
            .create(
                "Cursor",
                nugget_core::ipc::McpScopes {
                    sources: None,
                    tools: vec![],
                    include_sensitive: false,
                },
            )
            .unwrap();
        // A second registry over the same store (a restart) knows the client and its token.
        let again = ClientRegistry::new(
            Arc::new(StoreClientStore::new(store.clone())),
            load_server_key(&store).unwrap(),
        );
        assert_eq!(
            again.authenticate(token.expose()).unwrap().unwrap().id,
            info.id
        );
        let raw = store
            .get_cursor(&SourceId::new(CLIENTS_KEY))
            .unwrap()
            .unwrap()
            .to_string();
        assert!(!raw.contains(token.expose()), "tokens are never stored");
        assert!(again.revoke(&info.id).unwrap());
        let third = ClientRegistry::new(
            Arc::new(StoreClientStore::new(store.clone())),
            load_server_key(&store).unwrap(),
        );
        assert!(third.authenticate(token.expose()).unwrap().is_none());
    }

    #[test]
    fn server_key_round_trips() {
        let k = [7u8; 32];
        assert_eq!(decode_key(&encode_hex(&k)), Some(k));
        assert_eq!(decode_key("zz"), None);
    }

    #[test]
    fn snippets_get_the_home() {
        let opts = snippet_options("nuggetd".into(), None, Some(&PathBuf::from("/my home")));
        assert_eq!(opts.env, vec![(HOME_ENV.to_owned(), "/my home".to_owned())]);
        assert!(
            snippet_options("nuggetd".into(), Some(1), None)
                .env
                .is_empty()
        );
        let info = nugget_core::ipc::McpClientInfo {
            id: "mcp_c".into(),
            name: "C".into(),
            scopes: nugget_core::ipc::McpScopes {
                sources: None,
                tools: vec![],
                include_sensitive: false,
            },
            created_at: OffsetDateTime::UNIX_EPOCH,
            last_used: None,
        };
        let s = nugget_mcp::setup_snippets(&info, "t", &opts);
        let v: Value = serde_json::from_str(&s[0].text).unwrap();
        assert_eq!(v["mcpServers"]["nugget"]["env"]["NUGGET_HOME"], "/my home");
        assert_eq!(v["mcpServers"]["nugget"]["env"]["NUGGET_MCP_TOKEN"], "t");
        assert!(
            s[1].text
                .contains("--env NUGGET_HOME='/my home' --env NUGGET_MCP_TOKEN=t"),
            "{}",
            s[1].text
        );
    }

    /// A client scoped away from a source (or from sensitive items) sees an entity's aliases
    /// and identifiers only when an item it may read supplied them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detail_origins_give_scoped_clients_per_identifier_visibility() {
        use nugget_core::ipc::McpScopes;
        use nugget_graph::{EntityDetail, EntityKind, Identifier, Mention, NewEntity};
        use nugget_mcp::{Origin, Scope};
        use nugget_store::NewItem;

        let store = Store::open_in_memory().unwrap();
        let put = |source: &str, key: &str, sensitive: bool| {
            let mut it = NewItem::new(source, key, format!("h-{source}-{key}"));
            it.metadata = json!({ "sensitive": sensitive });
            store.upsert_item(&it).unwrap();
            ItemRef {
                source: SourceId::new(source),
                key: nugget_core::ids::ItemKey::new(key),
            }
        };
        let note = put("notes", "sam.md", false);
        let secret = put("notes", "secret.md", true);
        let mail = put("mail", "m1", false);

        let graph: Arc<dyn GraphStore> = Arc::new(nugget_graph::MemGraph::new());
        let mut new = NewEntity::new(EntityKind::Person, "Sam Lee");
        new.aliases = vec!["Sammy".into(), "S.L.".into(), "Boss".into()];
        new.identifiers = vec![
            Identifier::new("email", "sam@work.test"),
            Identifier::new("phone", "+15550100"),
            Identifier::new("handle:chat", "samlee"),
        ];
        let sam = graph.create_entity(&new).unwrap();
        for item in [&note, &secret, &mail] {
            graph
                .add_mention(&Mention {
                    entity: sam.clone(),
                    item: item.clone(),
                    chunk_id: None,
                    surface: "Sam".into(),
                    confidence: 0.9,
                })
                .unwrap();
        }
        let alias = |a: &str| EntityDetail::Alias(a.into());
        let ident = |s: &str, v: &str| EntityDetail::Identifier(Identifier::new(s, v));
        for (detail, item) in [
            (alias("Sammy"), &note),
            (alias("S.L."), &secret),
            (alias("Boss"), &mail),
            (ident("email", "sam@work.test"), &mail),
            (ident("email", "sam@work.test"), &note),
            (ident("phone", "+15550100"), &secret),
            (ident("handle:chat", "samlee"), &mail),
        ] {
            graph.add_detail_source(&sam, &detail, item).unwrap();
        }

        let backend = DaemonBackend {
            store: store.clone(),
            graph: graph.clone(),
            retriever: Retriever::new(store.clone(), None, Default::default()),
        };
        let all = ScopeFilter {
            sources: None,
            include_sensitive: true,
        };
        let d = backend
            .entity_detail_origins(&sam, &all)
            .await
            .unwrap()
            .unwrap();
        let origins_of = |scheme: &str| {
            d.identifiers
                .iter()
                .find(|(i, _)| i.0 == scheme)
                .map(|(_, o)| o.clone())
                .unwrap()
        };
        assert_eq!(
            origins_of("email"),
            vec![Origin::new("mail", false), Origin::new("notes", false)]
        );
        assert_eq!(origins_of("phone"), vec![Origin::new("notes", true)]);
        assert!(
            backend
                .entity_detail_origins(&EntityId::new("ent_nope"), &all)
                .await
                .unwrap()
                .is_none()
        );

        let view = async |sources: Option<&[&str]>, include_sensitive: bool| {
            let scope = Scope::new(&McpScopes {
                sources: sources.map(|l| l.iter().map(|s| SourceId::new(*s)).collect()),
                tools: vec![],
                include_sensitive,
            });
            let e = backend
                .get_entity(&sam, scope.filter())
                .await
                .unwrap()
                .unwrap();
            let details = backend
                .entity_detail_origins(&sam, scope.filter())
                .await
                .unwrap();
            let se = scope.entity_with_details(e, details.as_ref()).unwrap();
            let mut aliases = se.entity.aliases.clone();
            aliases.sort();
            let mut ids: Vec<_> = se.entity.identifiers.iter().map(|i| i.0.clone()).collect();
            ids.sort();
            (aliases, ids, se.details_withheld)
        };
        // Notes only, no sensitive items: mail-only and sensitive-only details are withheld.
        let (aliases, ids, withheld) = view(Some(&["notes"]), false).await;
        assert_eq!(aliases, ["Sammy"]);
        assert_eq!(ids, ["email"]);
        assert!(withheld);
        // Notes with sensitive items: the secret note's details too, never mail's.
        let (aliases, ids, _) = view(Some(&["notes"]), true).await;
        assert_eq!(aliases, ["S.L.", "Sammy"]);
        assert_eq!(ids, ["email", "phone"]);
        // Mail only: its own alias and identifiers.
        let (aliases, ids, _) = view(Some(&["mail"]), false).await;
        assert_eq!(aliases, ["Boss"]);
        assert_eq!(ids, ["email", "handle:chat"]);
        // Everything allowed: everything shown.
        let (aliases, ids, withheld) = view(None, true).await;
        assert_eq!((aliases.len(), ids.len(), withheld), (3, 3, false));
    }
}
