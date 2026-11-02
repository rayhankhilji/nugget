//! Tauri commands: thin, typed proxies to `nuggetd` methods (see `src/api.ts`).

use nugget_core::egress::EgressEvent;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::PluginCatalogResult;
use nugget_core::ipc::{
    AgentAnswer, AgentConfirm, AgentListRequest, AgentRunRequest, AgentStarted, AgentTaskDetail,
    AgentTaskRef, AgentTaskSummary, BrowserStatus, KillState, PolicyApprovals, PolicyLogExport,
    PolicySet, PolicyState, agent_method,
};
use nugget_core::ipc::{
    AskRequest, AskStarted, AuthBegin, ChatCancel, ChatRequest, ChatStarted, ClaimStatusRequest,
    ContextRequest, ContextResponse, EgressRecentRequest, EntitiesRequest, EntityDetail, EntityRef,
    EntitySummaryView, GraphRequest, GraphViewIpc, IngestStatus, KnowledgeStatus, McpClientCreate,
    McpClientCreated, McpClientInfo, McpClientRef, MergeRequest, ModelCancelRequest, ModelInfo,
    ModelInstallRequest, PluginApproveParams, PluginApproveResult, PluginEnableParams,
    PluginIdParams, PluginInfo, PluginInspectResult, PluginInstallParams, PluginInstallResult,
    PluginListResult, PluginRemoveResult, ProbeRequest, ProviderInfo, RuntimeInfo, SearchHit,
    SearchRequest, SourceInfo, SourceRef, SourceRemove, SourceSecret, SourceSpec, SourceSync,
    SourceUpdate, SplitRequest, StatusResponse, method, plugin_method,
};
use nugget_core::ipc::{
    BriefView, CardExecuteRequest, CardExecuteResult, CardFeedbackRequest, CardPreview,
    CardPreviewRequest, CardView, CardsChanged, CardsList, CardsListRequest, ProactiveLearning,
    ProactiveMetrics, ReminderView, ResetLearningRequest, ResetLearningResult, proactive_method,
};
use nugget_core::ipc::{EgressPageRequest, Page, SearchPageRequest};
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
/// The graph canvas renders at most 2,000 nodes; headroom lets the daemon report truncation.
const MAX_GRAPH_LIMIT: u32 = 2_500;
const MAX_GRAPH_DEPTH: u32 = 4;
/// Free-text fields (search text, entity ids, client names) beyond this are a UI bug.
const MAX_TEXT_LEN: usize = 1024;
const MAX_CLIENT_NAME_LEN: usize = 100;
/// Entity kinds and MCP tool names are short identifiers.
const MAX_LIST_LEN: usize = 64;
/// Facts one split may move.
const MAX_SPLIT_CLAIMS: usize = 2_000;
/// File paths (plugin packages, backup folders) beyond this are a UI bug.
const MAX_PATH_LEN: usize = 4096;
const MAX_CARD_ID_LEN: usize = 256;
const MAX_REASON_TEXT_LEN: usize = 2_000;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(CmdError::Other(msg.into()))
}

/// Commands without arguments: `name -> Output = method, params;`.
macro_rules! no_arg_commands {
    ($($name:ident -> $out:ty = $method:expr, $params:expr;)*) => {$(
        #[tauri::command]
        pub async fn $name(app: AppHandle, daemon: State<'_, Daemon>) -> Result<$out> {
            daemon.call(&app, $method, &$params).await
        }
    )*};
}

/// Commands that pass `request` through after an optional check:
/// `name(Request) -> Output = method [, check];`.
macro_rules! request_commands {
    ($($name:ident($req:ty) -> $out:ty = $method:expr $(, $check:expr)?;)*) => {$(
        #[tauri::command]
        pub async fn $name(
            app: AppHandle,
            daemon: State<'_, Daemon>,
            request: $req,
        ) -> Result<$out> {
            $(($check)(&request)?;)?
            daemon.call(&app, $method, &request).await
        }
    )*};
}

no_arg_commands! {
    status -> StatusResponse = method::STATUS, ();
    providers_list -> Vec<ProviderInfo> = method::PROVIDERS_LIST, ();
    models_list -> Vec<ModelInfo> = method::MODELS_LIST, ();
    sources_list -> Vec<SourceInfo> = method::SOURCES_LIST, ();
    ingest_status -> IngestStatus = method::INGEST_STATUS, ();
    ai_key_saved -> Value = "ai.key.saved", ();
    models_fit -> Value = "models.fit", ();
    runtime_status -> Vec<RuntimeInfo> = method::RUNTIME_STATUS, ();
    knowledge_status -> KnowledgeStatus = method::KNOWLEDGE_STATUS, ();
    mcp_clients_list -> Vec<McpClientInfo> = method::MCP_CLIENTS_LIST, ();
    plugins_list -> PluginListResult = plugin_method::LIST, ();
    plugins_catalog -> PluginCatalogResult = plugin_method::CATALOG, ();
    brief_get -> BriefView = proactive_method::BRIEF_GET, json!({});
    proactive_metrics -> ProactiveMetrics = proactive_method::PROACTIVE_METRICS, json!({});
    proactive_refresh -> CardsChanged = proactive_method::PROACTIVE_REFRESH, json!({});
    proactive_learning -> ProactiveLearning = proactive_method::PROACTIVE_LEARNING, json!({});
    reminders_list -> Vec<ReminderView> = proactive_method::REMINDERS_LIST, json!({});
    backup_status -> Value = "backup.status", json!({});
    policy_get -> PolicyState = agent_method::POLICY_GET, ();
    policy_approvals -> PolicyApprovals = agent_method::POLICY_APPROVALS, ();
    policy_kill -> KillState = agent_method::POLICY_KILL, ();
    policy_resume -> KillState = agent_method::POLICY_RESUME, ();
    policy_log_export -> PolicyLogExport = agent_method::POLICY_LOG_EXPORT, ();
    browser_status -> BrowserStatus = agent_method::BROWSER_STATUS, ();
}

// API keys travel in these requests: they are passed through, never logged. Backup requests may
// carry a passphrase, likewise. The daemon validates the rest.
request_commands! {
    sources_add(SourceSpec) -> SourceInfo = method::SOURCES_ADD, check_source_spec;
    sources_update(SourceUpdate) -> SourceInfo = method::SOURCES_UPDATE;
    sources_auth_begin(SourceRef) -> AuthBegin = method::SOURCES_AUTH_BEGIN;
    ai_key_test(Value) -> Value = "ai.key.test";
    ai_key_save(Value) -> Value = "ai.key.save";
    ai_key_update(Value) -> Value = "ai.key.update";
    ai_key_remove(Value) -> Value = "ai.key.remove";
    knowledge_entity(EntityRef) -> EntityDetail = method::KNOWLEDGE_ENTITY,
        |r: &EntityRef| check_entity_id("requested", r.id.as_str());
    knowledge_split(SplitRequest) -> EntityRef = method::KNOWLEDGE_SPLIT, check_split;
    plugins_inspect(PluginIdParams) -> PluginInspectResult = plugin_method::INSPECT, check_plugin_id;
    // Verifies and stages a package; `plugins_approve` commits it (unless it is an update within
    // the capabilities the user already approved).
    plugins_install(PluginInstallParams) -> PluginInstallResult = plugin_method::INSTALL,
        check_plugin_install;
    plugins_approve(PluginApproveParams) -> PluginApproveResult = plugin_method::APPROVE,
        |r: &PluginApproveParams| check_plugin_field("install token", &r.token);
    plugins_remove(PluginIdParams) -> PluginRemoveResult = plugin_method::REMOVE, check_plugin_id;
    plugins_enable(PluginEnableParams) -> PluginInfo = plugin_method::ENABLE,
        |r: &PluginEnableParams| check_plugin_field("id", &r.id);
    cards_list(CardsListRequest) -> CardsList = proactive_method::CARDS_LIST;
    cards_feedback(CardFeedbackRequest) -> CardView = proactive_method::CARDS_FEEDBACK,
        check_card_feedback;
    cards_preview(CardPreviewRequest) -> CardPreview = proactive_method::CARDS_PREVIEW,
        |r: &CardPreviewRequest| check_card_id(&r.card_id);
    cards_execute(CardExecuteRequest) -> CardExecuteResult = proactive_method::CARDS_EXECUTE,
        check_card_execute;
    proactive_reset_learning(ResetLearningRequest) -> ResetLearningResult =
        proactive_method::PROACTIVE_RESET_LEARNING, check_reset_learning;
    backup_configure(Value) -> Value = "backup.configure", check_backup_request;
    backup_set_passphrase(Value) -> Value = "backup.set_passphrase", check_backup_request;
    backup_run(Value) -> Value = "backup.run", check_backup_request;
    backup_list(Value) -> Value = "backup.list", check_backup_request;
    // Stages a restore; the daemon exits right after replying and the UI calls `start_daemon`,
    // which applies it.
    backup_restore(Value) -> Value = "backup.restore", check_backup_request;
    export_run(Value) -> Value = "export.run", check_backup_request;
    search_faceted(Value) -> Value = "search.faceted";
    search_timeline(Value) -> Value = "search.timeline";
    search_preview(Value) -> Value = "search.preview";
    search_upgrade_status(Value) -> Value = "search.upgrade.status";
    search_upgrade_start(Value) -> Value = "search.upgrade.start";
    search_upgrade_dismiss(Value) -> Value = "search.upgrade.dismiss";
    agent_run(AgentRunRequest) -> AgentStarted = agent_method::AGENT_RUN, check_agent_run;
    agent_list(AgentListRequest) -> Vec<AgentTaskSummary> = agent_method::AGENT_LIST;
    agent_get(AgentTaskRef) -> AgentTaskDetail = agent_method::AGENT_GET;
    policy_set(PolicySet) -> PolicyState = agent_method::POLICY_SET;
}

/// Calls a method whose result the UI doesn't need.
async fn call_unit<P: serde::Serialize + Sync>(
    app: &AppHandle,
    daemon: &Daemon,
    method: &str,
    params: &P,
) -> Result<()> {
    let _: Value = daemon.call(app, method, params).await?;
    Ok(())
}

fn check_agent_run(request: &AgentRunRequest) -> Result<()> {
    if request.goal.trim().is_empty() {
        return err("an agent task needs a goal");
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_cancel(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: AgentTaskRef,
) -> Result<()> {
    call_unit(&app, &daemon, agent_method::AGENT_CANCEL, &request).await
}

#[tauri::command]
pub async fn agent_confirm(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: AgentConfirm,
) -> Result<()> {
    call_unit(&app, &daemon, agent_method::AGENT_CONFIRM, &request).await
}

#[tauri::command]
pub async fn agent_answer(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: AgentAnswer,
) -> Result<()> {
    call_unit(&app, &daemon, agent_method::AGENT_ANSWER, &request).await
}

#[tauri::command]
pub async fn connection_info(app: AppHandle, daemon: State<'_, Daemon>) -> Result<ConnectionInfo> {
    Ok(daemon.connection_info(&app).await)
}

#[tauri::command]
pub async fn start_daemon(app: AppHandle, daemon: State<'_, Daemon>) -> Result<ConnectionInfo> {
    daemon.start(&app).await
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
        return err("a chat needs at least one message");
    }
    daemon.call(&app, method::CHAT, &request).await
}

#[tauri::command]
pub async fn chat_cancel(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    stream_id: String,
) -> Result<()> {
    call_unit(
        &app,
        &daemon,
        method::CHAT_CANCEL,
        &ChatCancel { stream_id },
    )
    .await
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

/// Stops a running install; its final progress says `cancelled`.
#[tauri::command]
pub async fn models_cancel(app: AppHandle, daemon: State<'_, Daemon>, id: String) -> Result<Value> {
    daemon
        .call(&app, method::MODELS_CANCEL, &ModelCancelRequest { id })
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

/// Cursor-paged search; the daemon validates cursors.
#[tauri::command]
pub async fn search_page(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SearchPageRequest,
) -> Result<Page<SearchHit>> {
    let request = SearchPageRequest {
        limit: request.limit.clamp(1, MAX_SEARCH_LIMIT),
        ..request
    };
    daemon.call(&app, method::SEARCH_PAGE, &request).await
}

#[tauri::command]
pub async fn egress_page(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: EgressPageRequest,
) -> Result<Page<EgressEvent>> {
    let request = EgressPageRequest {
        limit: request.limit.clamp(1, MAX_EGRESS_LIMIT),
        ..request
    };
    daemon.call(&app, method::EGRESS_PAGE, &request).await
}

fn check_source_spec(request: &SourceSpec) -> Result<()> {
    if request.kind.trim().is_empty() || request.name.trim().is_empty() {
        return err("a source needs a kind and a name");
    }
    Ok(())
}

#[tauri::command]
pub async fn sources_remove(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceRemove,
) -> Result<()> {
    call_unit(&app, &daemon, method::SOURCES_REMOVE, &request).await
}

/// Starts a sync pass; progress arrives as `nugget:ingest-progress` events.
#[tauri::command]
pub async fn sources_sync(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: SourceSync,
) -> Result<()> {
    call_unit(&app, &daemon, method::SOURCES_SYNC, &request).await
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
        return err("the password is empty");
    }
    if request.secret.len() > MAX_SECRET_LEN {
        return err("the password is too long");
    }
    call_unit(&app, &daemon, method::SOURCES_SECRET_SET, &request).await
}

fn context_budget(budget: u32) -> u32 {
    budget.clamp(MIN_CONTEXT_BUDGET, MAX_CONTEXT_BUDGET)
}

#[tauri::command]
pub async fn retrieve_context(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: ContextRequest,
) -> Result<ContextResponse> {
    if request.query.trim().is_empty() {
        return err("the question is empty");
    }
    let req = ContextRequest {
        budget_tokens: context_budget(request.budget_tokens),
        ..request
    };
    daemon.call(&app, method::RETRIEVE_CONTEXT, &req).await
}

/// Answer a question from retrieved context (sensitive passages never go to cloud models); output
/// arrives as `nugget:chat-delta` / `nugget:chat-done` events for the returned stream.
#[tauri::command]
pub async fn retrieve_ask(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: AskRequest,
) -> Result<AskStarted> {
    if request.query.trim().is_empty() {
        return err("the question is empty");
    }
    let req = AskRequest {
        budget_tokens: context_budget(request.budget_tokens),
        ..request
    };
    daemon.call(&app, method::RETRIEVE_ASK, &req).await
}

/// Start a local model server again after it failed or gave up.
#[tauri::command]
pub async fn runtime_retry(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    role: String,
) -> Result<Value> {
    daemon
        .call(&app, "runtime.retry", &json!({ "role": role }))
        .await
}

// ---- knowledge graph ----

fn check_text(what: &str, value: &str) -> Result<()> {
    if value.len() > MAX_TEXT_LEN {
        return err(format!("the {what} is too long"));
    }
    Ok(())
}

fn check_entity_id(what: &str, id: &str) -> Result<()> {
    if id.trim().is_empty() {
        return err(format!("the {what} entity is missing"));
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
        return err("too many kinds in the graph filter");
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
        return err("an entity can't be merged into itself");
    }
    Ok(())
}

/// The UI rejects a fact (`rejected`), restores it (`active`), or asserts / withdraws the user's
/// own fact (`user_asserted` / `unasserted`).
fn check_claim_status(request: &ClaimStatusRequest) -> Result<()> {
    match request.status.as_str() {
        "rejected" | "active" | "user_asserted" | "unasserted" => Ok(()),
        other => err(format!(
            "a fact can only be rejected, restored, asserted or withdrawn, not set to “{other}”"
        )),
    }
}

fn check_split(request: &SplitRequest) -> Result<()> {
    check_entity_id("split", request.entity.as_str())?;
    if request.name.trim().is_empty() {
        return err("the new entity needs a name");
    }
    check_text("name", &request.name)?;
    if request.claims.is_empty() {
        return err("choose at least one fact to split off");
    }
    if request.claims.len() > MAX_SPLIT_CLAIMS
        || request.aliases.len() > MAX_LIST_LEN
        || request.identifiers.len() > MAX_LIST_LEN
    {
        return err("too many items in the split");
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
    call_unit(&app, &daemon, method::KNOWLEDGE_MERGE, &request).await
}

#[tauri::command]
pub async fn knowledge_claim_status(
    app: AppHandle,
    daemon: State<'_, Daemon>,
    request: ClaimStatusRequest,
) -> Result<()> {
    check_claim_status(&request)?;
    call_unit(&app, &daemon, method::KNOWLEDGE_CLAIM_STATUS, &request).await
}

// ---- MCP clients ----

fn check_client_create(request: &McpClientCreate) -> Result<McpClientCreate> {
    let name = request.name.trim();
    if name.is_empty() {
        return err("the app needs a name");
    }
    if name.chars().count() > MAX_CLIENT_NAME_LEN {
        return err(format!(
            "the name is too long (at most {MAX_CLIENT_NAME_LEN} characters)"
        ));
    }
    let scopes = &request.scopes;
    if scopes.sources.as_ref().is_some_and(Vec::is_empty) {
        return err("choose at least one source, or allow all sources");
    }
    if scopes
        .sources
        .as_ref()
        .is_some_and(|s| s.len() > MAX_LIST_LEN)
        || scopes.tools.len() > MAX_LIST_LEN
    {
        return err("too many scopes");
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
        return err("no app to revoke");
    }
    check_text("app id", &request.id)?;
    call_unit(&app, &daemon, method::MCP_CLIENTS_REVOKE, &request).await
}

// ---- plugins ----

fn check_plugin_field(what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return err(format!("the plugin {what} is missing"));
    }
    check_text(what, value)
}

fn check_plugin_id(request: &PluginIdParams) -> Result<()> {
    check_plugin_field("id", &request.id)
}

fn check_plugin_install(request: &PluginInstallParams) -> Result<()> {
    if request.path.len() > MAX_PATH_LEN {
        return err("the plugin path is too long");
    }
    if !std::path::Path::new(&request.path).is_absolute() {
        return err("choose a plugin file");
    }
    Ok(())
}

// ---- Sign in with ChatGPT (local mode only) ----

no_arg_commands! {
    chatgpt_status -> Value = "ai.chatgpt.status", json!({});
    chatgpt_cancel -> Value = "ai.chatgpt.cancel", json!({});
    chatgpt_resume -> Value = "ai.chatgpt.resume", json!({});
    chatgpt_sign_out -> Value = "ai.chatgpt.sign_out", json!({});
}

/// The only page a ChatGPT sign-in may open: OpenAI's authorize endpoint.
const CHATGPT_AUTH_PREFIX: &str = "https://auth.openai.com/";
const CHATGPT_AUTH_HOST: &str = "auth.openai.com";

/// `ai.chatgpt.sign_in`'s answer: the authorize URL to open, and the status (`signing_in`).
/// No URL when the daemon refused (Heed Cloud is active: `status.error.code` is `cloud_mode`).
#[derive(serde::Deserialize)]
struct ChatgptSignIn {
    #[serde(default)]
    auth_url: Option<String>,
    status: Value,
}

/// Check the daemon's authorize URL before the browser opens it: `https://auth.openai.com/…`
/// only, and a well-formed web link (no spaces, control characters or other hosts).
fn chatgpt_auth_target(url: &str) -> std::result::Result<open::Target, String> {
    if !url.starts_with(CHATGPT_AUTH_PREFIX) {
        return Err("The ChatGPT sign-in link doesn't go to OpenAI's sign-in page.".into());
    }
    match open::classify(url)? {
        open::Target::Web(u)
            if u.host_str() == Some(CHATGPT_AUTH_HOST) && u.username().is_empty() =>
        {
            Ok(open::Target::Web(u))
        }
        _ => Err("The ChatGPT sign-in link doesn't go to OpenAI's sign-in page.".into()),
    }
}

/// Starts Sign in with ChatGPT: the daemon listens on 127.0.0.1 for the redirect and returns
/// OpenAI's authorize URL, which is checked and opened in the default browser. Returns the
/// status (`signing_in`); the page polls `chatgpt_status` until the sign-in finishes. When the
/// daemon refuses (`auth_url: null`, e.g. Heed Cloud is active) nothing opens and its status is
/// returned. When the link is refused or the browser can't open, the daemon's sign-in is cancelled.
#[tauri::command]
pub async fn chatgpt_sign_in(app: AppHandle, daemon: State<'_, Daemon>) -> Result<Value> {
    let started: ChatgptSignIn = daemon.call(&app, "ai.chatgpt.sign_in", &json!({})).await?;
    // Refused (e.g. `cloud_mode`): nothing to open; the status says why.
    let Some(auth_url) = started.auth_url.as_deref() else {
        return Ok(started.status);
    };
    let opened = match chatgpt_auth_target(auth_url) {
        Ok(target) => tauri::async_runtime::spawn_blocking(move || open::open(target))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map(|_| ())),
        Err(e) => Err(e),
    };
    if let Err(e) = opened {
        // Stop the loopback listener; the error the user sees is the one above.
        let _: std::result::Result<Value, CmdError> =
            daemon.call(&app, "ai.chatgpt.cancel", &json!({})).await;
        return Err(CmdError::Other(e));
    }
    Ok(started.status)
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

fn picked_path(what: &str, p: tauri_plugin_dialog::FilePath) -> Result<String> {
    let path = p
        .into_path()
        .map_err(|e| CmdError::Other(format!("unsupported {what}: {e}")))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| CmdError::Other(format!("{what} paths must be valid Unicode")))
}

fn file_dialog(app: &AppHandle) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    let dialog = app.dialog().file();
    match app.get_webview_window("main") {
        Some(window) => dialog.set_parent(&window),
        None => dialog,
    }
}

/// Native folder picker. Returns the chosen absolute paths (empty when cancelled).
#[tauri::command]
pub async fn pick_folders(app: AppHandle, multiple: bool) -> Result<Vec<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = file_dialog(&app);
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
        .map(|p| picked_path("folder", p))
        .collect()
}

/// Native file picker for a `.nuggetplugin` package. Returns its absolute path, or `None` when
/// cancelled.
#[tauri::command]
pub async fn pick_plugin_file(app: AppHandle) -> Result<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    file_dialog(&app)
        .set_title("Choose a plugin")
        .add_filter("Heed plugin", &["nuggetplugin"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let picked = rx
        .await
        .map_err(|_| CmdError::Other("the file picker closed unexpectedly".into()))?;
    picked.map(|p| picked_path("file", p)).transpose()
}

// ---- proactive: card inbox, brief, reminders ----

const CARD_FEEDBACK: &[&str] = &["accept", "dismiss", "snooze", "seen", "done"];
const DISMISS_REASONS: &[&str] = &[
    "not_relevant",
    "already_handled",
    "wrong_info",
    "not_important",
    "other",
];
/// Scopes `proactive.reset_learning` takes (`all` needs no key).
const LEARNING_SCOPES: &[&str] = &["all", "sender", "domain", "kind", "thread"];

fn check_card_id(card_id: &str) -> Result<()> {
    if card_id.trim().is_empty() || card_id.len() > MAX_CARD_ID_LEN {
        return err("no such card");
    }
    Ok(())
}

fn check_card_feedback(request: &CardFeedbackRequest) -> Result<()> {
    check_card_id(&request.card_id)?;
    if !CARD_FEEDBACK.contains(&request.feedback.as_str()) {
        return err("unknown card feedback");
    }
    if let Some(reason) = &request.reason
        && !DISMISS_REASONS.contains(&reason.as_str())
    {
        return err("unknown dismiss reason");
    }
    if request
        .reason_text
        .as_ref()
        .is_some_and(|t| t.len() > MAX_REASON_TEXT_LEN)
    {
        return err("the reason is too long");
    }
    Ok(())
}

fn check_card_execute(request: &CardExecuteRequest) -> Result<()> {
    check_card_id(&request.card_id)?;
    if request.digest.trim().is_empty() || request.digest.len() > MAX_TEXT_LEN {
        return err("preview the action before approving it");
    }
    Ok(())
}

fn check_reset_learning(request: &ResetLearningRequest) -> Result<()> {
    if !LEARNING_SCOPES.contains(&request.scope.as_str()) {
        return err("unknown learning scope");
    }
    match request.key.as_deref() {
        Some(key) if key.len() > MAX_TEXT_LEN => err("learning key is too long"),
        None | Some("") if request.scope != "all" => err("say what to forget"),
        _ => Ok(()),
    }
}

/// Show an OS notification (morning brief, due reminder) as plain text. With `open_today`,
/// clicking it opens the Today page (see `notify.rs`). The webview has no `notification:`
/// permission, so this is the only way.
#[tauri::command]
pub async fn notify(
    app: AppHandle,
    window: tauri::Window,
    title: String,
    body: String,
    open_today: Option<bool>,
) -> Result<()> {
    // Only the main window composes notifications (not the quick-ask box or the overlay).
    crate::platform::main_only(&window)?;
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

/// Backup and export requests are objects whose string fields are paths or a passphrase.
fn check_backup_request(request: &Value) -> Result<()> {
    let Some(fields) = request.as_object() else {
        return err("the request must be an object");
    };
    for (name, value) in fields {
        let limit = if name == "passphrase" {
            MAX_SECRET_LEN
        } else {
            MAX_PATH_LEN
        };
        if value.as_str().is_some_and(|s| s.len() > limit) {
            return err(format!("the {name} is too long"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::EntityId;
    use serde_json::json;

    #[test]
    fn chatgpt_sign_in_opens_only_openai_auth_pages() {
        let ok = "https://auth.openai.com/oauth/authorize?client_id=app_x&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&state=abc";
        assert!(
            matches!(chatgpt_auth_target(ok), Ok(open::Target::Web(u)) if u.host_str() == Some("auth.openai.com"))
        );
        for bad in [
            "",
            "http://auth.openai.com/oauth/authorize",
            "https://auth.openai.com",
            "https://auth.openai.com.evil.example/oauth/authorize",
            "https://auth.openai.com@evil.example/oauth/authorize",
            "https://evil.example/?next=https://auth.openai.com/",
            "https://AUTH.openai.com/oauth/authorize",
            "https://auth.openai.com/oauth/authorize?x=a b",
            "https://auth.openai.com/oauth/authorize\n",
            "javascript:alert(1)//https://auth.openai.com/",
            "file:///https://auth.openai.com/",
        ] {
            assert!(chatgpt_auth_target(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn chatgpt_sign_in_refusal_has_no_url() {
        let refused: ChatgptSignIn = serde_json::from_value(json!({
            "auth_url": null,
            "status": { "state": "signed_out", "error": { "code": "cloud_mode", "message": "Heed Cloud is active" } }
        }))
        .unwrap();
        assert!(refused.auth_url.is_none());
        assert_eq!(refused.status["error"]["code"], "cloud_mode");
        let started: ChatgptSignIn = serde_json::from_value(json!({
            "auth_url": "https://auth.openai.com/oauth/authorize?x=1",
            "status": { "state": "signing_in" }
        }))
        .unwrap();
        assert!(chatgpt_auth_target(started.auth_url.as_deref().unwrap()).is_ok());
    }

    #[test]
    fn requests_are_bounded_and_validated() {
        let req = entities_request(EntitiesRequest {
            text: Some("   ".into()),
            kind: Some(String::new()),
            limit: 1_000_000,
            offset: 3,
        })
        .unwrap();
        assert!(req.text.is_none() && req.kind.is_none());
        assert_eq!(req.limit, MAX_ENTITIES_LIMIT);
        assert!(
            entities_request(EntitiesRequest {
                text: Some("x".repeat(MAX_TEXT_LEN + 1)),
                limit: 10,
                ..Default::default()
            })
            .is_err()
        );

        let g = graph_request(GraphRequest {
            depth: 99,
            limit: 1_000_000,
            ..Default::default()
        })
        .unwrap();
        assert_eq!((g.depth, g.limit), (MAX_GRAPH_DEPTH, MAX_GRAPH_LIMIT));
        let g = graph_request(GraphRequest::default()).unwrap();
        assert_eq!((g.depth, g.limit), (1, 1));
        assert!(
            graph_request(GraphRequest {
                center: Some(EntityId::new(" ")),
                ..Default::default()
            })
            .is_err()
        );

        let same = MergeRequest {
            into: EntityId::new("ent_1"),
            from: EntityId::new("ent_1"),
        };
        assert!(check_merge(&same).is_err());

        for (status, ok) in [
            ("rejected", true),
            ("unasserted", true),
            ("retracted", false),
        ] {
            let req = ClaimStatusRequest {
                claim: 1,
                status: status.into(),
            };
            assert_eq!(check_claim_status(&req).is_ok(), ok, "{status}");
        }

        let split = SplitRequest {
            entity: EntityId::new("ent_1"),
            claims: vec![3],
            name: "Sam Lee".into(),
            kind: None,
            aliases: vec![],
            identifiers: vec![("email".into(), "sam@home.net".into())],
        };
        assert!(check_split(&split).is_ok());
        assert!(
            check_split(&SplitRequest {
                claims: vec![],
                ..split.clone()
            })
            .is_err()
        );

        assert!(check_plugin_field("id", " ").is_err());
        let relative = PluginInstallParams {
            path: "rss.nuggetplugin".into(),
        };
        assert!(check_plugin_install(&relative).is_err());
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
        for name in ["", &"x".repeat(MAX_CLIENT_NAME_LEN + 1)] {
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

    /// Secrets reach the webview where they must (the token is shown once) but never a log line
    /// or an error message.
    #[test]
    fn secrets_are_never_logged_or_quoted_in_errors() {
        let secret: SourceSecret =
            serde_json::from_value(json!({ "id": "imap-1", "secret": "hunter2" })).unwrap();
        assert!(!format!("{secret:?}").contains("hunter2"));

        let created = json!({
            "client": {
                "id": "mcp-1",
                "name": "Claude Desktop",
                "scopes": { "sources": null, "tools": [], "include_sensitive": false },
                "created_at": "2026-10-28T09:00:00Z",
                "last_used": null
            },
            "token": TOKEN,
            "setup": []
        });
        let c: McpClientCreated = serde_json::from_value(created.clone()).unwrap();
        assert!(!format!("{c:?}").contains(TOKEN));
        assert!(!format!("{c:#?}").contains(TOKEN));
        assert_eq!(serde_json::to_value(&c).unwrap()["token"], TOKEN);

        // serde quotes a string found where a list was expected.
        let mut bad = created;
        bad["setup"] = json!(TOKEN);
        let e = serde_json::from_value::<McpClientCreated>(bad).unwrap_err();
        let leaked = CmdError::Protocol(format!("{}: {e}", method::MCP_CLIENTS_CREATE));
        assert!(leaked.to_string().contains(TOKEN), "fixture should leak");
        let shown = without_response_detail(leaked);
        assert!(!shown.to_string().contains(TOKEN));
        assert!(!format!("{shown:?}").contains(TOKEN));
    }

    #[test]
    fn proactive_requests_are_checked() {
        let fb = |v: Value| serde_json::from_value::<CardFeedbackRequest>(v).unwrap();
        assert!(
            check_card_feedback(&fb(json!({
                "card_id": "c1", "feedback": "dismiss", "reason": "other", "reason_text": "meh"
            })))
            .is_ok()
        );
        for bad in [
            json!({ "card_id": "c1", "feedback": "delete" }),
            json!({ "card_id": "c1", "feedback": "dismiss", "reason": "spam" }),
            json!({ "card_id": " ", "feedback": "seen" }),
        ] {
            assert!(check_card_feedback(&fb(bad)).is_err());
        }

        let reset = |v: Value| serde_json::from_value::<ResetLearningRequest>(v).unwrap();
        assert!(check_reset_learning(&reset(json!({ "scope": "all" }))).is_ok());
        assert!(
            check_reset_learning(&reset(json!({ "scope": "sender", "key": "bo@x.io" }))).is_ok()
        );
        assert!(check_reset_learning(&reset(json!({ "scope": "kind" }))).is_err());
        assert!(check_reset_learning(&reset(json!({ "scope": "everything" }))).is_err());
    }
}
