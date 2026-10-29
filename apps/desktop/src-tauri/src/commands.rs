//! Tauri commands: thin, typed proxies to `nuggetd` methods (see `src/api.ts`).

use nugget_core::egress::EgressEvent;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AskRequest, AskStarted, AuthBegin, ChatCancel, ChatRequest, ChatStarted, ClaimStatusRequest,
    ContextRequest, ContextResponse, EgressRecentRequest, EntitiesRequest, EntityDetail, EntityRef,
    EntitySummaryView, GraphRequest, GraphViewIpc, IngestStatus, KnowledgeStatus, McpClientCreate,
    McpClientCreated, McpClientInfo, McpClientRef, MergeRequest, ModelInfo, ModelInstallRequest,
    PluginApproveParams, PluginApproveResult, PluginEnableParams, PluginIdParams, PluginInfo,
    PluginInspectResult, PluginInstallParams, PluginInstallResult, PluginListResult,
    PluginRemoveResult, ProbeRequest, ProviderInfo, RuntimeInfo, SearchHit, SearchRequest,
    SourceInfo, SourceRef, SourceRemove, SourceSecret, SourceSpec, SourceSync, SourceUpdate,
    SplitRequest, StatusResponse, method, plugin_method,
};
use nugget_core::ipc::{
    BriefView, CardExecuteRequest, CardExecuteResult, CardFeedbackRequest, CardPreview,
    CardPreviewRequest, CardView, CardsChanged, CardsList, CardsListRequest, ProactiveMetrics,
    ReminderView, proactive_method,
};
use serde_json::{Value, json};
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
/// Entity browser pages (the UI asks for one more than it shows, to know if there is a next page).
const MAX_ENTITIES_LIMIT: u32 = 501;
/// Graph view: the canvas renders at most 2,000 nodes; a little headroom lets the daemon report
/// truncation honestly.
const MAX_GRAPH_LIMIT: u32 = 2_500;
const MAX_GRAPH_DEPTH: u32 = 4;
/// Free-text fields (search text, entity ids, client names) beyond this are a UI bug.
const MAX_TEXT_LEN: usize = 1024;
const MAX_CLIENT_NAME_LEN: usize = 100;
/// Entity kinds and MCP tool names are short identifiers.
const MAX_LIST_LEN: usize = 64;
/// Facts one split may move.
const MAX_SPLIT_CLAIMS: usize = 2_000;

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

// ---- knowledge graph (M2) ----

fn check_text(what: &str, value: &str) -> Result<()> {
    if value.len() > MAX_TEXT_LEN {
        return Err(CmdError::Other(format!("the {what} is too long")));
    }
    Ok(())
}

fn check_entity_id(what: &str, id: &str) -> Result<()> {
    if id.trim().is_empty() {
        return Err(CmdError::Other(format!("the {what} entity is missing")));
    }
    check_text(what, id)
}

/// Normalise an entity browser request: bounded page size, blank filters dropped.
fn entities_request(request: EntitiesRequest) -> Result<EntitiesRequest> {
    let text = request
        .text
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    if let Some(t) = &text {
        check_text("search text", t)?;
    }
    let kind = request.kind.filter(|k| !k.trim().is_empty());
    if let Some(k) = &kind {
        check_text("kind", k)?;
    }
    Ok(EntitiesRequest {
        text,
        kind,
        limit: request.limit.clamp(1, MAX_ENTITIES_LIMIT),
        offset: request.offset,
    })
}

fn graph_request(request: GraphRequest) -> Result<GraphRequest> {
    if let Some(c) = &request.center {
        check_entity_id("center", c.as_str())?;
    }
    if request.kinds.len() > MAX_LIST_LEN {
        return Err(CmdError::Other("too many kinds in the graph filter".into()));
    }
    for k in &request.kinds {
        check_text("kind", k)?;
    }
    if let Some(s) = &request.source {
        check_text("source", s.as_str())?;
    }
    Ok(GraphRequest {
        depth: request.depth.clamp(1, MAX_GRAPH_DEPTH),
        limit: request.limit.clamp(1, MAX_GRAPH_LIMIT),
        ..request
    })
}

fn check_merge(request: &MergeRequest) -> Result<()> {
    check_entity_id("target", request.into.as_str())?;
    check_entity_id("merged", request.from.as_str())?;
    if request.into == request.from {
        return Err(CmdError::Other(
            "an entity can't be merged into itself".into(),
        ));
    }
    Ok(())
}

/// The UI rejects a fact (`rejected`), restores it (`active`), or asserts / withdraws the user's
/// own fact (`user_asserted` / `unasserted`).
fn check_claim_status(request: &ClaimStatusRequest) -> Result<()> {
    match request.status.as_str() {
        "rejected" | "active" | "user_asserted" | "unasserted" => Ok(()),
        other => Err(CmdError::Other(format!(
            "a fact can only be rejected, restored, asserted or withdrawn, not set to “{other}”"
        ))),
    }
}

fn check_split(request: &SplitRequest) -> Result<()> {
    check_entity_id("split", request.entity.as_str())?;
    if request.name.trim().is_empty() {
        return Err(CmdError::Other("the new entity needs a name".into()));
    }
    check_text("name", &request.name)?;
    if request.claims.is_empty() {
        return Err(CmdError::Other(
            "choose at least one fact to split off".into(),
        ));
    }
    if request.claims.len() > MAX_SPLIT_CLAIMS
        || request.aliases.len() > MAX_LIST_LEN
        || request.identifiers.len() > MAX_LIST_LEN
    {
        return Err(CmdError::Other("too many items in the split".into()));
    }
    if let Some(k) = &request.kind {
        check_text("kind", k)?;
    }
    for a in &request.aliases {
        check_text("alias", a)?;
    }
    for (scheme, value) in &request.identifiers {
        check_text("identifier", scheme)?;
        check_text("identifier", value)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn knowledge_entities(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: EntitiesRequest,
) -> Result<Vec<EntitySummaryView>> {
    let req = entities_request(request)?;
    daemon.call(&app, method::KNOWLEDGE_ENTITIES, &req).await
}

#[tauri::command]
pub async fn knowledge_entity(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: EntityRef,
) -> Result<EntityDetail> {
    check_entity_id("requested", request.id.as_str())?;
    daemon.call(&app, method::KNOWLEDGE_ENTITY, &request).await
}

#[tauri::command]
pub async fn knowledge_graph(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: GraphRequest,
) -> Result<GraphViewIpc> {
    let req = graph_request(request)?;
    daemon.call(&app, method::KNOWLEDGE_GRAPH, &req).await
}

#[tauri::command]
pub async fn knowledge_merge(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: MergeRequest,
) -> Result<()> {
    check_merge(&request)?;
    let _: Value = daemon.call(&app, method::KNOWLEDGE_MERGE, &request).await?;
    Ok(())
}

/// Reject, restore, assert or withdraw a fact (see `check_claim_status`).
#[tauri::command]
pub async fn knowledge_claim_status(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: ClaimStatusRequest,
) -> Result<()> {
    check_claim_status(&request)?;
    let _: Value = daemon
        .call(&app, method::KNOWLEDGE_CLAIM_STATUS, &request)
        .await?;
    Ok(())
}

/// Split facts off an entity into a new one; returns the new entity.
#[tauri::command]
pub async fn knowledge_split(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SplitRequest,
) -> Result<EntityRef> {
    check_split(&request)?;
    daemon.call(&app, method::KNOWLEDGE_SPLIT, &request).await
}

#[tauri::command]
pub async fn knowledge_status(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<KnowledgeStatus> {
    daemon.call(&app, method::KNOWLEDGE_STATUS, &()).await
}

// ---- MCP clients (M2) ----

fn check_client_create(request: &McpClientCreate) -> Result<McpClientCreate> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(CmdError::Other("the app needs a name".into()));
    }
    if name.chars().count() > MAX_CLIENT_NAME_LEN {
        return Err(CmdError::Other(format!(
            "the name is too long (at most {MAX_CLIENT_NAME_LEN} characters)"
        )));
    }
    let scopes = &request.scopes;
    if scopes.sources.as_ref().is_some_and(Vec::is_empty) {
        return Err(CmdError::Other(
            "choose at least one source, or allow all sources".into(),
        ));
    }
    if scopes
        .sources
        .as_ref()
        .is_some_and(|s| s.len() > MAX_LIST_LEN)
        || scopes.tools.len() > MAX_LIST_LEN
    {
        return Err(CmdError::Other("too many scopes".into()));
    }
    Ok(McpClientCreate {
        name: name.to_owned(),
        scopes: scopes.clone(),
    })
}

/// A decode error can quote the offending value, which for this response may be the token, so
/// the detail is dropped.
fn without_response_detail(err: CmdError) -> CmdError {
    match err {
        CmdError::Protocol(_) => CmdError::Protocol(format!(
            "{}: the response could not be read",
            method::MCP_CLIENTS_CREATE
        )),
        other => other,
    }
}

#[tauri::command]
pub async fn mcp_clients_list(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<Vec<McpClientInfo>> {
    daemon.call(&app, method::MCP_CLIENTS_LIST, &()).await
}

/// Creates a client. The result carries its bearer token, which is shown once in the UI and
/// never logged (`McpClientCreated`'s `Debug` redacts it).
#[tauri::command]
pub async fn mcp_clients_create(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: McpClientCreate,
) -> Result<McpClientCreated> {
    let req = check_client_create(&request)?;
    daemon
        .call(&app, method::MCP_CLIENTS_CREATE, &req)
        .await
        .map_err(without_response_detail)
}

#[tauri::command]
pub async fn mcp_clients_revoke(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: McpClientRef,
) -> Result<()> {
    if request.id.trim().is_empty() {
        return Err(CmdError::Other("no app to revoke".into()));
    }
    check_text("app id", &request.id)?;
    let _: Value = daemon
        .call(&app, method::MCP_CLIENTS_REVOKE, &request)
        .await?;
    Ok(())
}

// ---- plugins (M6) ----

/// Longest file path the UI may hand to `plugins.install`.
const MAX_PATH_LEN: usize = 4096;

fn check_plugin_field(what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(CmdError::Other(format!("the plugin {what} is missing")));
    }
    check_text(what, value)
}

fn check_plugin_install(request: &PluginInstallParams) -> Result<()> {
    if request.path.len() > MAX_PATH_LEN {
        return Err(CmdError::Other("the plugin path is too long".into()));
    }
    if !std::path::Path::new(&request.path).is_absolute() {
        return Err(CmdError::Other("choose a plugin file".into()));
    }
    Ok(())
}

#[tauri::command]
pub async fn plugins_list(app: AppHandle, daemon: State<'_, Daemon>) -> Result<PluginListResult> {
    daemon.call(&app, plugin_method::LIST, &()).await
}

#[tauri::command]
pub async fn plugins_inspect(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: PluginIdParams,
) -> Result<PluginInspectResult> {
    check_plugin_field("id", &request.id)?;
    daemon.call(&app, plugin_method::INSPECT, &request).await
}

/// Verifies and stages a package; it is committed only by `plugins_approve` (unless it is an
/// update within the capabilities the user already approved).
#[tauri::command]
pub async fn plugins_install(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: PluginInstallParams,
) -> Result<PluginInstallResult> {
    check_plugin_install(&request)?;
    daemon.call(&app, plugin_method::INSTALL, &request).await
}

#[tauri::command]
pub async fn plugins_approve(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: PluginApproveParams,
) -> Result<PluginApproveResult> {
    check_plugin_field("install token", &request.token)?;
    daemon.call(&app, plugin_method::APPROVE, &request).await
}

#[tauri::command]
pub async fn plugins_remove(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: PluginIdParams,
) -> Result<PluginRemoveResult> {
    check_plugin_field("id", &request.id)?;
    daemon.call(&app, plugin_method::REMOVE, &request).await
}

#[tauri::command]
pub async fn plugins_enable(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: PluginEnableParams,
) -> Result<PluginInfo> {
    check_plugin_field("id", &request.id)?;
    daemon.call(&app, plugin_method::ENABLE, &request).await
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

// ---- W4 proactive (M5) ----
//
// Typed proxies for the card inbox, morning brief and reminders, plus two shell helpers the
// Today page needs: OS notifications (driven from Rust; the webview has no `notification:`
// permission) and opening a draft in the mail app (`mailto:` only, see `open::mailto`).

/// Card ids and dismiss reasons are short; free-text dismiss reasons stay modest.
const MAX_CARD_ID_LEN: usize = 256;
const MAX_REASON_TEXT_LEN: usize = 2_000;

const CARD_FEEDBACK: &[&str] = &["accept", "dismiss", "snooze", "seen", "done"];
const DISMISS_REASONS: &[&str] = &[
    "not_relevant",
    "already_handled",
    "wrong_info",
    "not_important",
    "other",
];

fn check_card_id(card_id: &str) -> Result<()> {
    if card_id.trim().is_empty() || card_id.len() > MAX_CARD_ID_LEN {
        return Err(CmdError::Other("no such card".into()));
    }
    Ok(())
}

fn check_card_feedback(request: &CardFeedbackRequest) -> Result<()> {
    check_card_id(&request.card_id)?;
    if !CARD_FEEDBACK.contains(&request.feedback.as_str()) {
        return Err(CmdError::Other("unknown card feedback".into()));
    }
    if let Some(reason) = &request.reason
        && !DISMISS_REASONS.contains(&reason.as_str())
    {
        return Err(CmdError::Other("unknown dismiss reason".into()));
    }
    if request
        .reason_text
        .as_ref()
        .is_some_and(|t| t.len() > MAX_REASON_TEXT_LEN)
    {
        return Err(CmdError::Other("the reason is too long".into()));
    }
    Ok(())
}

#[tauri::command]
pub async fn cards_list(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: CardsListRequest,
) -> Result<CardsList> {
    daemon
        .call(&app, proactive_method::CARDS_LIST, &request)
        .await
}

#[tauri::command]
pub async fn cards_feedback(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: CardFeedbackRequest,
) -> Result<CardView> {
    check_card_feedback(&request)?;
    daemon
        .call(&app, proactive_method::CARDS_FEEDBACK, &request)
        .await
}

#[tauri::command]
pub async fn cards_preview(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: CardPreviewRequest,
) -> Result<CardPreview> {
    check_card_id(&request.card_id)?;
    daemon
        .call(&app, proactive_method::CARDS_PREVIEW, &request)
        .await
}

#[tauri::command]
pub async fn cards_execute(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: CardExecuteRequest,
) -> Result<CardExecuteResult> {
    check_card_id(&request.card_id)?;
    if request.digest.trim().is_empty() || request.digest.len() > MAX_TEXT_LEN {
        return Err(CmdError::Other(
            "preview the action before approving it".into(),
        ));
    }
    daemon
        .call(&app, proactive_method::CARDS_EXECUTE, &request)
        .await
}

#[tauri::command]
pub async fn brief_get(app: AppHandle, daemon: State<'_, Daemon>) -> Result<BriefView> {
    daemon
        .call(&app, proactive_method::BRIEF_GET, &json!({}))
        .await
}

#[tauri::command]
pub async fn proactive_metrics(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<ProactiveMetrics> {
    daemon
        .call(&app, proactive_method::PROACTIVE_METRICS, &json!({}))
        .await
}

#[tauri::command]
pub async fn proactive_refresh(app: AppHandle, daemon: State<'_, Daemon>) -> Result<CardsChanged> {
    daemon
        .call(&app, proactive_method::PROACTIVE_REFRESH, &json!({}))
        .await
}

#[tauri::command]
pub async fn reminders_list(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<Vec<ReminderView>> {
    daemon
        .call(&app, proactive_method::REMINDERS_LIST, &json!({}))
        .await
}

/// Show an OS notification (morning brief, due reminder). Text is shown as plain text. With
/// `open_today`, clicking it opens the Today page (see `notify.rs`).
#[tauri::command]
pub async fn notify(
    app: AppHandle,
    title: String,
    body: String,
    open_today: Option<bool>,
) -> Result<()> {
    crate::notify::show(&app, &title, &body, open_today.unwrap_or(false)).map_err(CmdError::Other)
}

/// Opens a `mailto:` link (a drafted reply) in the default mail app. Nothing else is accepted.
#[tauri::command]
pub async fn open_mailto(url: String) -> Result<()> {
    let url = open::mailto(&url).map_err(CmdError::Other)?;
    tauri::async_runtime::spawn_blocking(move || {
        tauri_plugin_opener::open_url(url.as_str(), None::<&str>)
            .map_err(|e| format!("Couldn't open the mail app: {e}"))
    })
    .await
    .map_err(|e| CmdError::Other(e.to_string()))?
    .map_err(CmdError::Other)
}

/// Native file picker for a `.nuggetplugin` package. Returns its absolute path, or `None` when
/// cancelled.
#[tauri::command]
pub async fn pick_plugin_file(app: AppHandle) -> Result<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    dialog
        .set_title("Choose a plugin")
        .add_filter("Nugget plugin", &["nuggetplugin"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let picked = rx
        .await
        .map_err(|_| CmdError::Other("the file picker closed unexpectedly".into()))?;
    picked
        .map(|p| {
            let path = p
                .into_path()
                .map_err(|e| CmdError::Other(format!("unsupported file: {e}")))?;
            path.into_os_string()
                .into_string()
                .map_err(|_| CmdError::Other("file paths must be valid Unicode".into()))
        })
        .transpose()
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

    /// The plugin `request` objects `api.ts` sends decode into the IPC types and are checked.
    #[test]
    fn plugin_requests_match_the_ipc_types() {
        let id: PluginIdParams = serde_json::from_value(json!({ "id": "rss" })).unwrap();
        assert!(check_plugin_field("id", &id.id).is_ok());
        assert!(check_plugin_field("id", " ").is_err());

        let absolute = if cfg!(windows) {
            r"C:\Users\me\rss.nuggetplugin"
        } else {
            "/home/me/rss.nuggetplugin"
        };
        let install: PluginInstallParams =
            serde_json::from_value(json!({ "path": absolute })).unwrap();
        assert!(check_plugin_install(&install).is_ok());
        let relative = PluginInstallParams {
            path: "rss.nuggetplugin".into(),
        };
        assert!(check_plugin_install(&relative).is_err());

        let approve: PluginApproveParams =
            serde_json::from_value(json!({ "token": "tok-1", "approve": false })).unwrap();
        assert!(!approve.approve);

        let enable: PluginEnableParams =
            serde_json::from_value(json!({ "id": "rss", "enabled": true })).unwrap();
        assert!(enable.enabled);
    }

    /// The knowledge and MCP `request` objects `api.ts` builds decode into the IPC types.
    #[test]
    fn knowledge_and_mcp_requests_match_the_ipc_types() {
        let ents: EntitiesRequest = serde_json::from_value(
            json!({ "text": "sam", "kind": "person", "limit": 51, "offset": 50 }),
        )
        .unwrap();
        assert_eq!(ents.kind.as_deref(), Some("person"));
        assert_eq!((ents.limit, ents.offset), (51, 50));
        // Optional fields may be omitted or null.
        let ents: EntitiesRequest =
            serde_json::from_value(json!({ "text": null, "limit": 10 })).unwrap();
        assert!(ents.text.is_none() && ents.kind.is_none() && ents.offset == 0);

        let r: EntityRef = serde_json::from_value(json!({ "id": "ent_1" })).unwrap();
        assert_eq!(r.id.as_str(), "ent_1");

        let g: GraphRequest = serde_json::from_value(
            json!({ "center": "ent_1", "depth": 2, "limit": 2000, "kinds": ["person", "project"] }),
        )
        .unwrap();
        assert_eq!(g.center.as_ref().map(|c| c.as_str()), Some("ent_1"));
        assert_eq!(g.kinds.len(), 2);
        let g: GraphRequest =
            serde_json::from_value(json!({ "center": null, "depth": 1, "limit": 200 })).unwrap();
        assert!(g.center.is_none() && g.kinds.is_empty());

        let m: MergeRequest =
            serde_json::from_value(json!({ "into": "ent_1", "from": "ent_2" })).unwrap();
        assert_eq!(m.from.as_str(), "ent_2");

        let c: ClaimStatusRequest =
            serde_json::from_value(json!({ "claim": 42, "status": "retracted" })).unwrap();
        assert_eq!(c.claim, 42);

        let create: McpClientCreate = serde_json::from_value(json!({
            "name": "Claude Desktop",
            "scopes": { "sources": ["files-1"], "tools": ["search"], "include_sensitive": false }
        }))
        .unwrap();
        assert_eq!(create.scopes.sources.as_ref().map(Vec::len), Some(1));
        // "All sources, all read-only tools" is `sources: null, tools: []`.
        let create: McpClientCreate = serde_json::from_value(json!({
            "name": "Cursor",
            "scopes": { "sources": null, "tools": [], "include_sensitive": false }
        }))
        .unwrap();
        assert!(create.scopes.sources.is_none() && !create.scopes.include_sensitive);

        let r: McpClientRef = serde_json::from_value(json!({ "id": "mcp-1" })).unwrap();
        assert_eq!(r.id, "mcp-1");
    }

    #[test]
    fn knowledge_requests_are_bounded_and_validated() {
        let req = entities_request(EntitiesRequest {
            text: Some("   ".into()),
            kind: Some(String::new()),
            limit: 1_000_000,
            offset: 3,
        })
        .unwrap();
        assert!(req.text.is_none() && req.kind.is_none());
        assert_eq!(req.limit, MAX_ENTITIES_LIMIT);
        let req = entities_request(EntitiesRequest {
            limit: 0,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(req.limit, 1);
        assert!(
            entities_request(EntitiesRequest {
                text: Some("x".repeat(MAX_TEXT_LEN + 1)),
                limit: 10,
                ..Default::default()
            })
            .is_err()
        );

        let g = graph_request(GraphRequest {
            center: None,
            depth: 99,
            limit: 1_000_000,
            kinds: vec![],
            source: None,
        })
        .unwrap();
        assert_eq!((g.depth, g.limit), (MAX_GRAPH_DEPTH, MAX_GRAPH_LIMIT));
        let g = graph_request(GraphRequest::default()).unwrap();
        assert_eq!((g.depth, g.limit), (1, 1));
        assert!(
            graph_request(GraphRequest {
                center: Some(nugget_core::ids::EntityId::new(" ")),
                depth: 1,
                limit: 10,
                kinds: vec![],
                source: None,
            })
            .is_err()
        );

        let same = MergeRequest {
            into: nugget_core::ids::EntityId::new("ent_1"),
            from: nugget_core::ids::EntityId::new("ent_1"),
        };
        assert!(check_merge(&same).is_err());

        for (status, ok) in [
            ("rejected", true),
            ("active", true),
            ("user_asserted", true),
            ("unasserted", true),
            ("retracted", false),
            ("superseded", false),
            ("", false),
        ] {
            let req = ClaimStatusRequest {
                claim: 1,
                status: status.into(),
            };
            assert_eq!(check_claim_status(&req).is_ok(), ok, "{status}");
        }

        let split = SplitRequest {
            entity: nugget_core::ids::EntityId::new("ent_1"),
            claims: vec![3],
            name: "Sam Lee".into(),
            kind: None,
            aliases: vec![],
            identifiers: vec![("email".into(), "sam@home.net".into())],
        };
        assert!(check_split(&split).is_ok());
        assert!(
            check_split(&SplitRequest {
                name: "  ".into(),
                ..split.clone()
            })
            .is_err()
        );
        assert!(
            check_split(&SplitRequest {
                claims: vec![],
                ..split.clone()
            })
            .is_err()
        );
    }

    #[test]
    fn mcp_client_create_is_validated() {
        use nugget_core::ipc::McpScopes;
        let scopes = McpScopes {
            sources: None,
            tools: vec![],
            include_sensitive: false,
        };
        let ok = check_client_create(&McpClientCreate {
            name: "  Cursor  ".into(),
            scopes: scopes.clone(),
        })
        .unwrap();
        assert_eq!(ok.name, "Cursor");
        for name in ["", "   ", &"x".repeat(MAX_CLIENT_NAME_LEN + 1)] {
            let req = McpClientCreate {
                name: name.into(),
                scopes: scopes.clone(),
            };
            assert!(check_client_create(&req).is_err());
        }
        let none_selected = McpClientCreate {
            name: "Cursor".into(),
            scopes: McpScopes {
                sources: Some(vec![]),
                ..scopes
            },
        };
        assert!(check_client_create(&none_selected).is_err());
    }

    const TOKEN: &str = "nmcp_s3cr3t-token-value";

    fn created_json() -> Value {
        json!({
            "client": {
                "id": "mcp-1",
                "name": "Claude Desktop",
                "scopes": { "sources": null, "tools": [], "include_sensitive": false },
                "created_at": "2026-10-28T09:00:00Z",
                "last_used": null
            },
            "token": TOKEN,
            "setup": [
                { "app": "claude-desktop", "config_path": "~/Library/Application Support/Claude/claude_desktop_config.json", "text": format!("{{\"token\": \"{TOKEN}\"}}") }
            ]
        })
    }

    /// The token reaches the webview (it must be shown once) but never a log line.
    #[test]
    fn mcp_token_is_redacted_in_debug() {
        let created: McpClientCreated = serde_json::from_value(created_json()).unwrap();
        assert_eq!(created.token, TOKEN);
        let dbg = format!("{created:?}");
        assert!(!dbg.contains(TOKEN), "Debug leaked the token: {dbg}");
        assert!(dbg.contains("<redacted>"));
        let pretty = format!("{created:#?}");
        assert!(!pretty.contains(TOKEN));
        // Serialization for the webview keeps it.
        let back = serde_json::to_value(&created).unwrap();
        assert_eq!(back["token"], TOKEN);
        assert_eq!(back["client"]["created_at"], "2026-10-28T09:00:00Z");
    }

    /// A malformed create response must not put the token into an error message the UI shows.
    #[test]
    fn create_decode_errors_never_quote_the_token() {
        let mut bad = created_json();
        // serde quotes a string found where a list was expected.
        bad["setup"] = json!(TOKEN);
        let err = serde_json::from_value::<McpClientCreated>(bad).unwrap_err();
        let leaked = CmdError::Protocol(format!("{}: {err}", method::MCP_CLIENTS_CREATE));
        assert!(leaked.to_string().contains(TOKEN), "fixture should leak");
        let shown = without_response_detail(leaked);
        assert!(!shown.to_string().contains(TOKEN));
        assert!(!format!("{shown:?}").contains(TOKEN));
        let v = serde_json::to_value(&shown).unwrap();
        assert_eq!(v["kind"], "protocol");
        // Other errors pass through unchanged.
        let rpc = without_response_detail(CmdError::Rpc {
            code: -32601,
            message: "method not found".into(),
        });
        assert!(matches!(rpc, CmdError::Rpc { code: -32601, .. }));
    }

    /// The proactive `request` objects `api.ts` builds decode into the IPC types and are checked.
    #[test]
    fn proactive_requests_match_the_ipc_types() {
        let list: CardsListRequest = serde_json::from_value(json!({})).unwrap();
        assert!(!list.include_closed);
        let fb: CardFeedbackRequest = serde_json::from_value(json!({
            "card_id": "c1", "feedback": "dismiss", "reason": "other", "reason_text": "meh"
        }))
        .unwrap();
        assert!(check_card_feedback(&fb).is_ok());
        let snooze: CardFeedbackRequest = serde_json::from_value(json!({
            "card_id": "c1", "feedback": "snooze", "until": "2026-10-29T08:00:00Z"
        }))
        .unwrap();
        assert!(snooze.until.is_some() && check_card_feedback(&snooze).is_ok());
        let bad: CardFeedbackRequest =
            serde_json::from_value(json!({ "card_id": "c1", "feedback": "delete" })).unwrap();
        assert!(check_card_feedback(&bad).is_err());
        let bad: CardFeedbackRequest = serde_json::from_value(
            json!({ "card_id": "c1", "feedback": "dismiss", "reason": "spam" }),
        )
        .unwrap();
        assert!(check_card_feedback(&bad).is_err());
        let bad: CardFeedbackRequest =
            serde_json::from_value(json!({ "card_id": " ", "feedback": "seen" })).unwrap();
        assert!(check_card_feedback(&bad).is_err());
        let prev: CardPreviewRequest =
            serde_json::from_value(json!({ "card_id": "c1", "action": 0 })).unwrap();
        assert_eq!(prev.action, 0);
        let exec: CardExecuteRequest = serde_json::from_value(
            json!({ "card_id": "c1", "action": 1, "digest": "abc", "confirmed": true }),
        )
        .unwrap();
        assert!(exec.confirmed);
        let exec: CardExecuteRequest =
            serde_json::from_value(json!({ "card_id": "c1", "action": 1, "digest": "abc" }))
                .unwrap();
        assert!(!exec.confirmed);
    }
}
