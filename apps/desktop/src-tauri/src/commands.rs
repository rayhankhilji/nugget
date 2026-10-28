//! Tauri commands: thin, typed proxies to `nuggetd` methods (see `src/api.ts`).

use nugget_core::egress::EgressEvent;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    ChatCancel, ChatRequest, ChatStarted, EgressRecentRequest, ModelInfo, ModelInstallRequest,
    ProbeRequest, ProviderInfo, SearchHit, SearchRequest, StatusResponse, method,
};
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::daemon::{CmdError, ConnectionInfo, Daemon};

type Result<T> = std::result::Result<T, CmdError>;

/// Upper bounds so a UI bug can't ask the daemon for unbounded work.
const MAX_SEARCH_LIMIT: u32 = 200;
const MAX_EGRESS_LIMIT: u32 = 10_000;

#[tauri::command]
pub async fn connection_info(app: AppHandle, daemon: State<'_, Daemon>) -> Result<ConnectionInfo> {
    Ok(daemon.connection_info(&app).await)
}

#[tauri::command]
pub async fn start_daemon(app: AppHandle, daemon: State<'_, Daemon>) -> Result<ConnectionInfo> {
    daemon.start(&app).await
}

#[tauri::command]
pub async fn status(app: AppHandle, daemon: State<'_, Daemon>) -> Result<StatusResponse> {
    daemon.call(&app, method::STATUS, &()).await
}

#[tauri::command]
pub async fn providers_list(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<Vec<ProviderInfo>> {
    daemon.call(&app, method::PROVIDERS_LIST, &()).await
}

/// The probe result type is not fixed by the protocol yet, so it is passed through as JSON.
#[tauri::command]
pub async fn providers_probe(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    provider: String,
) -> Result<Value> {
    let req = ProbeRequest {
        provider: ProviderId::new(provider),
    };
    daemon.call(&app, method::PROVIDERS_PROBE, &req).await
}

/// Starts a chat; output arrives as `nugget:chat-delta` / `nugget:chat-done` events.
#[tauri::command]
pub async fn chat(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: ChatRequest,
) -> Result<ChatStarted> {
    if request.messages.is_empty() {
        return Err(CmdError::Other("a chat needs at least one message".into()));
    }
    daemon.call(&app, method::CHAT, &request).await
}

#[tauri::command]
pub async fn chat_cancel(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    stream_id: String,
) -> Result<()> {
    let _: Value = daemon
        .call(&app, method::CHAT_CANCEL, &ChatCancel { stream_id })
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn search(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    query: String,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    let req = SearchRequest {
        query,
        limit: limit.clamp(1, MAX_SEARCH_LIMIT),
    };
    daemon.call(&app, method::SEARCH, &req).await
}

#[tauri::command]
pub async fn models_list(app: AppHandle, daemon: State<'_, Daemon>) -> Result<Vec<ModelInfo>> {
    daemon.call(&app, method::MODELS_LIST, &()).await
}

/// Starts an install; progress arrives as `nugget:models-progress` events.
#[tauri::command]
pub async fn models_install(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    id: String,
) -> Result<Value> {
    daemon
        .call(&app, method::MODELS_INSTALL, &ModelInstallRequest { id })
        .await
}

#[tauri::command]
pub async fn egress_recent(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    limit: u32,
) -> Result<Vec<EgressEvent>> {
    let req = EgressRecentRequest {
        limit: limit.clamp(1, MAX_EGRESS_LIMIT),
    };
    daemon.call(&app, method::EGRESS_RECENT, &req).await
}
