//! The MCP server core and its `rmcp` handler.

use std::collections::HashMap;
use std::sync::Arc;

use nugget_core::egress::EgressSink;
use nugget_core::ids::ItemRef;
use nugget_core::ipc::McpClientInfo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio_util::sync::CancellationToken;

use crate::audit::{
    ACTION_RESOURCE, ACTION_TOOL, AuditEntry, AuditSink, CallRecord, Outcome, Transport,
};
use crate::auth::{ClientRegistry, ServerKey};
use crate::backend::KnowledgeBackend;
use crate::rate::{RateLimit, RateLimiter};
use crate::render::{self, RESULT_PREAMBLE, ResourceUri, SAFETY_NOTE};
use crate::scope::Scope;
use crate::tools::{self, ToolFailure, ToolOutput};

/// Server name reported in `initialize`.
pub const SERVER_NAME: &str = "nugget";
/// Entities listed by `resources/list` per page.
const RESOURCE_PAGE: usize = 100;

/// Tunables.
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    /// Per-client request rate.
    pub rate_limit: RateLimit,
}

struct Inner {
    backend: Arc<dyn KnowledgeBackend>,
    registry: Option<Arc<ClientRegistry>>,
    audit: Option<AuditSink>,
    /// Keys the hashes of free-text arguments in audit entries: the registry's key, so they stay
    /// comparable across restarts, or a per-process one without a registry.
    audit_key: Option<ServerKey>,
    egress: Option<Arc<dyn EgressSink>>,
    limiter: RateLimiter,
    refusals: RefusalLog,
}

/// How long refusals of one kind for one client are coalesced into one audit entry.
pub const REFUSAL_AUDIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Coalesces audit entries for refused requests (rate limited, revoked), so a runaway client
/// cannot flood the audit log: per client and outcome, at most one entry per
/// [`REFUSAL_AUDIT_WINDOW`]. The entry that opens a window carries `refused_since_last_entry`,
/// the number of refusals of that kind that were coalesced (not written) since the previous one.
#[derive(Default)]
struct RefusalLog {
    windows: std::sync::Mutex<HashMap<(String, Outcome), RefusalWindow>>,
}

struct RefusalWindow {
    opened: std::time::Instant,
    coalesced: u64,
}

impl RefusalLog {
    /// `Some(coalesced since the last entry)` when this refusal should be written.
    fn admit(&self, client: &str, outcome: Outcome) -> Option<u64> {
        let now = std::time::Instant::now();
        let mut w = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        if w.len() > 10_000 {
            w.retain(|_, v| now.duration_since(v.opened) < REFUSAL_AUDIT_WINDOW);
        }
        match w.get_mut(&(client.to_owned(), outcome)) {
            Some(win) if now.duration_since(win.opened) < REFUSAL_AUDIT_WINDOW => {
                win.coalesced += 1;
                None
            }
            Some(win) => {
                let n = win.coalesced;
                *win = RefusalWindow {
                    opened: now,
                    coalesced: 0,
                };
                Some(n)
            }
            None => {
                w.insert(
                    (client.to_owned(), outcome),
                    RefusalWindow {
                        opened: now,
                        coalesced: 0,
                    },
                );
                Some(0)
            }
        }
    }
}

/// Nugget's MCP server. Cheap to clone; one instance serves every client and transport.
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer").finish_non_exhaustive()
    }
}

/// Builder for [`McpServer`].
pub struct McpServerBuilder {
    backend: Arc<dyn KnowledgeBackend>,
    registry: Option<Arc<ClientRegistry>>,
    audit: Option<AuditSink>,
    egress: Option<Arc<dyn EgressSink>>,
    config: McpConfig,
}

impl McpServerBuilder {
    /// Client registry: required for HTTP; for stdio it makes revocation and scope changes apply
    /// to running sessions.
    pub fn registry(mut self, registry: Arc<ClientRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Called with one [`AuditEntry`] per tool call and resource read.
    pub fn audit(mut self, sink: impl Fn(AuditEntry) + Send + Sync + 'static) -> Self {
        self.audit = Some(Arc::new(sink));
        self
    }

    /// Egress ledger, used only for remote clients (none exist in M2).
    pub fn egress(mut self, sink: Arc<dyn EgressSink>) -> Self {
        self.egress = Some(sink);
        self
    }

    pub fn config(mut self, config: McpConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> McpServer {
        let audit_key = match &self.registry {
            Some(r) => Some(r.key().clone()),
            None => ServerKey::generate().ok(),
        };
        McpServer {
            inner: Arc::new(Inner {
                backend: self.backend,
                registry: self.registry,
                audit: self.audit,
                audit_key,
                egress: self.egress,
                limiter: RateLimiter::new(self.config.rate_limit),
                refusals: RefusalLog::default(),
            }),
        }
    }
}

/// Why a request was refused before reaching a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessError {
    /// The client was revoked (or never existed).
    Revoked,
    RateLimited {
        retry_after_secs: u64,
    },
}

impl McpServer {
    pub fn builder(backend: Arc<dyn KnowledgeBackend>) -> McpServerBuilder {
        McpServerBuilder {
            backend,
            registry: None,
            audit: None,
            egress: None,
            config: McpConfig::default(),
        }
    }

    pub fn registry(&self) -> Option<&Arc<ClientRegistry>> {
        self.inner.registry.as_ref()
    }

    pub(crate) fn limiter(&self) -> &RateLimiter {
        &self.inner.limiter
    }

    /// An rmcp handler bound to one already-authenticated client (stdio and other streams).
    pub fn handler_for(&self, client: McpClientInfo, transport: Transport) -> McpHandler {
        McpHandler {
            server: self.clone(),
            binding: Binding::Fixed { client, transport },
        }
    }

    /// An rmcp handler that reads the client from each HTTP request (set by [`crate::http`]).
    pub(crate) fn http_handler(&self) -> McpHandler {
        McpHandler {
            server: self.clone(),
            binding: Binding::Http,
        }
    }

    /// The current state of a fixed client: re-read from the registry when there is one, so a
    /// revoked client stops working and scope changes apply immediately.
    fn refresh(&self, client: &McpClientInfo) -> Result<McpClientInfo, AccessError> {
        match &self.inner.registry {
            None => Ok(client.clone()),
            Some(reg) => match reg.get(&client.id) {
                Ok(Some(c)) => Ok(reg.touch(c).unwrap_or_else(|_| client.clone())),
                Ok(None) => Err(AccessError::Revoked),
                Err(e) => {
                    tracing::warn!(error = %e, "mcp client lookup failed");
                    Err(AccessError::Revoked)
                }
            },
        }
    }

    fn rate_check(&self, client: &McpClientInfo) -> Result<(), AccessError> {
        self.inner
            .limiter
            .check(&client.id)
            .map_err(|d| AccessError::RateLimited {
                retry_after_secs: d.as_secs().max(1),
            })
    }

    fn record(&self, rec: CallRecord<'_>) {
        if let Some(sink) = &self.inner.audit {
            sink(rec.to_entry(self.inner.audit_key.as_ref()));
        }
        if rec.transport.is_remote()
            && rec.outcome == Outcome::Ok
            && let Some(egress) = &self.inner.egress
        {
            // Unreachable in M2 (no remote transport); kept so the rule lives next to the audit.
            egress.record(nugget_core::egress::EgressEvent {
                at: time::OffsetDateTime::now_utc(),
                host: "mcp-client".into(),
                feature: "mcp".into(),
                bytes_sent: 0,
                bytes_received: 0,
                items: rec.items.to_vec(),
            });
        }
    }

    /// Audit a request refused before it ran (rate limited or revoked), coalesced per client and
    /// outcome (see [`REFUSAL_AUDIT_WINDOW`]). `action` and `target` name the refused call; HTTP
    /// refusals at the gate, before the method is known, use [`crate::audit::ACTION_REFUSED`] and `"http"`.
    pub(crate) fn record_refusal(
        &self,
        client: &McpClientInfo,
        transport: Transport,
        action: &'static str,
        target: &str,
        outcome: Outcome,
    ) {
        let Some(sink) = &self.inner.audit else {
            return;
        };
        let Some(coalesced) = self.inner.refusals.admit(&client.id, outcome) else {
            return;
        };
        let mut entry = CallRecord {
            client_id: &client.id,
            client_name: &client.name,
            transport,
            action,
            target,
            args: None,
            outcome,
            results: 0,
            items: &[],
        }
        .to_entry(None);
        if let serde_json::Value::Object(d) = &mut entry.detail {
            d.insert("refused_since_last_entry".into(), coalesced.into());
            d.insert(
                "coalesce_window_secs".into(),
                REFUSAL_AUDIT_WINDOW.as_secs().into(),
            );
        }
        sink(entry);
    }

    /// Run one tool for a client: scope check, execution, audit. Transport-independent.
    pub async fn call_tool(
        &self,
        client: &McpClientInfo,
        transport: Transport,
        name: &str,
        args: &JsonObject,
    ) -> Result<ToolOutput, ToolFailure> {
        self.call_tool_until(client, transport, name, args, &CancellationToken::new())
            .await
    }

    /// [`Self::call_tool`] that stops working when `cancel` fires (the client sent
    /// `notifications/cancelled`). Exactly one audit entry either way; a cancelled call is
    /// recorded with the outcome `cancelled`.
    pub async fn call_tool_until(
        &self,
        client: &McpClientInfo,
        transport: Transport,
        name: &str,
        args: &JsonObject,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolFailure> {
        let scope = Scope::new(&client.scopes);
        let result = if !tools::ALL_TOOLS.contains(&name) {
            Err(ToolFailure {
                outcome: Outcome::Rejected,
                message: format!("unknown tool: {name}"),
            })
        } else if !scope.allows_tool(name) {
            Err(ToolFailure {
                outcome: Outcome::Denied,
                message: format!("the tool {name} is not enabled for this client"),
            })
        } else {
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(ToolFailure::cancelled()),
                r = tools::run(self.inner.backend.as_ref(), &scope, name, args) => r,
            }
        };
        let (outcome, results, items): (Outcome, usize, &[ItemRef]) = match &result {
            Ok(o) => (Outcome::Ok, o.results, &o.items),
            Err(f) => (f.outcome, 0, &[]),
        };
        self.record(CallRecord {
            client_id: &client.id,
            client_name: &client.name,
            transport,
            action: ACTION_TOOL,
            target: name,
            args: Some(args),
            outcome,
            results,
            items,
        });
        result
    }

    /// Read a `nugget://` resource for a client. Returns `(mime type, text)`.
    pub async fn read_resource(
        &self,
        client: &McpClientInfo,
        transport: Transport,
        uri: &str,
    ) -> Result<(String, String), ToolFailure> {
        self.read_resource_until(client, transport, uri, &CancellationToken::new())
            .await
    }

    /// [`Self::read_resource`] that stops working when `cancel` fires; audited once either way.
    pub async fn read_resource_until(
        &self,
        client: &McpClientInfo,
        transport: Transport,
        uri: &str,
        cancel: &CancellationToken,
    ) -> Result<(String, String), ToolFailure> {
        let scope = Scope::new(&client.scopes);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolFailure::cancelled()),
            r = self.read_resource_inner(&scope, uri) => r,
        };
        let (outcome, items) = match &result {
            Ok((_, _, items)) => (Outcome::Ok, items.clone()),
            Err(f) => (f.outcome, Vec::new()),
        };
        self.record(CallRecord {
            client_id: &client.id,
            client_name: &client.name,
            transport,
            action: ACTION_RESOURCE,
            target: uri,
            args: None,
            outcome,
            results: usize::from(outcome == Outcome::Ok),
            items: &items,
        });
        result.map(|(mime, text, _)| (mime, text))
    }

    async fn read_resource_inner(
        &self,
        scope: &Scope,
        uri: &str,
    ) -> Result<(String, String, Vec<ItemRef>), ToolFailure> {
        let denied = |tool: &str| ToolFailure {
            outcome: Outcome::Denied,
            message: format!(
                "reading this resource needs the {tool} tool, which is not enabled for this client"
            ),
        };
        let backend = self.inner.backend.as_ref();
        match render::parse_uri(uri) {
            Some(ResourceUri::Entity(id)) => {
                if !scope.allows_tool(tools::GET_ENTITY) {
                    return Err(denied(tools::GET_ENTITY));
                }
                let se = tools::load_entity(backend, scope, &id).await?;
                let (page, items) = tools::entity_markdown(&se);
                Ok((
                    "text/markdown".into(),
                    format!("<!-- {RESULT_PREAMBLE} -->\n{page}\n"),
                    items,
                ))
            }
            Some(ResourceUri::Item { source, key }) => {
                if !scope.allows_tool(tools::GET_ITEM) {
                    return Err(denied(tools::GET_ITEM));
                }
                let out =
                    tools::load_item(backend, scope, &source, &key, 0, tools::DEFAULT_ITEM_CHARS)
                        .await?;
                Ok(("text/plain".into(), out.text, out.items))
            }
            None => Err(ToolFailure {
                outcome: Outcome::Rejected,
                message: format!(
                    "unknown resource URI; expected {}<id> or {}<source>/<key>",
                    render::ENTITY_URI_PREFIX,
                    render::ITEM_URI_PREFIX
                ),
            }),
        }
    }

    /// Entity resources for `resources/list`, starting at `offset`.
    async fn list_entity_resources(
        &self,
        scope: &Scope,
        offset: usize,
    ) -> Result<(Vec<Resource>, bool), ToolFailure> {
        if !scope.allows_tool(tools::GET_ENTITY) {
            return Ok((Vec::new(), false));
        }
        let list = self
            .inner
            .backend
            .find_entities(None, None, RESOURCE_PAGE, offset, scope.filter())
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "mcp resource listing failed");
                ToolFailure {
                    outcome: Outcome::Error,
                    message: "Nugget could not list resources; try again later.".into(),
                }
            })?;
        let more = list.len() == RESOURCE_PAGE;
        let resources = scope
            .entities(list)
            .into_iter()
            .map(|e| {
                Resource::new(
                    render::entity_uri(e.id.as_str()),
                    render::inline(&e.name, 200),
                )
                .with_description(format!(
                    "Knowledge-graph page for a {} (facts with citations).",
                    e.kind
                ))
                .with_mime_type("text/markdown")
            })
            .collect();
        Ok((resources, more))
    }
}

#[derive(Clone)]
enum Binding {
    Fixed {
        client: McpClientInfo,
        transport: Transport,
    },
    Http,
}

/// The `rmcp` [`ServerHandler`]. Obtain one with [`McpServer::handler_for`].
#[derive(Clone)]
pub struct McpHandler {
    server: McpServer,
    binding: Binding,
}

/// Request extension set by the HTTP layer after authenticating the bearer token.
#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedClient(pub McpClientInfo);

impl McpHandler {
    /// The calling client and transport, after revocation and rate checks. `audited` names the
    /// call (action and target) when it is one the audit log records: a refused tool call or
    /// resource read is then audited too (coalesced, see [`REFUSAL_AUDIT_WINDOW`]).
    fn client(
        &self,
        ctx: &RequestContext<RoleServer>,
        count: bool,
        audited: Option<(&'static str, &str)>,
    ) -> Result<(McpClientInfo, Transport), ErrorData> {
        match &self.binding {
            Binding::Fixed { client, transport } => {
                let refused = |e: AccessError| {
                    if let Some((action, target)) = audited {
                        let outcome = match e {
                            AccessError::Revoked => Outcome::Denied,
                            AccessError::RateLimited { .. } => Outcome::RateLimited,
                        };
                        self.server
                            .record_refusal(client, *transport, action, target, outcome);
                    }
                    access_error(e)
                };
                let c = self.server.refresh(client).map_err(refused)?;
                if count {
                    self.server.rate_check(&c).map_err(refused)?;
                }
                Ok((c, *transport))
            }
            // The HTTP layer authenticated and rate-limited this request.
            Binding::Http => ctx
                .extensions
                .get::<http::request::Parts>()
                .and_then(|p| p.extensions.get::<AuthenticatedClient>())
                .map(|a| (a.0.clone(), Transport::LoopbackHttp))
                .ok_or_else(|| ErrorData::invalid_request("unauthenticated", None)),
        }
    }
}

fn access_error(e: AccessError) -> ErrorData {
    match e {
        AccessError::Revoked => {
            ErrorData::invalid_request("this MCP client has been revoked in Nugget", None)
        }
        AccessError::RateLimited { retry_after_secs } => ErrorData::invalid_request(
            format!("rate limited; retry after {retry_after_secs} s"),
            None,
        ),
    }
}

/// Answer to a request whose work was stopped: the client sent `notifications/cancelled`, or
/// the session ended (end of input, disconnect, shutdown). Rarely seen by anyone: rmcp 3.8.0
/// drops the response to a request it cancelled, and an ended session has no one to read it.
fn cancelled_error() -> ErrorData {
    ErrorData::invalid_request("request cancelled", None)
}

/// A `resources/list` cursor: the decimal offset of the next page, as this server hands out in
/// `nextCursor`. Clients treat it as opaque; anything else is refused rather than silently
/// restarting the listing (which would loop a client forever).
fn parse_cursor(cursor: &str) -> Option<usize> {
    let canonical = !cursor.is_empty()
        && cursor.bytes().all(|b| b.is_ascii_digit())
        && (cursor == "0" || !cursor.starts_with('0'));
    if !canonical {
        return None;
    }
    cursor
        .parse::<usize>()
        .ok()
        .filter(|offset| offset % RESOURCE_PAGE == 0)
}

fn instructions() -> String {
    format!(
        "Nugget is the user's local, private knowledge base: their files, mail, calendar and \
         notes, plus a knowledge graph of the people, organizations, projects and commitments in \
         them. Use get_context to gather cited passages for a question, search to find items, \
         find_entities/get_entity for facts about people and things (every fact cites its \
         sources), and get_item to read a whole item. Cite sources when you answer. {SAFETY_NOTE}"
    )
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")).with_title("Nugget"),
        )
        .with_instructions(instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let (client, _) = self.client(&ctx, false, None)?;
        let scope = Scope::new(&client.scopes);
        Ok(ListToolsResult::with_all_items(
            tools::definitions()
                .into_iter()
                .filter(|t| scope.allows_tool(&t.name))
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let (client, transport) = self.client(&ctx, true, Some((ACTION_TOOL, &request.name)))?;
        let args = request.arguments.unwrap_or_default();
        // Unknown tools are audited too (outcome `rejected`), then answered with the protocol
        // error the spec asks for (-32602) instead of a tool result.
        let result = match self
            .server
            .call_tool_until(&client, transport, &request.name, &args, &ctx.ct)
            .await
        {
            Ok(out) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(out.text)]);
                r.structured_content = Some(out.structured);
                r
            }
            Err(f) if f.outcome == Outcome::Cancelled => return Err(cancelled_error()),
            Err(f) if !tools::ALL_TOOLS.contains(&request.name.as_ref()) => {
                return Err(ErrorData::invalid_params(f.message, None));
            }
            Err(f) => CallToolResult::error(vec![ContentBlock::text(f.message)]),
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let (client, _) = self.client(&ctx, true, None)?;
        let scope = Scope::new(&client.scopes);
        let offset = match request.and_then(|r| r.cursor) {
            None => 0,
            Some(c) => parse_cursor(&c).ok_or_else(|| {
                ErrorData::invalid_params("invalid cursor; start again without one", None)
            })?,
        };
        let (resources, more) = self
            .server
            .list_entity_resources(&scope, offset)
            .await
            .map_err(|f| ErrorData::internal_error(f.message, None))?;
        let mut result = ListResourcesResult::with_all_items(resources);
        if more {
            result.next_cursor = Some((offset + RESOURCE_PAGE).to_string());
        }
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let (client, _) = self.client(&ctx, false, None)?;
        let scope = Scope::new(&client.scopes);
        let mut templates = Vec::new();
        if scope.allows_tool(tools::GET_ENTITY) {
            templates.push(
                ResourceTemplate::new(
                    format!("{}{{id}}", render::ENTITY_URI_PREFIX),
                    "Knowledge-graph entity",
                )
                .with_description(
                    "An entity's page as Markdown: facts with numbered citations to sources.",
                )
                .with_mime_type("text/markdown"),
            );
        }
        if scope.allows_tool(tools::GET_ITEM) {
            templates.push(
                ResourceTemplate::new(
                    format!("{}{{source}}/{{key}}", render::ITEM_URI_PREFIX),
                    "Source item",
                )
                .with_description(
                    "A source item's text (first 20,000 characters); source and key are \
                     percent-encoded.",
                )
                .with_mime_type("text/plain"),
            );
        }
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (client, transport) = self.client(&ctx, true, Some((ACTION_RESOURCE, &request.uri)))?;
        match self
            .server
            .read_resource_until(&client, transport, &request.uri, &ctx.ct)
            .await
        {
            Ok((mime, text)) => {
                let contents = ResourceContents::TextResourceContents {
                    uri: request.uri,
                    mime_type: Some(mime),
                    text,
                    meta: None,
                };
                Ok(ReadResourceResult::new(vec![contents]).into())
            }
            Err(f) if f.outcome == Outcome::Error => {
                Err(ErrorData::internal_error(f.message, None))
            }
            Err(f) if f.outcome == Outcome::Cancelled => Err(cancelled_error()),
            Err(f) => Err(ErrorData::resource_not_found(f.message, None)),
        }
    }
}
