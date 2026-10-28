//! App ⇄ `nuggetd` protocol: JSON-RPC 2.0, one JSON object per line, over a local socket
//! (Unix domain socket on macOS/Linux, named pipe on Windows). Server → client notifications
//! carry streaming output. Method names are the `METHOD` constants below.

use serde::{Deserialize, Serialize};

use crate::capability::{Capabilities, Locality, Tier};
use crate::egress::EgressEvent;
use crate::ids::{ItemRef, ProviderId};

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
}

pub mod notification {
    pub const CHAT_DELTA: &str = "chat.delta";
    pub const CHAT_DONE: &str = "chat.done";
    pub const MODELS_PROGRESS: &str = "models.progress";
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
