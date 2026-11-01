//! App ⇄ `nuggetd` protocol: JSON-RPC 2.0, one JSON object per line, over a local socket
//! (Unix domain socket on macOS/Linux, named pipe on Windows). Server → client notifications
//! carry streaming output. Method names are the `METHOD` constants below.

use serde::{Deserialize, Serialize};

use crate::capability::{Capabilities, Locality, Tier};
use crate::ids::{EntityId, ItemRef, ProviderId, SourceId};

pub const PROTOCOL_VERSION: u32 = 1;

/// Socket name; `nugget-ipc` maps it to a platform path/pipe name.
pub const SOCKET_NAME: &str = "nuggetd.sock";

/// Request methods. Params and results per method: docs/book/src/reference/ipc.md.
pub mod method {
    pub const STATUS: &str = "status";
    /// Stop the daemon cleanly, as SIGTERM would. Local IPC socket only: no MCP, HTTP or
    /// browser-extension surface forwards it.
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
    pub const CHAT: &str = "chat";
    pub const CHAT_CANCEL: &str = "chat.cancel";
    pub const SEARCH: &str = "search";
    pub const SEARCH_PAGE: &str = "search.page";
    pub const PROVIDERS_LIST: &str = "providers.list";
    pub const PROVIDERS_PROBE: &str = "providers.probe";
    pub const MODELS_LIST: &str = "models.list";
    pub const MODELS_INSTALL: &str = "models.install";
    /// Stop a running `models.install`; staged bytes are kept and the next install resumes.
    pub const MODELS_CANCEL: &str = "models.cancel";
    pub const EGRESS_RECENT: &str = "egress.recent";
    pub const EGRESS_PAGE: &str = "egress.page";
    /// A request the desktop app made itself (update checks), for the daemon's ledger.
    pub const EGRESS_RECORD: &str = "egress.record";
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
    /// Whether a text holds something sensitive (the ingest PII scan), before the desktop
    /// keeps a draft in its history.
    pub const DRAFT_CHECK_SENSITIVE: &str = "draft.check_sensitive";
    /// Sent when the trigger fires, before `draft.begin`: warm the provider, retrieval and voice.
    pub const DRAFT_PREWARM: &str = "draft.prewarm";
    /// Prefill the coming draft's prompt into the local server's cache while a reply field
    /// keeps focus. Params: `DraftRequest`; result `{ "started": bool, "skipped": reason? }`.
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
    pub const KNOWLEDGE_CHANGED: &str = "knowledge.changed";
    pub const ASK_RESTARTED: &str = "ask.restarted";
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
    /// Free space on the data disk; `None` when unknown.
    #[serde(default)]
    pub disk: Option<DiskStatus>,
    /// The daemon's process id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// [`SERVICE_SYSTEMD`] or [`SERVICE_LAUNCHD`] when a service manager runs this daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Features that did not start (a bad config section), with why.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub failed_features: std::collections::BTreeMap<String, String>,
}

pub const SERVICE_SYSTEMD: &str = "systemd";
pub const SERVICE_LAUNCHD: &str = "launchd";

// ---- daemon.shutdown ----

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonShutdownRequest {
    /// Why, for the daemon's log (`app_quit`, `version_mismatch`, `update`); clipped.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownResponse {
    pub stopping: bool,
    pub pid: u32,
}

pub const DISK_FULL_BYTES: u64 = 200 * 1024 * 1024;
pub const DISK_LOW_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskStatus {
    #[serde(default)]
    pub free_bytes: Option<u64>,
    pub level: DiskLevel,
    /// A write failed recently because the disk was full (it may have recovered since).
    #[serde(default)]
    pub recent_full_error: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskLevel {
    #[default]
    Ok,
    Low,
    Full,
    /// A level this build does not know (a newer daemon): show nothing.
    #[serde(other)]
    Unknown,
}

impl DiskStatus {
    /// Unknown free space counts as `Ok` unless a write failed for lack of space.
    pub fn classify(free_bytes: Option<u64>, recent_full_error: bool) -> Self {
        let level = match free_bytes {
            Some(f) if f < DISK_FULL_BYTES => DiskLevel::Full,
            Some(f) if f < DISK_LOW_BYTES && recent_full_error => DiskLevel::Full,
            None if recent_full_error => DiskLevel::Full,
            Some(f) if f < DISK_LOW_BYTES => DiskLevel::Low,
            _ => DiskLevel::Ok,
        };
        Self {
            free_bytes,
            level,
            recent_full_error,
        }
    }
}

// ---- providers ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: ProviderId,
    /// `anthropic`, `openai_responses`, `openai_compatible`.
    pub kind: String,
    pub model: String,
    pub locality: Locality,
    pub tiers: Vec<Tier>,
    pub capabilities: Option<Capabilities>,
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
    /// Items whose content the messages include; recorded in the egress ledger.
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
    /// Post-check of a sourced `retrieve.ask` answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<AnswerCheck>,
}

/// The daemon's post-check of a sourced `retrieve.ask` answer, sent on `chat.done`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerCheck {
    /// The answer says the user's items don't contain what was asked.
    #[serde(default)]
    pub not_found: bool,
    /// Sentences of the answer that state something (not counting a "not found" reply).
    #[serde(default)]
    pub claims: u32,
    /// Claims the client should mark, in answer order.
    #[serde(default)]
    pub flagged: Vec<FlaggedClaim>,
    /// Citation markers that named a passage the model was not given; removed from the text.
    #[serde(default)]
    pub invalid_citations: u32,
}

/// One claim of an answer the post-check could not tie to its sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlaggedClaim {
    /// The sentence exactly as it appears in the streamed answer text.
    pub text: String,
    pub reason: ClaimFlag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimFlag {
    /// States something but cites no passage.
    Uncited,
    /// Cites passages, but a number, amount or date it states is in none of them.
    Unsupported,
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

/// `search.page`: every page ranks to the same depth (500 hits); items indexed between pages can
/// shift the ranking, so clients merge pages by item and chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPageRequest {
    pub query: String,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Result of the cursor-paged methods (`search.page`, `egress.page`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass back as `cursor` for the next page; `None` at the end.
    #[serde(default)]
    pub next_cursor: Option<String>,
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
pub struct ModelCancelRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCancelResponse {
    pub cancelled: bool,
}

/// `models.progress`. In `failed`, `downloaded` counts the staged bytes (kept for resuming).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub phase: ModelPhase,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ModelFailReason>,
    /// The HTTP status when `reason` is `http_error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

/// Why a model install failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFailReason {
    /// No network, or the download server cannot be reached (DNS / connect failure).
    Offline,
    /// The server answered with an error status (see `http_status`).
    HttpError,
    /// The network served a sign-in web page instead of the file.
    CaptivePortal,
    /// No data arrived for several seconds.
    Stalled,
    /// The connection kept closing before the file was complete.
    Interrupted,
    DiskFull,
    /// The downloaded bytes did not match the signed manifest; they were discarded.
    Integrity,
    /// Stopped by `models.cancel` (or the install's client went away).
    Cancelled,
    Other,
    /// A reason this build does not know (newer daemon).
    #[serde(other)]
    Unknown,
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

/// `egress.page`: ledger rows newest first; rows recorded while paging never shift later pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPageRequest {
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// The only ledger features a client may record with `egress.record` (the desktop updater).
pub const CLIENT_EGRESS_FEATURES: &[&str] = &["update_check", "update_download"];

/// `egress.record`: one request a client made off the device (carrying no user content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressRecordRequest {
    /// When the request started; the daemon's clock when absent.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<time::OffsetDateTime>,
    /// Destination host (`host` or `host:port`).
    pub host: String,
    /// One of [`CLIENT_EGRESS_FEATURES`].
    pub feature: String,
    #[serde(default)]
    pub bytes_sent: u64,
    #[serde(default)]
    pub bytes_received: u64,
}

// ---- sources ----

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
    /// Plain-language sync health: what is wrong and how to fix it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<SourceHealth>,
    /// What the last whole-folder scan found and skipped (files sources). Watcher changes do
    /// not refresh it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// The walk of this pass ended (false while it runs).
    #[serde(default)]
    pub finished: bool,
    /// Files that will be (or already are) indexed this pass.
    #[serde(default)]
    pub found: u64,
    /// New, changed or deleted files this pass sends for indexing (its progress total).
    #[serde(default)]
    pub to_index: u64,
    /// Files skipped because their type is not indexed.
    #[serde(default)]
    pub unsupported: u64,
    /// Files skipped because they are over `max_file_bytes`.
    #[serde(default)]
    pub too_large: u64,
    /// Files or subfolders that could not be read.
    #[serde(default)]
    pub unreadable: u64,
    /// Roots that are not there (unmounted or moved).
    #[serde(default)]
    pub roots_missing: u64,
    /// Roots that exist but cannot be read (permission denied).
    #[serde(default)]
    pub roots_denied: u64,
    #[serde(default)]
    pub max_file_bytes: u64,
    /// Up to 5 examples of skipped files, first found first.
    #[serde(default)]
    pub skipped: Vec<SkippedFile>,
}

/// One skipped file in a [`ScanReport`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    /// Relative to its root, `/`-separated.
    #[serde(default)]
    pub path: String,
    /// `unsupported`, `too_large` or `unreadable`.
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// A source's sync health, for people rather than logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHealth {
    /// Stable machine code (`ok`, `syncing`, `disabled`, `reconnect`, `not_set_up`,
    /// `rate_limited`, `offline`, `server_error`, `too_large`, `disk_full`, `config`,
    /// `item_errors`, `nothing_indexable`, `unreadable`, `error`); clients translate it and
    /// fall back to `reason`/`fix`.
    pub code: String,
    /// One sentence on what is going on, in English.
    pub reason: String,
    /// One sentence on what the user can do, in English (empty when nothing is needed).
    #[serde(default)]
    pub fix: String,
    /// Items that failed in recent passes (each one skipped; the rest of the sync went on).
    #[serde(default)]
    pub item_errors: u64,
    /// The most recent item-level failure message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_item_error: Option<String>,
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

/// Start of an interactive authorization: the client opens `url`; completion arrives as
/// `sources.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthBegin {
    pub url: String,
    /// RFC 8628 device flows only: the code the user types on the verification page.
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
    /// Background triage progress (items are searchable before triage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageProgress>,
}

/// What the background triage job is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageState {
    /// Triage is turned off or no local triage provider is routable.
    Off,
    /// Nothing to triage.
    Idle,
    /// Classifying items.
    Running,
    /// Waiting for interactive model requests (Ask, Double Tap, agent) to finish.
    Yielding,
    /// Waiting for power (battery) to allow heavy work.
    Paused,
}

/// Background triage counters since the daemon started (`pending` is the current backlog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageProgress {
    pub state: TriageState,
    /// Items indexed and searchable whose triage has not run yet.
    pub pending: u64,
    /// Items triaged by the model.
    pub done: u64,
    /// Items labelled by rule without a model call (e.g. PII already found).
    #[serde(default)]
    pub skipped: u64,
    /// Items whose triage failed; they count as sensitive and are retried with backoff.
    #[serde(default)]
    pub failed: u64,
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

/// `retrieve.ask`: sensitive items are left out of the context for a `Cloud` provider, and the
/// cited items are recorded in the egress ledger.
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
    /// Earlier turns, oldest first. Untrusted: `System` entries are ignored.
    #[serde(default)]
    pub history: Vec<ChatMessage>,
    /// The client's UTC offset in minutes (clamped to ±18 h), for "today" in the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_minutes: Option<i32>,
}

fn default_ask_tier() -> Tier {
    Tier::Synthesis
}

/// Result of `retrieve.ask`, sent before the provider answers; a later fallback to another
/// provider is announced with [`AskRestarted`].
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
    /// Nothing relevant was found: the model answers as an ordinary chat.
    #[serde(default)]
    pub general: bool,
    /// The date phrase found in the question and how it shaped the search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<AskDateFilter>,
    /// Citation numbers of near-miss passages (missing a rare key term of the question).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<u32>,
}

/// How a date phrase in an Ask question filtered the search (`AskStarted::date`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskDateFilter {
    /// The phrase as written in the question, e.g. `"last week"`.
    pub phrase: String,
    /// Start of the range (inclusive), at the offset the phrase was read with.
    #[serde(with = "time::serde::rfc3339")]
    pub start: time::OffsetDateTime,
    /// End of the range (exclusive).
    #[serde(with = "time::serde::rfc3339")]
    pub end: time::OffsetDateTime,
    /// `false`: nothing in the range matched, so the search ran without the date.
    pub applied: bool,
    /// A generic question: the sources are the period's items by date.
    #[serde(default)]
    pub listed: bool,
}

/// `ask.restarted`: the stream moved on to `provider` (tier routing only). The context was
/// repacked for its locality, so these fields replace [`AskStarted`]'s. Sent before any of its
/// `chat.delta`s, possibly before the `retrieve.ask` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskRestarted {
    pub stream_id: String,
    pub provider: ProviderId,
    pub locality: Locality,
    pub citations: Vec<Citation>,
    #[serde(default)]
    pub excluded_sensitive: u32,
    #[serde(default)]
    pub general: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<AskDateFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<u32>,
}

// ---- local runtime ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Chat,
    Embedding,
    /// Cross-encoder reranker, used by retrieval only.
    Rerank,
    /// Local Double Tap draft model (`[draft.local] model`), in its own server so drafting
    /// never waits behind Chat. Called by provider id only.
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
    /// What went wrong, as one short sentence for the user (`failed`).
    #[serde(default)]
    pub error: Option<String>,
    /// Milliseconds spent in the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_ms: Option<u64>,
    /// While `starting`: expected load time in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_ms: Option<u64>,
    /// While `failed`: ms until the next automatic restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    /// While `failed`: the supervisor gave up after repeated failures and waits for
    /// `runtime.retry`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gave_up: bool,
    /// The server's last output line before it failed (for the log view).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRetryRequest {
    pub role: RuntimeRole,
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
    /// All evidence is from sensitive items: kept out of the vault and default-scope MCP.
    #[serde(default)]
    pub sensitive: bool,
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
    /// Every active claim and mention comes from sensitive items: no vault page, and hidden
    /// from `knowledge.graph` unless `include_sensitive`.
    #[serde(default)]
    pub sensitive: bool,
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
    /// Only nodes and edges supported by items of this source (the centre always stays).
    #[serde(default)]
    pub source: Option<SourceId>,
    /// Also show sensitive-only entities and claims.
    #[serde(default)]
    pub include_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeView {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub depth: u32,
    /// Sensitive-only entity (see [`EntityDetail::sensitive`]).
    #[serde(default)]
    pub sensitive: bool,
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
    /// An extraction run is in progress.
    #[serde(default)]
    pub running: bool,
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

/// `draft.check_sensitive` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveCheck {
    pub text: String,
}

/// `draft.check_sensitive` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveVerdict {
    pub sensitive: bool,
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

// ---- Feature method names ----

pub mod runtime_retry_method {
    pub const RETRY: &str = "runtime.retry";
}

/// The one-time offer to re-index search with the recommended embedding model. Params `{}`;
/// every method answers [`SearchUpgradeStatus`].
pub mod search_upgrade_method {
    pub const UPGRADE_STATUS: &str = "search.upgrade.status";
    /// Accept: download the model if needed, then re-embed in the background and switch.
    pub const UPGRADE_START: &str = "search.upgrade.start";
    /// Hide the offer for good, cancelling an upgrade in progress (the old index stays).
    pub const UPGRADE_DISMISS: &str = "search.upgrade.dismiss";
}

/// See [`search_upgrade_method`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchUpgradeStatus {
    /// Until accepted or dismissed, when the index's embedding model is not the recommended one.
    pub offer: Option<SearchUpgradeOffer>,
    /// The upgrade in progress.
    pub job: Option<SearchUpgradeJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchUpgradeOffer {
    /// The index's embedding model and the recommended one (manifest ids).
    pub from: String,
    pub to: String,
    /// Bytes to download first (0 when the model is installed).
    pub download_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchUpgradeJob {
    pub to: String,
    pub phase: SearchUpgradePhase,
    /// Bytes while downloading, chunks while re-embedding.
    pub done: u64,
    pub total: u64,
    /// The last failure; the upgrade retries later.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchUpgradePhase {
    Downloading,
    Embedding,
}

/// Request and response types live in nuggetd's `features/search_ui`.
pub mod search_ui_method {
    pub const FACETED: &str = "search.faceted";
    pub const TIMELINE: &str = "search.timeline";
    pub const PREVIEW: &str = "search.preview";
}

// ---- Plugins (M6) ----

pub mod plugin_method {
    pub const LIST: &str = "plugins.list";
    pub const INSPECT: &str = "plugins.inspect";
    pub const INSTALL: &str = "plugins.install";
    pub const APPROVE: &str = "plugins.approve";
    pub const REMOVE: &str = "plugins.remove";
    pub const ENABLE: &str = "plugins.enable";
    pub const CATALOG: &str = "plugins.catalog";
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
    /// Settings fields of a connector source that hold a folder or file the plugin may read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_roots: Vec<String>,
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

/// The verified package, staged under `token` (10 minutes) until `plugins.approve`, or
/// committed at once (`installed`) when no approval is needed.
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

/// One `.nuggetplugin` in a catalog directory, verified but not installed. With `error` set
/// only `path` and `file_name` are reliable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogEntry {
    /// Absolute path to pass to `plugins.install`.
    pub path: String,
    pub file_name: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kinds: Vec<String>,
    /// What the package's manifest requests.
    #[serde(default)]
    pub requested: PluginCapabilities,
    #[serde(default)]
    pub connector_display_name: Option<String>,
    #[serde(default)]
    pub signature: PluginSignature,
    /// Hex ed25519 public key that signed the package (empty when unknown or unsigned).
    #[serde(default)]
    pub signer: String,
    /// The installed version of this plugin, if any.
    #[serde(default)]
    pub installed_version: Option<String>,
    /// Installed at an older version than this package.
    #[serde(default)]
    pub update_available: bool,
    /// Installing it would ask for approval (a new install, or an update asking for more).
    #[serde(default)]
    pub needs_approval: bool,
    /// Why it cannot be installed (unreadable, unsigned, untrusted signer, ...).
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogResult {
    pub entries: Vec<PluginCatalogEntry>,
    /// The directories that were searched.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// More packages were found than are listed.
    #[serde(default)]
    pub truncated: bool,
}

// ---- Agent (M4) ----
//
// Served by the daemon's `agent`, `policy` and `browser` features. Types owned by those crates
// travel as JSON values; each field names the type whose serialized form it carries.

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
    /// [`super::AgentEventNotice`], broadcast to every connection.
    pub const AGENT_EVENT: &str = "agent.event";
    /// [`super::KillState`], broadcast when the kill switch is engaged or released.
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
    /// Candidate answers offered with `question` (empty: free text). Untrusted screen text.
    #[serde(default)]
    pub options: Vec<String>,
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
// Cards, the daily brief and reminders. `cards.execute` takes an approval bound to the digest of
// the `cards.preview` the user saw, and runs through the policy gate.

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

/// Text the app writes in the user's language: a message key (`unanswered.title`,
/// `action.reminder`, ...) and its arguments. Dates are ISO strings (`YYYY-MM-DD`, or RFC 3339
/// for a time), counts and amounts numbers (an amount comes with its ISO 4217 `currency`); the
/// app formats them for the locale. Names, subjects and quotes are untrusted source text. A key
/// the app does not know falls back to the English text sent beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalText {
    pub key: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub args: std::collections::BTreeMap<String, serde_json::Value>,
}

impl LocalText {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            args: Default::default(),
        }
    }

    /// Add an argument.
    #[must_use]
    pub fn arg(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.args.insert(name.to_owned(), value.into());
        self
    }
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
    /// `label` for the app to localise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_text: Option<LocalText>,
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
    /// `title` for the app to localise (the English `title` is the fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_text: Option<LocalText>,
    /// `detail` for the app to localise; unset when the detail is quoted source text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_text: Option<LocalText>,
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

/// One way feedback changed a card's ranking (the app phrases it).
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
    /// `summary` for the app to localise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<LocalText>,
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
    /// For `needs_confirmation`: what to ask, for the app to localise (`detail` is the English).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_text: Option<LocalText>,
    #[serde(default)]
    pub artifact: Option<ActionArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefCardView {
    pub card_id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    /// See [`CardView::title_text`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_text: Option<LocalText>,
    /// `summary` for the app to localise; unset for the judge's summary or quoted source text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<LocalText>,
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
    /// The calendar item it comes from.
    #[serde(default)]
    pub item: Option<ItemRef>,
    #[serde(default)]
    pub url: Option<String>,
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
    fn disk_status_is_additive_and_classified() {
        // An older daemon's status (no disk) still parses.
        let old: StatusResponse = serde_json::from_value(serde_json::json!({
            "protocol": 1, "version": "x", "uptime_secs": 1, "items": 0, "chunks": 0,
            "providers": []
        }))
        .unwrap();
        assert_eq!(old.disk, None);
        let mib = 1024 * 1024;
        let level = |free, err| DiskStatus::classify(free, err).level;
        assert_eq!(level(Some(50 * 1024 * mib), false), DiskLevel::Ok);
        assert_eq!(level(Some(50 * 1024 * mib), true), DiskLevel::Ok);
        assert_eq!(level(Some(500 * mib), false), DiskLevel::Low);
        assert_eq!(level(Some(500 * mib), true), DiskLevel::Full);
        assert_eq!(level(Some(199 * mib), false), DiskLevel::Full);
        assert_eq!(level(None, false), DiskLevel::Ok);
        assert_eq!(level(None, true), DiskLevel::Full);
        let v = serde_json::to_value(DiskStatus::classify(Some(5), true)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"free_bytes": 5, "level": "full", "recent_full_error": true})
        );
    }
}
