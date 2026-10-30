//! App ⇄ `nuggetd` protocol: JSON-RPC 2.0, one JSON object per line, over a local socket
//! (Unix domain socket on macOS/Linux, named pipe on Windows). Server → client notifications
//! carry streaming output. Method names are the `METHOD` constants below.

use serde::{Deserialize, Serialize};

use crate::capability::{Capabilities, Locality, Tier};
use crate::egress::EgressEvent;
use crate::ids::{EntityId, ItemRef, ProviderId, SourceId};

pub const PROTOCOL_VERSION: u32 = 1;

/// Socket name; `nugget-ipc` maps it to a platform path/pipe name.
pub const SOCKET_NAME: &str = "nuggetd.sock";

/// Request → response types, by method:
///
/// | method | params | result |
/// |---|---|---|
/// | `status` | `{}` | [`StatusResponse`] |
/// | `chat` | [`ChatRequest`] | [`ChatStarted`], then `chat.delta`/`chat.done` notifications |
/// | `chat.cancel` | [`ChatCancel`] | `null` |
/// | `search` | [`SearchRequest`] | `Vec<`[`SearchHit`]`>` |
/// | `providers.list` | `{}` | `Vec<`[`ProviderInfo`]`>` |
/// | `providers.probe` | [`ProbeRequest`] | [`Capabilities`] |
/// | `models.list` | `{}` | `Vec<`[`ModelInfo`]`>` |
/// | `models.install` | [`ModelInstallRequest`] | `null`, then `models.progress` notifications |
/// | `egress.recent` | [`EgressRecentRequest`] | [`EgressRecentResponse`] |
/// | `sources.list` | `{}` | `Vec<`[`SourceInfo`]`>` |
/// | `sources.add` | [`SourceSpec`] | [`SourceInfo`] |
/// | `sources.update` | [`SourceUpdate`] | [`SourceInfo`] |
/// | `sources.remove` | [`SourceRemove`] | `null` |
/// | `sources.sync` | [`SourceSync`] | `null` (progress via `ingest.progress`) |
/// | `sources.auth.begin` | [`SourceRef`] | [`AuthBegin`] (completion via `sources.changed`) |
/// | `sources.secret.set` | [`SourceSecret`] | `null` |
/// | `ingest.status` | `{}` | [`IngestStatus`] |
/// | `retrieve.context` | [`ContextRequest`] | [`ContextResponse`] |
/// | `retrieve.ask` | [`AskRequest`] | [`AskStarted`], then `chat.delta`/`chat.done` notifications |
/// | `runtime.status` | `{}` | `Vec<`[`RuntimeInfo`]`>` |
/// | `knowledge.entities` | [`EntitiesRequest`] | `Vec<`[`EntitySummaryView`]`>` |
/// | `knowledge.entity` | [`EntityRef`] | [`EntityDetail`] |
/// | `knowledge.graph` | [`GraphRequest`] | [`GraphViewIpc`] |
/// | `knowledge.merge` | [`MergeRequest`] | `null` |
/// | `knowledge.claim.status` | [`ClaimStatusRequest`] | `null` |
/// | `knowledge.split` | [`SplitRequest`] | [`EntityRef`] (the new entity) |
/// | `knowledge.status` | `{}` | [`KnowledgeStatus`] |
/// | `mcp.clients.list` | `{}` | `Vec<`[`McpClientInfo`]`>` |
/// | `mcp.clients.create` | [`McpClientCreate`] | [`McpClientCreated`] (token shown once) |
/// | `mcp.clients.revoke` | [`McpClientRef`] | `null` |
/// | `draft.begin` | [`DraftRequest`] | [`DraftStarted`], then `chat.delta`/`chat.done` notifications |
/// | `draft.cancel` | [`ChatCancel`] | `null` |
/// | `draft.feedback` | [`DraftFeedback`] | `null` |
/// | `voice.status` | `{}` | [`VoiceStatus`] |
pub mod method {
    pub const STATUS: &str = "status";
    pub const CHAT: &str = "chat";
    pub const CHAT_CANCEL: &str = "chat.cancel";
    pub const SEARCH: &str = "search";
    pub const PROVIDERS_LIST: &str = "providers.list";
    pub const PROVIDERS_PROBE: &str = "providers.probe";
    pub const MODELS_LIST: &str = "models.list";
    pub const MODELS_INSTALL: &str = "models.install";
    pub const EGRESS_RECENT: &str = "egress.recent";
    pub const SOURCES_LIST: &str = "sources.list";
    pub const SOURCES_ADD: &str = "sources.add";
    pub const SOURCES_UPDATE: &str = "sources.update";
    pub const SOURCES_REMOVE: &str = "sources.remove";
    pub const SOURCES_SYNC: &str = "sources.sync";
    pub const SOURCES_AUTH_BEGIN: &str = "sources.auth.begin";
    pub const SOURCES_SECRET_SET: &str = "sources.secret.set";
    pub const INGEST_STATUS: &str = "ingest.status";
    pub const RETRIEVE_CONTEXT: &str = "retrieve.context";
    pub const RETRIEVE_ASK: &str = "retrieve.ask";
    pub const RUNTIME_STATUS: &str = "runtime.status";
    pub const KNOWLEDGE_ENTITIES: &str = "knowledge.entities";
    pub const KNOWLEDGE_ENTITY: &str = "knowledge.entity";
    pub const KNOWLEDGE_GRAPH: &str = "knowledge.graph";
    pub const KNOWLEDGE_MERGE: &str = "knowledge.merge";
    pub const KNOWLEDGE_CLAIM_STATUS: &str = "knowledge.claim.status";
    pub const KNOWLEDGE_SPLIT: &str = "knowledge.split";
    pub const KNOWLEDGE_STATUS: &str = "knowledge.status";
    pub const MCP_CLIENTS_LIST: &str = "mcp.clients.list";
    pub const MCP_CLIENTS_CREATE: &str = "mcp.clients.create";
    pub const MCP_CLIENTS_REVOKE: &str = "mcp.clients.revoke";
    pub const DRAFT_BEGIN: &str = "draft.begin";
    pub const DRAFT_CANCEL: &str = "draft.cancel";
    pub const DRAFT_FEEDBACK: &str = "draft.feedback";
    /// Sent when the trigger fires, before `draft.begin`: warm the provider, retrieval and voice.
    pub const DRAFT_PREWARM: &str = "draft.prewarm";
    /// Sent while a reply field keeps focus (Double Tap read-ahead, local providers only):
    /// prefill the coming draft's whole prompt into the local server's cache, generating
    /// nothing. Params: `DraftRequest`; result `{ "started": bool, "skipped": reason? }`.
    pub const DRAFT_READ_AHEAD: &str = "draft.read_ahead";
    /// Cancel a read-ahead in progress (the field lost focus). Result `{ "cancelled": bool }`.
    pub const DRAFT_READ_AHEAD_CANCEL: &str = "draft.read_ahead_cancel";
    pub const VOICE_STATUS: &str = "voice.status";
}

pub mod notification {
    pub const CHAT_DELTA: &str = "chat.delta";
    pub const CHAT_DONE: &str = "chat.done";
    pub const MODELS_PROGRESS: &str = "models.progress";
    pub const INGEST_PROGRESS: &str = "ingest.progress";
    pub const SOURCES_CHANGED: &str = "sources.changed";
    /// [`KnowledgeChanged`].
    pub const KNOWLEDGE_CHANGED: &str = "knowledge.changed";
}

// ---- status ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub protocol: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub items: u64,
    pub chunks: u64,
    pub providers: Vec<ProviderInfo>,
    /// Result of verifying the audit hash chain at startup. `None` if not checked.
    #[serde(default)]
    pub audit_chain_ok: Option<bool>,
}

// ---- providers ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: ProviderId,
    /// `anthropic`, `openai_responses`, `openai_compatible`.
    pub kind: String,
    pub model: String,
    pub locality: Locality,
    /// Tiers this provider is assigned to in config.
    pub tiers: Vec<Tier>,
    /// `None` until a probe has run.
    pub capabilities: Option<Capabilities>,
    /// Why this provider is unavailable (e.g. it failed to build from config), if it is.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub provider: ProviderId,
}

// ---- chat ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Explicit provider, or route by `tier`.
    pub provider: Option<ProviderId>,
    pub tier: Tier,
    pub messages: Vec<ChatMessage>,
    /// Items whose content the messages include (for example passages pasted from
    /// `retrieve.context`). Recorded in the egress ledger when the request leaves the device.
    #[serde(default)]
    pub items: Vec<ItemRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStarted {
    pub stream_id: String,
    pub provider: ProviderId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDelta {
    pub stream_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDone {
    pub stream_id: String,
    /// `None` on success.
    pub error: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCancel {
    pub stream_id: String,
}

// ---- search ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub item: ItemRef,
    /// Store chunk id of the matching passage, when known.
    #[serde(default)]
    pub chunk_id: Option<i64>,
    pub title: Option<String>,
    pub snippet: String,
    pub score: f32,
    /// Where to open the original (file path URL, web link), if known.
    #[serde(default)]
    pub url: Option<String>,
}

// ---- models ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub size_bytes: u64,
    pub installed: bool,
    pub recommended: bool,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub tiers: Vec<Tier>,
    #[serde(default)]
    pub license: String,
    /// Pinned upstream revision of the weights.
    #[serde(default)]
    pub revision: String,
    /// An installed copy differs from the manifest entry.
    #[serde(default)]
    pub update_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInstallRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub phase: ModelPhase,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPhase {
    Downloading,
    Verifying,
    Installed,
    Failed,
}

// ---- egress ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressRecentRequest {
    pub limit: u32,
}

pub type EgressRecentResponse = Vec<EgressEvent>;

// ---- sources ----

/// Connector kinds shipped in M1: `files`, `obsidian`, `gmail`, `gcal`, `imap`, `caldav`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Chosen by the daemon when `None` (e.g. `files-1`).
    #[serde(default)]
    pub id: Option<SourceId>,
    pub kind: String,
    /// Human-readable label.
    pub name: String,
    /// Kind-specific settings (roots, account, folders, ...). Never contains secrets.
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Up to date, waiting for the next poll.
    Idle,
    /// Running a sync pass.
    Syncing,
    /// Up to date and receiving live change notifications.
    Watching,
    /// Needs the user to (re)authorize or set a password.
    NeedsAuth,
    /// Disabled by the user.
    Disabled,
    /// Last pass failed; see `last_error`. Retried with backoff.
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub state: SourceState,
    pub items: u64,
    pub chunks: u64,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_sync: Option<time::OffsetDateTime>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Same shape as [`SourceSpec::settings`]; never contains secrets.
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub id: SourceId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceUpdate {
    pub id: SourceId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRemove {
    pub id: SourceId,
    /// Also delete everything indexed from this source (default true).
    #[serde(default = "default_true")]
    pub purge: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSync {
    /// `None` syncs every enabled source.
    #[serde(default)]
    pub id: Option<SourceId>,
}

/// Start of an interactive authorization: the client opens `url` in the user's browser; the
/// daemon listens on a loopback redirect (or polls, for a device flow) and emits
/// `sources.changed` when done. For an RFC 8628 device flow `user_code` is set and the client
/// shows it so the user can type it on the verification page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthBegin {
    pub url: String,
    /// The code the user enters on the verification page (device flows only); absent for
    /// redirect flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
}

/// Store a password or app password for a source in the OS keychain. Write-only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSecret {
    pub id: SourceId,
    pub secret: String,
}

impl std::fmt::Debug for SourceSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSecret")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

// ---- ingest ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestPhase {
    Scanning,
    Indexing,
    Idle,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestProgress {
    pub source: SourceId,
    pub phase: IngestPhase,
    pub done: u64,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestStatus {
    /// Changes observed but not yet searchable.
    pub queue_depth: u64,
    /// Change-to-searchable latency over the recent window, in milliseconds.
    #[serde(default)]
    pub freshness_p50_ms: Option<u64>,
    #[serde(default)]
    pub freshness_p95_ms: Option<u64>,
    /// Provider used for embeddings, if any (otherwise text-only search).
    #[serde(default)]
    pub embedding_provider: Option<ProviderId>,
    /// Provider used for triage, if any.
    #[serde(default)]
    pub triage_provider: Option<ProviderId>,
    pub sources: Vec<SourceInfo>,
}

// ---- retrieval ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequest {
    pub query: String,
    /// Approximate token budget for `text`.
    pub budget_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    /// The `[n]` marker used in [`ContextResponse::text`], from 1.
    pub n: u32,
    pub item: ItemRef,
    pub chunk_id: i64,
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// The item is marked sensitive (PII findings or triage): it never goes to cloud models.
    #[serde(default)]
    pub sensitive: bool,
    /// The item's date (`YYYY-MM-DD`): its source modification time, else its last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextResponse {
    /// Packed passages, each prefixed with its `[n]` marker.
    pub text: String,
    pub citations: Vec<Citation>,
    /// Relevant sensitive items left out of `text` (when sensitive items were excluded).
    #[serde(default)]
    pub excluded_sensitive: u32,
}

/// Answer a question from retrieved context (`retrieve.ask`). The daemon picks the provider
/// first, then packs the context for it: sensitive items are left out when the provider is a
/// `Cloud` one, and the cited items are recorded in the egress ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    pub query: String,
    /// Approximate token budget for the packed context.
    pub budget_tokens: u32,
    /// Explicit provider (used alone, never substituted), or route by `tier`.
    #[serde(default)]
    pub provider: Option<ProviderId>,
    #[serde(default = "default_ask_tier")]
    pub tier: Tier,
    /// Earlier turns of the conversation, oldest first, for a follow-up question: the query
    /// is rewritten into a standalone one for retrieval (by a local model only) and the turns
    /// are forwarded to the answering model. Untrusted text: `System` entries are ignored.
    #[serde(default)]
    pub history: Vec<ChatMessage>,
}

fn default_ask_tier() -> Tier {
    Tier::Synthesis
}

/// Result of `retrieve.ask`: the stream (as for `chat`) plus the context it was given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskStarted {
    pub stream_id: String,
    pub provider: ProviderId,
    pub locality: Locality,
    /// The passages sent to the model, numbered as the answer cites them.
    pub citations: Vec<Citation>,
    /// Relevant sensitive items withheld because the provider is a cloud model.
    #[serde(default)]
    pub excluded_sensitive: u32,
    /// The standalone query actually searched, when the follow-up was rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_rewrite: Option<String>,
    /// Nothing relevant was found in the sources, so the model answers as an ordinary chat
    /// (no passages, no citations). Clients should say so on the reply.
    #[serde(default)]
    pub general: bool,
}

// ---- local runtime ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Chat,
    Embedding,
    /// A cross-encoder reranker (`llama-server --reranking`), used by retrieval only: never
    /// routed for a model tier.
    Rerank,
    /// The small model local Double Tap drafts with, opt-in via `[draft.local] model`: its own
    /// server, so drafting never waits behind (or evicts the prompt cache of) Chat. Never routed
    /// for a model tier; only the draft feature calls it, by provider id (`local-draft`).
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    /// Runtime binary or model not installed.
    NotInstalled,
    Stopped,
    Starting,
    Ready,
    /// Asked to stop: no longer accepting requests, the server process is shutting down.
    /// Becomes `Stopped` once it exited.
    Stopping,
    /// Crashed or failed health checks; restarting with backoff.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub model_id: String,
    pub role: RuntimeRole,
    pub state: RuntimeState,
    /// Provider id the router knows this runtime by (e.g. `local-chat`).
    pub provider: ProviderId,
    #[serde(default)]
    pub error: Option<String>,
}

// ---- knowledge graph ----
//
// Entity kinds travel as snake_case strings: `person`, `organization`, `project`, `commitment`,
// `preference`, `event`, `place`, `topic`. Claim statuses: `active`, `superseded`, `retracted`
// (lost its evidence), `user_asserted` (written by the user), `rejected` (rejected by the user;
// sticky: re-extracting its sources never makes it active again). Clients should show unknown
// statuses as not current.

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntitiesRequest {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntitySummaryView {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub claims: u64,
    pub mentions: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub id: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceView {
    pub item: ItemRef,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// When the source item was last modified (its creation in the index if unknown).
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub observed_at: Option<time::OffsetDateTime>,
}

/// Exactly one of `entity` / `literal` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimObjectView {
    #[serde(default)]
    pub entity: Option<EntityLink>,
    #[serde(default)]
    pub literal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityLink {
    pub id: EntityId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimView {
    pub id: i64,
    pub subject: EntityLink,
    pub predicate: String,
    pub object: ClaimObjectView,
    pub text: String,
    pub confidence: f32,
    pub status: String,
    pub evidence: Vec<EvidenceView>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub updated_at: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionView {
    pub item: ItemRef,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    pub surface: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDetail {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// `(scheme, value)` pairs, e.g. `("email", "sam@example.com")`.
    pub identifiers: Vec<(String, String)>,
    pub attributes: serde_json::Value,
    pub claims: Vec<ClaimView>,
    pub mentions: Vec<MentionView>,
    /// The entity's vault page, relative to the vault root.
    #[serde(default)]
    pub vault_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRequest {
    /// Neighbourhood of this entity, or an overview of the most connected entities when `None`.
    #[serde(default)]
    pub center: Option<EntityId>,
    pub depth: u32,
    pub limit: u32,
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only nodes and edges supported by items of this source: an edge whose claim has evidence
    /// from it, a node with a mention or an active claim backed by it (the centre always stays).
    #[serde(default)]
    pub source: Option<SourceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeView {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeView {
    pub claim: i64,
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphViewIpc {
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRequest {
    pub into: EntityId,
    pub from: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimStatusRequest {
    pub claim: i64,
    /// The user's decision:
    /// - `rejected`: reject the fact. Sticky: re-extracting its sources never revives it.
    ///   `retracted` is accepted as an older name for the same thing.
    /// - `active`: restore a rejected, retracted or superseded fact. A fact with no source
    ///   evidence (one the user wrote) is restored as `user_asserted`.
    /// - `user_asserted`: assert the fact as the user's own (e.g. redo an undone assertion).
    /// - `unasserted`: withdraw the user's assertion of a `user_asserted` fact (undo): it becomes
    ///   `active` when sources support it, else `retracted` (not sticky).
    pub status: String,
}

/// Split facts off an entity that conflates two things into a new entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitRequest {
    pub entity: EntityId,
    /// Claims (with `entity` as subject or object) that move to the new entity.
    pub claims: Vec<i64>,
    /// Name of the new entity.
    pub name: String,
    /// Kind of the new entity; the split entity's kind when `None`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Aliases of `entity` that move to the new entity.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// `(scheme, value)` identifiers of `entity` that move to the new entity.
    #[serde(default)]
    pub identifiers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VaultStatus {
    pub path: Option<String>,
    pub pages: u64,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_render: Option<time::OffsetDateTime>,
    /// Pages whose user edits could not be merged automatically.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Pages failing validation (broken links, missing citations, bad frontmatter).
    #[serde(default)]
    pub invalid: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeStatus {
    pub entities: u64,
    pub claims: u64,
    pub mentions: u64,
    /// Items changed but not yet processed by extraction.
    pub pending_items: u64,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_run: Option<time::OffsetDateTime>,
    /// Provider used for LLM extraction, if any (deterministic extractors always run).
    #[serde(default)]
    pub extractor_provider: Option<ProviderId>,
    pub vault: VaultStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeChanged {
    pub entities: Vec<EntityId>,
}

// ---- MCP clients ----

/// What an MCP client may see and call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpScopes {
    /// Sources whose content the client may read; `None` means all.
    #[serde(default)]
    pub sources: Option<Vec<SourceId>>,
    /// Tool names the client may call; empty means all read-only tools.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Whether items flagged sensitive may be returned (default false).
    #[serde(default)]
    pub include_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientInfo {
    pub id: String,
    pub name: String,
    pub scopes: McpScopes,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_used: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientCreate {
    pub name: String,
    pub scopes: McpScopes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientRef {
    pub id: String,
}

/// Copy-paste setup for one MCP-capable app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSetupSnippet {
    /// `claude-desktop`, `claude-code`, `cursor`, `generic-http`, `generic-stdio`.
    pub app: String,
    /// Config file path hint, if the app uses one.
    #[serde(default)]
    pub config_path: Option<String>,
    pub text: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientCreated {
    pub client: McpClientInfo,
    /// Bearer token; returned only once and never stored in plain text by the daemon.
    pub token: String,
    pub setup: Vec<McpSetupSnippet>,
}

impl std::fmt::Debug for McpClientCreated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClientCreated")
            .field("client", &self.client)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

// ---- Double Tap drafting (M3) ----
//
// The desktop app reads the focused field through the OS accessibility APIs (`nugget-a11y`),
// sends a `DraftContext` to the daemon, streams the draft, inserts it and reports feedback.

/// The application that owns the focused field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// macOS bundle id, Windows executable name or Linux desktop-file id.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub window_title: Option<String>,
    /// Page URL when the app is a browser and it is readable.
    #[serde(default)]
    pub url: Option<String>,
}

/// The focused editable element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldInfo {
    /// Normalised accessibility role: `text_area`, `text_field`, `combo_box`, `document`,
    /// `web_area`, `search_field`, ...
    pub role: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    /// Current text of the field (empty for a new reply).
    #[serde(default)]
    pub value: String,
    /// Selection as character offsets `(start, end)` into `value`.
    #[serde(default)]
    pub selection: Option<(u32, u32)>,
    /// Password or other protected field. Drafting is refused and the value is never read.
    #[serde(default)]
    pub secure: bool,
}

/// One message of the conversation around the field, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    #[serde(default)]
    pub author: Option<String>,
    pub text: String,
    /// Whether the user wrote it, when the app exposes that.
    #[serde(default)]
    pub from_me: Option<bool>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// Everything known about where the user wants a draft.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftContext {
    pub app: AppInfo,
    pub field: FieldInfo,
    #[serde(default)]
    pub thread: Vec<ThreadMessage>,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// PNG screenshot of the focused window, only when accessibility text is unavailable and the
    /// user allowed screenshots for this app.
    #[serde(default)]
    pub screenshot_png_base64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRequest {
    pub context: DraftContext,
    /// Optional user hint, e.g. "decline politely".
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub provider: Option<ProviderId>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// Field kinds, decided by rules from the context (never by the model): `email_body`,
/// `chat_message`, `comment`, `document`, `form_field`, `search`, `unknown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftStarted {
    pub stream_id: String,
    pub provider: ProviderId,
    pub locality: Locality,
    pub field_kind: String,
    /// Sources the draft may draw on (`[n]` markers are not inserted into drafts).
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// Sensitive items left out because the provider is a cloud model.
    #[serde(default)]
    pub excluded_sensitive: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftFeedback {
    pub stream_id: String,
    /// `accepted` (inserted as is), `edited` (inserted then changed), `rejected`.
    pub outcome: String,
    /// Text the user finally sent, when known (improves the voice profile; stored encrypted).
    #[serde(default)]
    pub final_text: Option<String>,
    /// Whether insertion was verified by reading the field back.
    #[serde(default)]
    pub insert_verified: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceStatus {
    /// Messages written by the user that the profile was built from.
    pub samples: u64,
    /// Channels with enough samples for a channel-specific style (e.g. `email`, `chat`).
    pub channels: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_built: Option<time::OffsetDateTime>,
}

// ---- Plugins (M6) ----

/// IPC method names of the plugins feature.
pub mod plugin_method {
    pub const LIST: &str = "plugins.list";
    pub const INSPECT: &str = "plugins.inspect";
    pub const INSTALL: &str = "plugins.install";
    pub const APPROVE: &str = "plugins.approve";
    pub const REMOVE: &str = "plugins.remove";
    pub const ENABLE: &str = "plugins.enable";
}

/// Capabilities a plugin requests or the user approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Hosts reachable over HTTP (`example.com`, `*.example.com`, `http://host:port`, ...).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Names of the plugin's own keychain secrets.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Size of the plugin's key-value store in bytes (`0` = none).
    #[serde(default)]
    pub kv_max_bytes: u64,
}

/// Who signed a plugin, as far as this daemon trusts it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSignature {
    /// A key in `[plugins] trusted_keys`.
    Trusted,
    /// The daemon's local dev key (`[plugins] dev_mode = true`).
    Dev,
    /// Neither: the plugin is listed but not loaded.
    #[default]
    Untrusted,
}

/// One installed plugin.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// `connector` and/or `tool`.
    pub kinds: Vec<String>,
    pub enabled: bool,
    /// Running in the plugin host right now.
    pub loaded: bool,
    /// Why it is not loaded (untrusted signer, load failure, ...).
    #[serde(default)]
    pub error: Option<String>,
    pub signature: PluginSignature,
    /// Hex ed25519 public key that signed the package.
    pub signer: String,
    pub approved: PluginCapabilities,
    /// RFC 3339.
    pub installed_at: String,
    /// `plugin:<id>` for connector plugins.
    #[serde(default)]
    pub connector_kind: Option<String>,
    /// Agent tool names.
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginListResult {
    pub plugins: Vec<PluginInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIdParams {
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInspectResult {
    pub plugin: PluginInfo,
    /// What the installed version's manifest requests.
    pub requested: PluginCapabilities,
    #[serde(default)]
    pub connector_display_name: Option<String>,
}

/// `plugins.install`: an absolute path to a `.nuggetplugin` file or a package directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInstallParams {
    pub path: String,
}

/// The verified package, staged under `token` (10 minutes) until `plugins.approve`. When
/// `needs_approval` is false (an update within the approved capabilities) it was committed at
/// once and `installed` is true.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInstallResult {
    pub token: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub kinds: Vec<String>,
    pub signature: PluginSignature,
    pub signer: String,
    pub requested: PluginCapabilities,
    #[serde(default)]
    pub previously_approved: Option<PluginCapabilities>,
    #[serde(default)]
    pub installed_version: Option<String>,
    pub needs_approval: bool,
    pub installed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginApproveParams {
    pub token: String,
    pub approve: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginApproveResult {
    /// The installed plugin, or `None` when the install was rejected.
    #[serde(default)]
    pub plugin: Option<PluginInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRemoveResult {
    pub removed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEnableParams {
    pub id: String,
    pub enabled: bool,
}

// ---- Agent (M4) ----
//
// The agent runtime (`nugget-agent`), the safety policy (`nugget-policy`) and the browser bridge
// (`nugget-browser`), served by the daemon's `agent`, `policy` and `browser` features. Types
// owned by those crates travel as JSON values (`serde_json::Value`); each field names the type
// whose serialized form it carries.
//
// | Method | Params | Result |
// |---|---|---|
// | `agent.run` | [`AgentRunRequest`] | [`AgentStarted`], then `agent.event` notifications |
// | `agent.cancel` | [`AgentTaskRef`] | `null` |
// | `agent.confirm` | [`AgentConfirm`] | `null` |
// | `agent.answer` | [`AgentAnswer`] | `null` (resumes a run waiting on `ask_user`) |
// | `agent.list` | [`AgentListRequest`] | `Vec<`[`AgentTaskSummary`]`>`, newest first |
// | `agent.get` | [`AgentTaskRef`] | [`AgentTaskDetail`] (with `explain`) |
// | `policy.get` | `{}` | [`PolicyState`] |
// | `policy.set` | [`PolicySet`] | [`PolicyState`] |
// | `policy.approvals` | `{}` | [`PolicyApprovals`] |
// | `policy.kill` | `{}` | [`KillState`] |
// | `policy.resume` | `{}` | [`KillState`] |
// | `policy.log.export` | `{}` | [`PolicyLogExport`] |
// | `browser.status` | `{}` | [`BrowserStatus`] |
// | `browser.host.attach` | `nugget_browser::bridge::AttachInfo` | `null` when detached; `browser.host.request` notifications meanwhile |
// | `browser.host.deliver` | `nugget_browser::ExtensionMessage` | `null` |

pub mod agent_method {
    pub const AGENT_RUN: &str = "agent.run";
    pub const AGENT_CANCEL: &str = "agent.cancel";
    pub const AGENT_CONFIRM: &str = "agent.confirm";
    pub const AGENT_ANSWER: &str = "agent.answer";
    pub const AGENT_LIST: &str = "agent.list";
    pub const AGENT_GET: &str = "agent.get";
    pub const POLICY_GET: &str = "policy.get";
    pub const POLICY_SET: &str = "policy.set";
    pub const POLICY_APPROVALS: &str = "policy.approvals";
    pub const POLICY_KILL: &str = "policy.kill";
    pub const POLICY_RESUME: &str = "policy.resume";
    pub const POLICY_LOG_EXPORT: &str = "policy.log.export";
    pub const BROWSER_STATUS: &str = "browser.status";
    /// Native host → daemon, long-lived: registers the host; returns when it is detached.
    pub const BROWSER_HOST_ATTACH: &str = "browser.host.attach";
    /// Native host → daemon: a reply or event from the extension.
    pub const BROWSER_HOST_DELIVER: &str = "browser.host.deliver";
}

pub mod agent_notification {
    /// [`AgentEventNotice`], broadcast to every connection.
    pub const AGENT_EVENT: &str = "agent.event";
    /// [`KillState`], broadcast when the kill switch is engaged or released.
    pub const POLICY_KILL_CHANGED: &str = "policy.kill.changed";
    /// Daemon → native host: a `nugget_browser::HostMessage` to forward to the extension.
    pub const BROWSER_HOST_REQUEST: &str = "browser.host.request";
}

/// Optional limits for one run; unset fields use the `[agent]` config (or the crate defaults).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBudgets {
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub deadline_secs: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunRequest {
    pub goal: String,
    /// Extra trusted context from the user.
    #[serde(default)]
    pub context: Option<String>,
    /// Tool names the run may use (the built-ins `finish`, `ask_user`, `note` are always
    /// available). `None` means every registered tool.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub budgets: AgentBudgets,
    /// Use this provider instead of the router's agentic tier.
    #[serde(default)]
    pub provider: Option<ProviderId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStarted {
    pub task_id: String,
    /// Tools the run can call.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskRef {
    pub task_id: String,
}

/// The user's answer to a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmChoice {
    /// Approve this action only.
    Once,
    /// Approve this action and the same risk class on the same target for the rest of the task
    /// (only Send and Destroy can be approved for a task; others behave like `once`).
    Task,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfirm {
    pub task_id: String,
    /// `ConfirmationRequest::request_id` from the `confirmation_needed` event.
    pub request_id: String,
    pub choice: ConfirmChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAnswer {
    pub task_id: String,
    pub answer: String,
}

/// One `agent.event` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEventNotice {
    pub task_id: String,
    /// A `nugget_agent::AgentEvent` (tagged by its `event` field).
    pub event: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentListRequest {
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Lifecycle of a task as the daemon tracks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskState {
    Running,
    /// Waiting for the user to answer a confirmation.
    Confirming,
    /// Suspended on `ask_user`; answer with `agent.answer`.
    NeedsInput,
    Finished,
    /// Started before the daemon last restarted and never finished.
    Interrupted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentVerificationCounts {
    pub passed: u32,
    pub failed: u32,
    pub unverifiable: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskSummary {
    pub task_id: String,
    pub goal: String,
    pub state: AgentTaskState,
    /// `nugget_agent::Outcome::label()` once finished (`succeeded`, `unverified`, `failed`, ...).
    #[serde(default)]
    pub outcome: Option<String>,
    /// The outcome's detail or the model's summary.
    #[serde(default)]
    pub detail: Option<String>,
    /// Unix milliseconds.
    pub started_ms: u64,
    #[serde(default)]
    pub finished_ms: Option<u64>,
    pub steps: u32,
    #[serde(default)]
    pub verification: AgentVerificationCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskDetail {
    pub summary: AgentTaskSummary,
    /// The task's `nugget_agent::LogEntry`s, in order.
    pub entries: Vec<serde_json::Value>,
    /// `nugget_agent::explain` over `entries`: a readable report.
    pub explain: String,
    /// Open `nugget_agent::ConfirmationRequest`s.
    #[serde(default)]
    pub pending: Vec<serde_json::Value>,
    /// The `ask_user` question when the state is `needs_input`.
    #[serde(default)]
    pub question: Option<String>,
}

/// Status of the tamper-evident action log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionLogStatus {
    pub entries: u64,
    /// `(seq, hash)` of the last entry.
    #[serde(default)]
    pub head: Option<(u64, String)>,
    pub chain_ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyState {
    /// The user's `nugget_policy::PolicyConfig` (rules, disabled defaults, rate limits, timeout).
    pub config: serde_json::Value,
    /// Every built-in `nugget_policy::Rule`, with whether the user turned it off.
    pub defaults: Vec<DefaultRuleView>,
    pub halted: bool,
    pub log: ActionLogStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefaultRuleView {
    /// A `nugget_policy::Rule`.
    pub rule: serde_json::Value,
    pub enabled: bool,
}

/// Replace the user's policy configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicySet {
    /// A `nugget_policy::PolicyConfig`.
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskApprovalView {
    pub task_id: String,
    /// `nugget_policy::RiskClass` as its snake_case name.
    pub risk: String,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyApprovals {
    /// Open `nugget_agent::ConfirmationRequest`s across all tasks.
    pub pending: Vec<serde_json::Value>,
    /// Standing per-task approvals.
    pub task: Vec<TaskApprovalView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillState {
    pub halted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyLogExport {
    /// The whole hash-chained log, one entry per line.
    pub jsonl: String,
    pub status: ActionLogStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStatus {
    /// A native host (and so the extension) is attached.
    pub connected: bool,
    /// The calling extension's origin.
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub host_version: Option<String>,
    /// Browser tools registered for the agent.
    pub tools: Vec<String>,
}

// ---- Proactive (M5) ----
//
// The daemon refreshes cards after ingest and knowledge runs and on a schedule. The desktop lists
// them (`cards.list`), records feedback (`cards.feedback`), previews an action as a dry-run diff
// (`cards.preview`) and executes it with an approval bound to that preview's digest
// (`cards.execute`), through the policy gate. Wire shapes mirror `nugget-proactive`'s types.
//
// | method | params | result |
// |---|---|---|
// | `cards.list` | [`CardsListRequest`] | [`CardsList`] |
// | `cards.feedback` | [`CardFeedbackRequest`] | [`CardView`] |
// | `cards.preview` | [`CardPreviewRequest`] | [`CardPreview`] |
// | `cards.execute` | [`CardExecuteRequest`] | [`CardExecuteResult`] |
// | `brief.get` | `{}` | [`BriefView`] (today's, composed on demand) |
// | `proactive.metrics` | `{}` | [`ProactiveMetrics`] |
// | `proactive.refresh` | `{}` | [`CardsChanged`] (runs a refresh now) |
// | `reminders.list` | `{}` | `Vec<`[`ReminderView`]`>` (pending, soonest first) |
// | `proactive.learning` | `{}` | [`ProactiveLearning`] (what feedback taught the ranking) |
// | `proactive.reset_learning` | [`ResetLearningRequest`] | [`ResetLearningResult`] (audited) |

pub mod proactive_method {
    pub const CARDS_LIST: &str = "cards.list";
    pub const CARDS_FEEDBACK: &str = "cards.feedback";
    pub const CARDS_PREVIEW: &str = "cards.preview";
    pub const CARDS_EXECUTE: &str = "cards.execute";
    pub const BRIEF_GET: &str = "brief.get";
    pub const PROACTIVE_METRICS: &str = "proactive.metrics";
    pub const PROACTIVE_REFRESH: &str = "proactive.refresh";
    pub const REMINDERS_LIST: &str = "reminders.list";
    pub const PROACTIVE_LEARNING: &str = "proactive.learning";
    pub const PROACTIVE_RESET_LEARNING: &str = "proactive.reset_learning";
}

pub mod proactive_notification {
    /// [`CardsChanged`](super::CardsChanged).
    pub const CARDS_CHANGED: &str = "cards.changed";
    /// [`BriefView`](super::BriefView), once a day at the configured brief time.
    pub const BRIEF_READY: &str = "brief.ready";
    /// [`ReminderView`](super::ReminderView), when a stored reminder is due (the desktop shows
    /// an OS notification).
    pub const REMINDER_DUE: &str = "reminder.due";
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardsListRequest {
    /// Also return snoozed, accepted and closed cards (default: the open inbox only).
    #[serde(default)]
    pub include_closed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardsList {
    /// Open cards best first (then the rest, newest first, when `include_closed`).
    pub cards: Vec<CardView>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_refresh: Option<time::OffsetDateTime>,
    /// `[proactive] enabled`.
    pub enabled: bool,
}

/// One feature's share of a card's score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardFactor {
    /// `urgency`, `importance`, `freshness`, `kind_<kind>`, `bias`, ...
    pub name: String,
    pub value: f64,
    pub weight: f64,
    /// `value × weight` (log-odds).
    pub contribution: f64,
}

/// A source item backing a card. `quote` is untrusted source text: render it as plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardEvidence {
    pub item: ItemRef,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// An action a card offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardActionView {
    /// Position in the card's actions (`cards.preview`/`cards.execute` take it).
    pub index: u32,
    /// `draft_reply`, `create_event`, `reminder`, `archive_thread`, `research_note`.
    pub kind: String,
    /// Short label, e.g. "Draft a reply to Ana".
    pub label: String,
    /// Declared risk: `observe`, `edit`, `send`, `destroy`, `spend`, `security`.
    pub risk: String,
    /// Whether an executor is configured for it (e.g. archiving needs a connector that can).
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardView {
    pub id: String,
    /// `unanswered_thread`, `commitment`, `meeting_prep`, `renewal`, `follow_up`.
    pub kind: String,
    pub title: String,
    pub detail: String,
    /// The judge's one-line summary, when it ran.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub due: Option<time::OffsetDateTime>,
    pub score: f64,
    pub rank: f64,
    /// Top factors of the score (why it is here).
    pub factors: Vec<CardFactor>,
    pub evidence: Vec<CardEvidence>,
    pub actions: Vec<CardActionView>,
    /// `new`, `seen`, `snoozed`, `accepted`, `dismissed`, `done`, `expired`.
    pub state: String,
    pub sensitive: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub snoozed_until: Option<time::OffsetDateTime>,
    /// How the user's own feedback changed this card's ranking, strongest first (empty when it
    /// did not).
    #[serde(default)]
    pub learned: Vec<CardLearned>,
}

/// One way feedback changed a card's ranking, e.g. "shown less often: you dismissed 3 similar
/// cards from this sender". Structured so the app can phrase it in the user's language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardLearned {
    /// `sender`, `domain`, `kind`, `thread`.
    pub scope: String,
    /// The sender address, domain or card kind (`thread`: an opaque thread key).
    pub key: String,
    /// `less` (ranked lower), `more` (ranked higher), `hidden` (suppressed: a thread the user
    /// marked already handled).
    pub direction: String,
    /// Accepts and dismissals behind it (decayed, rounded).
    pub accepted: u32,
    pub dismissed: u32,
    /// The most common dismiss reason behind it, when dismissals dominate.
    #[serde(default)]
    pub reason: Option<String>,
    /// Change in log-odds of the score.
    pub contribution: f64,
}

/// `proactive.learning`: what Nugget learned from the user's feedback.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProactiveLearning {
    /// Strongest adjustment first.
    pub entries: Vec<LearnedView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnedView {
    /// `sender`, `domain`, `kind`, `thread`.
    pub scope: String,
    pub key: String,
    /// A human label (a thread's card title), when the key is opaque. Untrusted source text.
    #[serde(default)]
    pub label: Option<String>,
    /// Decayed counts.
    pub accepted: f64,
    pub dismissed: f64,
    /// Current adjustment in log-odds (negative ranks lower).
    pub adjustment: f64,
    /// The entry suppresses matching cards (a thread marked already handled).
    #[serde(default)]
    pub suppresses: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub last_at: time::OffsetDateTime,
}

/// `proactive.reset_learning`: forget what feedback taught the ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetLearningRequest {
    /// `all`, `sender`, `domain`, `kind`, `thread`.
    pub scope: String,
    /// Required unless `scope` is `all`.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetLearningResult {
    /// Learned entries forgotten.
    pub cleared: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardFeedbackRequest {
    pub card_id: String,
    /// `accept`, `dismiss`, `snooze`, `seen`, `done`.
    pub feedback: String,
    /// For `dismiss`: `not_relevant`, `already_handled`, `wrong_info`, `not_important`, `other`.
    #[serde(default)]
    pub reason: Option<String>,
    /// For `dismiss` with `other`.
    #[serde(default)]
    pub reason_text: Option<String>,
    /// For `snooze` (default: tomorrow at the brief time).
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub until: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPreviewRequest {
    pub card_id: String,
    /// [`CardActionView::index`].
    pub action: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewLine {
    /// `add`, `remove`, `context`.
    pub op: String,
    pub text: String,
}

/// A dry run: what the action would change. Nothing has executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPreview {
    pub card_id: String,
    pub action: u32,
    pub kind: String,
    pub summary: String,
    pub risk: String,
    /// Where the change lands, e.g. `mail:drafts`, `calendar`, `vault`.
    pub target: String,
    pub diff: Vec<PreviewLine>,
    /// Approvals are bound to it: pass it back to `cards.execute`. Valid until the daemon
    /// restarts or about an hour passes.
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardExecuteRequest {
    pub card_id: String,
    pub action: u32,
    /// [`CardPreview::digest`] of the preview the user approved.
    pub digest: String,
    /// The user confirmed the policy's (or the action's) confirmation reason.
    #[serde(default)]
    pub confirmed: bool,
}

/// Something the desktop finishes on the user's behalf after an action ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionArtifact {
    /// `draft` (text to copy, `url` a `mailto:` link to open it in the mail app), `file` (open
    /// `path` with the OS, e.g. an `.ics`), `note` (vault note at `path`), `event` (created on
    /// the calendar server), `reminder` (stored; `reminder.due` fires at `at`), `archived`.
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub at: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardExecuteResult {
    /// `executed`, `needs_confirmation` (ask, then retry with `confirmed`), `denied`, `halted`.
    pub outcome: String,
    /// The executor's result id, or the policy's reason.
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub artifact: Option<ActionArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefCardView {
    pub card_id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub due: Option<time::OffsetDateTime>,
    pub rank: f64,
    pub factors: Vec<CardFactor>,
    /// Offered action kinds, best first.
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefMeetingView {
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub start: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub end: Option<time::OffsetDateTime>,
    pub all_day: bool,
    #[serde(default)]
    pub location: Option<String>,
    pub attendees: Vec<String>,
    /// Attendees outside the user's organisation.
    pub external: Vec<String>,
    #[serde(default)]
    pub prep_card: Option<String>,
    pub prep: Vec<CardEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefView {
    /// The user's local date, `YYYY-MM-DD`.
    pub date: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: time::OffsetDateTime,
    pub top: Vec<BriefCardView>,
    pub meetings: Vec<BriefMeetingView>,
    pub overdue: Vec<BriefCardView>,
    pub renewals: Vec<BriefCardView>,
    pub open_cards: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KindMetricsView {
    pub shown: u64,
    pub accepted: u64,
    pub dismissed: u64,
    #[serde(default)]
    pub precision: Option<f64>,
}

/// Card quality: precision = accepted / shown, time to action, dismiss reasons.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProactiveMetrics {
    pub total: u64,
    pub by_state: std::collections::BTreeMap<String, u64>,
    pub shown: u64,
    pub accepted: u64,
    pub dismissed: u64,
    pub snoozed: u64,
    #[serde(default)]
    pub precision: Option<f64>,
    pub by_kind: std::collections::BTreeMap<String, KindMetricsView>,
    #[serde(default)]
    pub median_time_to_action_secs: Option<f64>,
    #[serde(default)]
    pub mean_time_to_action_secs: Option<f64>,
    pub dismiss_reasons: std::collections::BTreeMap<String, u64>,
    pub model_updates: u64,
}

/// `cards.changed`: the card set changed (refresh, feedback or an executed action). Refetch
/// `cards.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardsChanged {
    /// `refresh`, `feedback`, `execute`, `reset_learning`.
    pub reason: String,
    pub open: u32,
    pub created: u32,
    pub resolved: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderView {
    pub id: String,
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    #[serde(default)]
    pub about: Option<ItemRef>,
    /// The card it came from.
    #[serde(default)]
    pub card_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_begin_user_code_is_optional_on_the_wire() {
        let redirect = AuthBegin {
            url: "https://example.com/authorize".into(),
            user_code: None,
        };
        let v = serde_json::to_value(&redirect).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"url": "https://example.com/authorize"})
        );
        assert_eq!(serde_json::from_value::<AuthBegin>(v).unwrap(), redirect);

        let device = AuthBegin {
            url: "https://example.com/device".into(),
            user_code: Some("WDJB-MJHT".into()),
        };
        let v = serde_json::to_value(&device).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"url": "https://example.com/device", "user_code": "WDJB-MJHT"})
        );
        assert_eq!(serde_json::from_value::<AuthBegin>(v).unwrap(), device);
    }
}
