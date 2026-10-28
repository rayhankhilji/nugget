//! Tauri commands: thin, typed proxies to `nuggetd` methods (see `src/api.ts`).

use nugget_core::egress::EgressEvent;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AskRequest, AskStarted, AuthBegin, ChatCancel, ChatRequest, ChatStarted, ContextRequest,
    ContextResponse, EgressRecentRequest, IngestStatus, ModelInfo, ModelInstallRequest,
    ProbeRequest, ProviderInfo, RuntimeInfo, SearchHit, SearchRequest, SourceInfo, SourceRef,
    SourceRemove, SourceSecret, SourceSpec, SourceSync, SourceUpdate, StatusResponse, method,
};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::daemon::{CmdError, ConnectionInfo, Daemon};
use crate::open::{self, Opened};

type Result<T> = std::result::Result<T, CmdError>;

/// Upper bounds so a UI bug can't ask the daemon for unbounded work.
const MAX_SEARCH_LIMIT: u32 = 200;
const MAX_EGRESS_LIMIT: u32 = 10_000;
const MIN_CONTEXT_BUDGET: u32 = 256;
const MAX_CONTEXT_BUDGET: u32 = 32_000;
/// Secrets longer than this are certainly a paste mistake.
const MAX_SECRET_LEN: usize = 4096;

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

// ---- sources (M1) ----

#[tauri::command]
pub async fn sources_list(app: AppHandle, daemon: State<'_, Daemon>) -> Result<Vec<SourceInfo>> {
    daemon.call(&app, method::SOURCES_LIST, &()).await
}

#[tauri::command]
pub async fn sources_add(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceSpec,
) -> Result<SourceInfo> {
    if request.kind.trim().is_empty() || request.name.trim().is_empty() {
        return Err(CmdError::Other("a source needs a kind and a name".into()));
    }
    daemon.call(&app, method::SOURCES_ADD, &request).await
}

#[tauri::command]
pub async fn sources_update(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceUpdate,
) -> Result<SourceInfo> {
    daemon.call(&app, method::SOURCES_UPDATE, &request).await
}

#[tauri::command]
pub async fn sources_remove(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceRemove,
) -> Result<()> {
    let _: Value = daemon.call(&app, method::SOURCES_REMOVE, &request).await?;
    Ok(())
}

/// Starts a sync pass; progress arrives as `nugget:ingest-progress` events.
#[tauri::command]
pub async fn sources_sync(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceSync,
) -> Result<()> {
    let _: Value = daemon.call(&app, method::SOURCES_SYNC, &request).await?;
    Ok(())
}

/// Starts an OAuth authorization. The UI opens the returned URL with `open_url`; completion
/// arrives as a `nugget:sources-changed` event.
#[tauri::command]
pub async fn sources_auth_begin(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceRef,
) -> Result<AuthBegin> {
    daemon
        .call(&app, method::SOURCES_AUTH_BEGIN, &request)
        .await
}

/// Stores a password in the OS keychain through the daemon. Write-only: nothing returns it, and
/// it is never logged (`SourceSecret`'s `Debug` redacts it).
#[tauri::command]
pub async fn sources_secret_set(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceSecret,
) -> Result<()> {
    if request.secret.is_empty() {
        return Err(CmdError::Other("the password is empty".into()));
    }
    if request.secret.len() > MAX_SECRET_LEN {
        return Err(CmdError::Other("the password is too long".into()));
    }
    let _: Value = daemon
        .call(&app, method::SOURCES_SECRET_SET, &request)
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn ingest_status(app: AppHandle, daemon: State<'_, Daemon>) -> Result<IngestStatus> {
    daemon.call(&app, method::INGEST_STATUS, &()).await
}

#[tauri::command]
pub async fn retrieve_context(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: ContextRequest,
) -> Result<ContextResponse> {
    if request.query.trim().is_empty() {
        return Err(CmdError::Other("the question is empty".into()));
    }
    let req = ContextRequest {
        budget_tokens: request
            .budget_tokens
            .clamp(MIN_CONTEXT_BUDGET, MAX_CONTEXT_BUDGET),
        ..request
    };
    daemon.call(&app, method::RETRIEVE_CONTEXT, &req).await
}

/// Answer a question from retrieved context. The daemon chooses the model, then packs the
/// context for it (sensitive passages never go to cloud models); output arrives as
/// `nugget:chat-delta` / `nugget:chat-done` events for the returned stream.
#[tauri::command]
pub async fn retrieve_ask(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: AskRequest,
) -> Result<AskStarted> {
    if request.query.trim().is_empty() {
        return Err(CmdError::Other("the question is empty".into()));
    }
    let req = AskRequest {
        budget_tokens: request
            .budget_tokens
            .clamp(MIN_CONTEXT_BUDGET, MAX_CONTEXT_BUDGET),
        ..request
    };
    daemon.call(&app, method::RETRIEVE_ASK, &req).await
}

#[tauri::command]
pub async fn runtime_status(app: AppHandle, daemon: State<'_, Daemon>) -> Result<Vec<RuntimeInfo>> {
    daemon.call(&app, method::RUNTIME_STATUS, &()).await
}

// ---- shell helpers ----

/// Opens an `https:`, `obsidian:` or local `file:` URL with the OS default handler. Anything else
/// is refused, and files that could run code are shown in their folder instead (see `open.rs`).
#[tauri::command]
pub async fn open_url(url: String) -> Result<Opened> {
    let target = open::classify(&url).map_err(CmdError::Other)?;
    tauri::async_runtime::spawn_blocking(move || open::open(target))
        .await
        .map_err(|e| CmdError::Other(e.to_string()))?
        .map_err(CmdError::Other)
}

/// Native folder picker. Returns the chosen absolute paths (empty when cancelled).
#[tauri::command]
pub async fn pick_folders(app: AppHandle, multiple: bool) -> Result<Vec<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    if multiple {
        dialog
            .set_title("Choose folders")
            .pick_folders(move |paths| {
                let _ = tx.send(paths.unwrap_or_default());
            });
    } else {
        dialog
            .set_title("Choose a folder")
            .pick_folder(move |path| {
                let _ = tx.send(path.into_iter().collect());
            });
    }
    let picked = rx
        .await
        .map_err(|_| CmdError::Other("the folder picker closed unexpectedly".into()))?;
    picked
        .into_iter()
        .map(|p| {
            let path = p
                .into_path()
                .map_err(|e| CmdError::Other(format!("unsupported folder: {e}")))?;
            path.into_os_string()
                .into_string()
                .map_err(|_| CmdError::Other("folder paths must be valid Unicode".into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `request` objects `api.ts` sends must decode into the IPC types unchanged.
    #[test]
    fn frontend_requests_match_the_ipc_types() {
        let spec: SourceSpec = serde_json::from_value(json!({
            "id": null, "kind": "files", "name": "Docs",
            "settings": { "roots": ["/home/me/Docs"], "include": [], "exclude": [] }
        }))
        .unwrap();
        assert_eq!(spec.kind, "files");
        assert!(spec.id.is_none());

        let upd: SourceUpdate =
            serde_json::from_value(json!({ "id": "files-1", "enabled": false })).unwrap();
        assert_eq!(upd.enabled, Some(false));
        assert!(upd.name.is_none() && upd.settings.is_none());

        let rm: SourceRemove =
            serde_json::from_value(json!({ "id": "files-1", "purge": false })).unwrap();
        assert!(!rm.purge);

        let sync: SourceSync = serde_json::from_value(json!({ "id": null })).unwrap();
        assert!(sync.id.is_none());

        let r: SourceRef = serde_json::from_value(json!({ "id": "gmail-1" })).unwrap();
        assert_eq!(r.id.as_str(), "gmail-1");

        let secret: SourceSecret =
            serde_json::from_value(json!({ "id": "imap-1", "secret": "hunter2" })).unwrap();
        assert!(!format!("{secret:?}").contains("hunter2"));

        let ctx: ContextRequest =
            serde_json::from_value(json!({ "query": "q", "budget_tokens": 2000 })).unwrap();
        assert_eq!(ctx.budget_tokens, 2000);
    }
}
