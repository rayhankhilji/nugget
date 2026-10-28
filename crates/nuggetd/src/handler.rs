//! The IPC [`Handler`]: one function per `nugget_core::ipc::method`.

use std::collections::HashSet;
use std::sync::{Arc, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use nugget_connector::AuthKind;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    self, AuthBegin, ChatCancel, ChatDelta, ChatDone, ChatStarted, ContextRequest, ContextResponse,
    EgressRecentRequest, ModelInfo, ModelInstallRequest, ModelPhase, ModelProgress,
    PROTOCOL_VERSION, ProbeRequest, ProviderInfo, Role as IpcRole, SearchHit, SearchRequest,
    SourceRef, SourceRemove, SourceSecret, SourceSpec, SourceSync, SourceUpdate, StatusResponse,
    method, notification,
};
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, FEATURE_PROBE, Message, ProbeOptions, Provider,
    ProviderConfig, StopReason, probe_with,
};
use nugget_ipc::{Handler, Notifier, RpcError, code, parse_params, to_result};
use nugget_models::{InstalledModel, ModelEntry, recommend};
use nugget_store::AuditEntry;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::tier_name;
use crate::daemon::{InFlightGuard, State, blocking, spawn_ensure};
use crate::sources::{client_secret_account, reconcile_settings};

/// Upper bounds for list-style requests.
const MAX_SEARCH_RESULTS: u32 = 200;
const MAX_EGRESS_EVENTS: u32 = 1000;
/// Upper bound for `retrieve.context` budgets, in tokens.
const MAX_CONTEXT_TOKENS: u32 = 128 * 1024;
/// Longest accepted source secret (passwords, OAuth client secrets), in bytes.
const MAX_SECRET_BYTES: usize = 4096;
/// How long the daemon waits for an OAuth flow's result (ingest abandons it after
/// [`nugget_ingest::AUTH_TIMEOUT`]).
const AUTH_RESULT_WAIT: std::time::Duration =
    nugget_ingest::AUTH_TIMEOUT.saturating_add(std::time::Duration::from_secs(30));

const ALL_TIERS: [Tier; 5] = [
    Tier::Triage,
    Tier::Light,
    Tier::Synthesis,
    Tier::Agentic,
    Tier::Embedding,
];

pub(crate) struct RpcHandler {
    state: Arc<State>,
}

#[async_trait]
impl Handler for RpcHandler {
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError> {
        tracing::debug!(%method, "rpc call");
        match method {
            method::STATUS => self.status().await,
            method::CHAT => self.chat(parse_params(params)?, notify).await,
            method::CHAT_CANCEL => self.chat_cancel(parse_params(params)?),
            method::SEARCH => self.search(parse_params(params)?).await,
            method::PROVIDERS_LIST => to_result(self.provider_infos()),
            method::PROVIDERS_PROBE => self.probe(parse_params(params)?).await,
            method::MODELS_LIST => self.models_list().await,
            method::MODELS_INSTALL => self.models_install(parse_params(params)?, notify).await,
            method::EGRESS_RECENT => self.egress_recent(parse_params(params)?).await,
            method::SOURCES_LIST => to_result(self.state.ingest.list().await?),
            method::SOURCES_ADD => self.sources_add(parse_params(params)?).await,
            method::SOURCES_UPDATE => self.sources_update(parse_params(params)?).await,
            method::SOURCES_REMOVE => self.sources_remove(parse_params(params)?).await,
            method::SOURCES_SYNC => self.sources_sync(parse_params(params)?),
            method::SOURCES_AUTH_BEGIN => self.sources_auth_begin(parse_params(params)?).await,
            method::SOURCES_SECRET_SET => self.sources_secret_set(parse_params(params)?).await,
            method::INGEST_STATUS => to_result(self.state.ingest.status().await?),
            method::RETRIEVE_CONTEXT => self.retrieve_context(parse_params(params)?).await,
            method::RUNTIME_STATUS => to_result(
                self.state
                    .local
                    .as_ref()
                    .map(|l| l.status())
                    .unwrap_or_default(),
            ),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    /// Every connection receives broadcast notifications (`ingest.progress`, `sources.changed`).
    fn connected(&self, notify: &Notifier) {
        self.state.hub.subscribe(notify);
    }
}

impl RpcHandler {
    pub(crate) fn new(state: Arc<State>) -> Self {
        Self { state }
    }

    // ---- status / providers ----

    async fn status(&self) -> Result<Value, RpcError> {
        let store = self.state.store.clone();
        let counts = blocking(move || store.counts()).await?;
        to_result(StatusResponse {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: self.state.started.elapsed().as_secs(),
            items: counts.items,
            chunks: counts.chunks,
            providers: self.provider_infos(),
            audit_chain_ok: self.state.audit_chain_ok.get().copied(),
        })
    }

    /// Every configured provider in config order, including ones that failed to build (with
    /// `error` set), so a misconfigured provider never silently disappears.
    fn provider_infos(&self) -> Vec<ProviderInfo> {
        let router = &self.state.router;
        self.state
            .configs
            .iter()
            .filter_map(|cfg| {
                if let Some(reason) = self.state.unavailable.get(&cfg.id) {
                    return Some(unavailable_info(cfg, reason));
                }
                let p = router.provider(&cfg.id)?;
                let meta = p.meta();
                Some(ProviderInfo {
                    id: p.id().clone(),
                    kind: meta.kind.as_str().to_string(),
                    model: meta.model,
                    locality: meta.locality,
                    tiers: router.tiers_of(p.id()),
                    capabilities: router.capabilities(p.id()),
                    error: None,
                })
            })
            .chain(self.runtime_infos())
            .collect()
    }

    /// Local runtime slots whose model is installed (the others cannot serve anything).
    fn runtime_infos(&self) -> Vec<ProviderInfo> {
        let Some(local) = &self.state.local else {
            return Vec::new();
        };
        let router = &self.state.router;
        local
            .slots
            .iter()
            .filter(|s| s.is_installed())
            .map(|s| {
                let meta = s.meta();
                ProviderInfo {
                    id: s.id().clone(),
                    kind: meta.kind.as_str().to_string(),
                    model: meta.model,
                    locality: meta.locality,
                    tiers: router.tiers_of(s.id()),
                    capabilities: router.capabilities(s.id()),
                    error: local.slot_error(s),
                }
            })
            .collect()
    }

    async fn probe(&self, req: ProbeRequest) -> Result<Value, RpcError> {
        // Runtime slots are probed like any provider, but their results are not persisted (the
        // server's port changes between runs).
        let cfg = self.state.config_of(&req.provider).cloned();
        let slot_model = self.state.slot(&req.provider).map(|s| s.model().id.clone());
        let model = match (&cfg, slot_model) {
            (Some(c), _) => c.model.clone(),
            (None, Some(m)) => m,
            (None, None) => return Err(self.unknown_provider(&req.provider)),
        };
        let provider = self
            .state
            .router
            .provider(&req.provider)
            .ok_or_else(|| self.unknown_provider(&req.provider))?;
        let opts = ProbeOptions::default();
        // A probe against a server that is down would record every capability as missing and
        // block plain chat until the next probe. Only record results from a reachable server.
        let checked = match ensure_reachable(provider.as_ref(), &opts).await {
            Ok(()) => {
                let report = probe_with(provider.as_ref(), &opts).await;
                if report.capabilities.streaming {
                    Ok(report)
                } else {
                    // The server may have gone away during the probe.
                    ensure_reachable(provider.as_ref(), &opts)
                        .await
                        .map(|()| report)
                }
            }
            Err(e) => Err(e),
        };
        let report = match checked {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(provider = %req.provider, error = %e, "probe not recorded: provider unreachable");
                self.audit(
                    "provider.probe",
                    req.provider.as_str(),
                    json!({"model": model, "error": e.to_string()}),
                )
                .await;
                return Err(RpcError::from(e));
            }
        };
        let caps = report.capabilities.clone();
        self.state
            .router
            .set_capabilities(&req.provider, caps.clone());
        tracing::info!(provider = %req.provider, ?caps, "probe finished");

        if let Some(cfg) = cfg {
            let probes = self.state.probes.clone();
            let caps = caps.clone();
            let persist = blocking(move || {
                probes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .put(&cfg, caps)
            })
            .await;
            if let Err(e) = persist {
                tracing::warn!(provider = %req.provider, error = %e, "cannot persist probe result");
            }
        }
        let checks: Vec<Value> = report
            .checks
            .iter()
            .map(|c| json!({"name": c.name, "passed": c.passed, "detail": c.detail, "elapsed_ms": c.elapsed_ms}))
            .collect();
        self.audit(
            "provider.probe",
            req.provider.as_str(),
            json!({"model": model, "capabilities": caps, "checks": checks}),
        )
        .await;
        to_result(caps)
    }

    // ---- chat ----

    async fn chat(&self, req: ipc::ChatRequest, notify: Notifier) -> Result<Value, RpcError> {
        if self.state.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let infer_req = to_infer_request(req.messages)?;
        let messages = infer_req.messages.len();
        // Open the stream first (with fallback across the tier's providers), so `ChatStarted`
        // names the provider that is actually serving it: the UI shows on-device vs cloud.
        let router = &self.state.router;
        let start = async {
            match &req.provider {
                // An explicit provider is used alone, never substituted.
                Some(id) => {
                    let p = router
                        .provider(id)
                        .ok_or_else(|| self.unknown_provider(id))?;
                    match p.chat(infer_req).await {
                        Ok(stream) => Ok((p, stream)),
                        Err(e) => {
                            if is_provider_failure(&e) {
                                router.report_failure(p.id());
                            }
                            Err(RpcError::from(e))
                        }
                    }
                }
                None => router
                    .chat(req.tier, &Capabilities::default(), infer_req)
                    .await
                    .map_err(RpcError::from),
            }
        };
        let (provider, stream) = tokio::select! {
            r = start => r?,
            () = self.state.shutdown.cancelled() => return Err(shutting_down()),
        };

        let stream_id = uuid::Uuid::new_v4().to_string();
        let meta = provider.meta();
        tracing::info!(stream = %stream_id, provider = %provider.id(), tier = tier_name(req.tier), "chat started");
        self.audit(
            "chat",
            provider.id().as_str(),
            json!({
                "stream_id": stream_id,
                "tier": req.tier,
                "explicit_provider": req.provider.is_some(),
                "model": meta.model,
                "locality": meta.locality,
                "messages": messages,
            }),
        )
        .await;

        // No `.await` between registering the token and handing its guard to the task: if this
        // call is dropped (client gone), the guard still removes the token.
        let token = CancellationToken::new();
        let guard = StreamGuard::register(self.state.clone(), stream_id.clone(), token.clone());
        let started = ChatStarted {
            stream_id: stream_id.clone(),
            provider: provider.id().clone(),
        };
        let state = self.state.clone();
        self.state.tasks.spawn(run_chat(
            state, provider, stream, stream_id, token, guard, notify,
        ));
        to_result(started)
    }

    fn chat_cancel(&self, req: ChatCancel) -> Result<Value, RpcError> {
        // Idempotent: cancelling a finished or unknown stream is not an error.
        match self.state.streams().get(&req.stream_id) {
            Some(token) => {
                tracing::info!(stream = %req.stream_id, "chat cancel requested");
                token.cancel();
            }
            None => tracing::debug!(stream = %req.stream_id, "cancel for unknown stream"),
        }
        Ok(Value::Null)
    }

    // ---- search / egress ----

    async fn search(&self, req: SearchRequest) -> Result<Value, RpcError> {
        let limit = req.limit.min(MAX_SEARCH_RESULTS) as usize;
        if limit == 0 || req.query.trim().is_empty() {
            return to_result(Vec::<SearchHit>::new());
        }
        let retriever = self.state.retriever.clone();
        let results =
            on_blocking_thread(async move { retriever.search(&req.query, limit).await }).await?;
        tracing::debug!(mode = ?results.mode, hits = results.items.len(), "search");
        to_result(results.hits())
    }

    async fn retrieve_context(&self, req: ContextRequest) -> Result<Value, RpcError> {
        let budget = req.budget_tokens.min(MAX_CONTEXT_TOKENS);
        if budget == 0 || req.query.trim().is_empty() {
            return to_result(ContextResponse {
                text: String::new(),
                citations: Vec::new(),
            });
        }
        let retriever = self.state.retriever.clone();
        let ctx =
            on_blocking_thread(async move { retriever.build_context(&req.query, budget).await })
                .await?;
        to_result(ctx)
    }

    // ---- sources ----

    async fn sources_add(&self, mut spec: SourceSpec) -> Result<Value, RpcError> {
        spec.settings = reconcile_settings(&spec.kind, spec.settings)?;
        let info = self.state.ingest.add(spec).await?;
        tracing::info!(source = %info.id, kind = %info.kind, "source added");
        self.audit(
            "source.add",
            info.id.as_str(),
            json!({"kind": info.kind, "name": info.name, "settings": info.settings}),
        )
        .await;
        to_result(info)
    }

    async fn sources_update(&self, mut update: SourceUpdate) -> Result<Value, RpcError> {
        if let Some(settings) = update.settings.take() {
            let kind = self.state.ingest.get(&update.id).await?.kind;
            update.settings = Some(reconcile_settings(&kind, settings)?);
        }
        let detail = json!({
            "name": update.name,
            "enabled": update.enabled,
            "settings": update.settings,
        });
        let info = self.state.ingest.update(update).await?;
        tracing::info!(source = %info.id, enabled = info.enabled, "source updated");
        self.audit("source.update", info.id.as_str(), detail).await;
        to_result(info)
    }

    async fn sources_remove(&self, req: SourceRemove) -> Result<Value, RpcError> {
        self.state.ingest.remove(&req.id, req.purge).await?;
        // Ingest deletes every account it recorded; make sure of the client secret anyway.
        let secrets = self.state.secrets.clone();
        let account = client_secret_account(&req.id);
        if let Err(e) = blocking(move || secrets.delete(&account)).await {
            tracing::warn!(source = %req.id, error = %e, "cannot delete the OAuth client secret");
        }
        tracing::info!(source = %req.id, purge = req.purge, "source removed");
        self.audit(
            "source.remove",
            req.id.as_str(),
            json!({"purge": req.purge}),
        )
        .await;
        Ok(Value::Null)
    }

    fn sources_sync(&self, req: SourceSync) -> Result<Value, RpcError> {
        self.state.ingest.sync_now(req.id.as_ref())?;
        Ok(Value::Null)
    }

    /// Start an OAuth flow and return its URL; the client opens it (the daemon never opens a
    /// browser). Ingest awaits the flow (abandoning it after 5 minutes) and emits
    /// `sources.changed` when it ends; this records the outcome in the audit log.
    async fn sources_auth_begin(&self, req: SourceRef) -> Result<Value, RpcError> {
        if self.state.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let flow = self.state.ingest.begin_auth(&req.id).await?;
        tracing::info!(source = %req.id, "authorization started");
        self.audit("source.auth.begin", req.id.as_str(), json!({}))
            .await;
        let state = self.state.clone();
        let id = req.id.clone();
        let done = flow.done;
        self.state.tasks.spawn(async move {
            let detail = tokio::select! {
                () = state.shutdown.cancelled() => json!({"result": "cancelled", "reason": "shutdown"}),
                r = tokio::time::timeout(AUTH_RESULT_WAIT, done) => match r {
                    Ok(Ok(())) => json!({"result": "authorized"}),
                    Ok(Err(e)) => json!({"result": "failed", "error": e.to_string()}),
                    Err(_) => json!({"result": "failed", "error": "timed out"}),
                },
            };
            tracing::info!(source = %id, result = %detail["result"], "authorization finished");
            append_audit(&state, "source.auth.result", id.as_str(), detail).await;
        });
        to_result(AuthBegin { url: flow.url })
    }

    /// Store a source credential (see `crate::sources` for where each kind's goes). The value
    /// is never echoed, logged or audited.
    async fn sources_secret_set(&self, req: SourceSecret) -> Result<Value, RpcError> {
        if req.secret.is_empty() {
            return Err(RpcError::invalid_params("the secret must not be empty"));
        }
        if req.secret.len() > MAX_SECRET_BYTES {
            return Err(RpcError::invalid_params("the secret is too long"));
        }
        let ingest = &self.state.ingest;
        let kind = ingest.get(&req.id).await?.kind;
        let account = match crate::sources::auth_kind(ingest, &kind) {
            Some(AuthKind::Password) => {
                ingest.set_password(&req.id, &req.secret).await?;
                nugget_core::secrets::password_account(req.id.as_str())
            }
            Some(AuthKind::OAuth) => {
                let account = client_secret_account(&req.id);
                ingest.set_secret(&req.id, &account, &req.secret).await?;
                // The connector reads it from its settings at build time: rebuild.
                ingest.reload(&req.id).await?;
                account
            }
            Some(AuthKind::None) | None => {
                return Err(RpcError::invalid_params(format!(
                    "`{kind}` sources take no secret"
                )));
            }
        };
        tracing::info!(source = %req.id, %account, "source secret stored");
        self.audit(
            "source.secret.set",
            req.id.as_str(),
            json!({"account": account}),
        )
        .await;
        Ok(Value::Null)
    }

    async fn egress_recent(&self, req: EgressRecentRequest) -> Result<Value, RpcError> {
        let limit = req.limit.min(MAX_EGRESS_EVENTS) as usize;
        let store = self.state.store.clone();
        let events = blocking(move || store.recent_egress(limit)).await?;
        to_result(events)
    }

    // ---- models ----

    async fn models_list(&self) -> Result<Value, RpcError> {
        let manifest = self.manifest()?;
        let models = self.state.models.clone();
        let installed = blocking(move || models.installed()).await?;
        let hw = self.state.hardware;
        let recommended: HashSet<&str> = ALL_TIERS
            .iter()
            .filter_map(|t| recommend(manifest, &hw, *t))
            .map(|e| e.id.as_str())
            .collect();
        let list: Vec<ModelInfo> = manifest
            .models
            .iter()
            .map(|e| {
                let slot = installed.get(&e.id);
                ModelInfo {
                    id: e.id.clone(),
                    size_bytes: e.size,
                    installed: slot.is_some(),
                    recommended: recommended.contains(e.id.as_str()),
                    family: e.family.clone(),
                    tiers: e.tiers.clone(),
                    license: e.license.clone(),
                    revision: e.revision.clone(),
                    update_available: slot.is_some_and(|s| s.active.sha256 != e.sha256),
                }
            })
            .collect();
        to_result(list)
    }

    async fn models_install(
        &self,
        req: ModelInstallRequest,
        notify: Notifier,
    ) -> Result<Value, RpcError> {
        let entry = self.manifest()?.get(&req.id).cloned().ok_or_else(|| {
            RpcError::from(Error::NotFound(format!(
                "model `{}` is not in the manifest",
                req.id
            )))
        })?;
        let guard = self.state.installs.begin(&entry.id).ok_or_else(|| {
            RpcError::new(
                code::SERVER_ERROR,
                format!("model `{}` is already being installed", entry.id),
            )
        })?;
        tracing::info!(model = %entry.id, size = entry.size, "model install started");
        self.audit(
            "model.install",
            &entry.id,
            json!({"revision": entry.revision, "sha256": entry.sha256, "size": entry.size}),
        )
        .await;
        let state = self.state.clone();
        self.state
            .tasks
            .spawn(run_install(state, entry, guard, notify));
        Ok(Value::Null)
    }

    fn manifest(&self) -> Result<&nugget_models::Manifest, RpcError> {
        self.state.manifest.as_ref().map_err(|e| {
            RpcError::from(Error::Integrity(format!("model manifest unavailable: {e}")))
        })
    }

    async fn audit(&self, action: &str, target: &str, detail: Value) {
        append_audit(&self.state, action, target, detail).await;
    }

    /// Not found, or found in config but unavailable (with the reason).
    fn unknown_provider(&self, id: &ProviderId) -> RpcError {
        match self.state.unavailable.get(id) {
            Some(reason) => RpcError::from(Error::NotFound(format!(
                "provider `{id}` is unavailable: {reason}"
            ))),
            None => not_configured(id),
        }
    }
}

/// A provider that failed to build: what config says about it, plus why.
fn unavailable_info(cfg: &ProviderConfig, reason: &str) -> ProviderInfo {
    let mut tiers = cfg.tiers.clone();
    tiers.sort_by_key(|t| *t as u8);
    tiers.dedup();
    ProviderInfo {
        id: cfg.id.clone(),
        kind: cfg.kind.as_str().to_string(),
        model: cfg.model.clone(),
        // If the URL cannot even be classified, assume the worst for privacy.
        locality: cfg.effective_locality().unwrap_or(Locality::Cloud),
        tiers,
        capabilities: None,
        error: Some(reason.to_string()),
    }
}

/// Check that a provider's server answers at all, with one small chat. Only transport-level
/// failures (and timeouts) count as unreachable: any answer from the server, even an error
/// status, means it is up.
async fn ensure_reachable(provider: &dyn Provider, opts: &ProbeOptions) -> Result<()> {
    let req = ChatRequest::new(vec![Message::user_text("Reply with one word: pong")])
        .with_max_tokens(16)
        .with_feature(FEATURE_PROBE);
    let attempt = async {
        let mut stream = provider.chat(req).await?;
        match stream.next().await {
            Some(Err(e)) => Err(e),
            _ => Ok(()),
        }
    };
    let unreachable = |why: String| {
        Error::Network(format!(
            "provider `{}` is unreachable ({why}); probe results were not recorded",
            provider.id()
        ))
    };
    match tokio::time::timeout(opts.timeout, attempt).await {
        Err(_) => Err(unreachable(format!("no answer within {:?}", opts.timeout))),
        Ok(Err(e)) if is_transport_failure(&e) => Err(unreachable(e.to_string())),
        Ok(_) => Ok(()),
    }
}

/// Failures that say the server could not be reached or would not serve right now.
fn is_transport_failure(e: &Error) -> bool {
    matches!(
        e,
        Error::Network(_) | Error::RateLimited { .. } | Error::Io(_)
    )
}

/// Run `fut` on a blocking-pool thread (driving it with the runtime's handle), for work that
/// makes synchronous SQLite calls between awaits, like retrieval.
async fn on_blocking_thread<T, F>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    blocking(move || handle.block_on(fut)).await
}

fn shutting_down() -> RpcError {
    RpcError::new(code::SERVER_ERROR, "nuggetd is shutting down")
}

async fn append_audit(state: &State, action: &str, target: &str, detail: Value) {
    let store = state.store.clone();
    let entry = AuditEntry {
        actor: "user".into(),
        action: action.into(),
        target: target.into(),
        detail,
    };
    if let Err(e) = blocking(move || store.append_audit(&entry)).await {
        tracing::warn!(action, error = %e, "cannot append audit entry");
    }
}

fn not_configured(id: &ProviderId) -> RpcError {
    RpcError::from(Error::NotFound(format!(
        "provider `{id}` is not configured or not available"
    )))
}

/// IPC chat messages → a provider request. System messages become the system prompt.
fn to_infer_request(messages: Vec<ipc::ChatMessage>) -> Result<ChatRequest, RpcError> {
    let mut system = Vec::new();
    let mut turns = Vec::new();
    for m in messages {
        match m.role {
            IpcRole::System => system.push(m.text),
            IpcRole::User => turns.push(Message::user_text(m.text)),
            IpcRole::Assistant => turns.push(Message::assistant_text(m.text)),
        }
    }
    match turns.last() {
        None => {
            return Err(RpcError::invalid_params(
                "a chat needs at least one user message",
            ));
        }
        Some(last) if last.role != nugget_infer::Role::User => {
            return Err(RpcError::invalid_params(
                "the last chat message must be from the user",
            ));
        }
        Some(_) => {}
    }
    let mut req = ChatRequest::new(turns);
    if !system.is_empty() {
        req.system = Some(system.join("\n\n"));
    }
    Ok(req)
}

// ---- background tasks ----

/// How a chat stream ended.
enum ChatEnd {
    Done {
        stop_reason: StopReason,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    },
    Failed(Error),
    Cancelled,
    Shutdown,
    Disconnected,
}

/// Registers a stream's cancellation token; removes it when dropped.
struct StreamGuard {
    state: Arc<State>,
    id: String,
}

impl StreamGuard {
    fn register(state: Arc<State>, id: String, token: CancellationToken) -> Self {
        state.streams().insert(id.clone(), token);
        Self { state, id }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.state.streams().remove(&self.id);
    }
}

async fn run_chat(
    state: Arc<State>,
    provider: Arc<dyn Provider>,
    stream: ChatStream,
    stream_id: String,
    token: CancellationToken,
    _guard: StreamGuard,
    notify: Notifier,
) {
    // Dropping `pump` drops the provider stream, which closes the HTTP request.
    let end = tokio::select! {
        biased;
        () = token.cancelled() => ChatEnd::Cancelled,
        () = state.shutdown.cancelled() => ChatEnd::Shutdown,
        () = notify.closed() => ChatEnd::Disconnected,
        end = pump(stream, provider.id(), &stream_id, &notify) => end,
    };

    let mut done = ChatDone {
        stream_id: stream_id.clone(),
        error: None,
        input_tokens: None,
        output_tokens: None,
    };
    match end {
        ChatEnd::Done {
            stop_reason,
            input_tokens,
            output_tokens,
        } => {
            state.router.report_success(provider.id());
            done.input_tokens = input_tokens;
            done.output_tokens = output_tokens;
            done.error = match stop_reason {
                StopReason::Refusal => Some("the model declined to answer".into()),
                StopReason::ContentFilter => Some("the provider filtered the output".into()),
                _ => None,
            };
            tracing::info!(stream = %stream_id, ?input_tokens, ?output_tokens, "chat finished");
        }
        ChatEnd::Failed(e) => {
            if is_provider_failure(&e) {
                state.router.report_failure(provider.id());
            }
            tracing::warn!(stream = %stream_id, provider = %provider.id(), error = %e, "chat failed");
            done.error = Some(e.to_string());
        }
        ChatEnd::Cancelled => {
            tracing::info!(stream = %stream_id, "chat cancelled");
            done.error = Some("cancelled".into());
        }
        ChatEnd::Shutdown => {
            tracing::info!(stream = %stream_id, "chat stopped for shutdown");
            done.error = Some("nuggetd is shutting down".into());
        }
        ChatEnd::Disconnected => {
            tracing::info!(stream = %stream_id, "client disconnected; chat stopped");
            return;
        }
    }
    let _ = notify.send(notification::CHAT_DONE, &done).await;
}

/// Forward an open provider stream as `chat.delta` notifications until it ends.
async fn pump(
    mut stream: ChatStream,
    provider: &ProviderId,
    stream_id: &str,
    notify: &Notifier,
) -> ChatEnd {
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ChatEvent::TextDelta(text)) if !text.is_empty() => {
                let delta = ChatDelta {
                    stream_id: stream_id.to_string(),
                    text,
                };
                if notify.send(notification::CHAT_DELTA, &delta).await.is_err() {
                    return ChatEnd::Disconnected;
                }
            }
            Ok(ChatEvent::Done {
                stop_reason,
                input_tokens,
                output_tokens,
            }) => {
                return ChatEnd::Done {
                    stop_reason,
                    input_tokens,
                    output_tokens,
                };
            }
            // No tools over IPC chat yet; opaque replay state is only useful to agent loops.
            Ok(_) => {}
            Err(e) => return ChatEnd::Failed(e),
        }
    }
    ChatEnd::Failed(Error::Provider(format!(
        "{provider}: stream ended before completion"
    )))
}

/// Failures that say something about the provider's health (so the router tries others first
/// for a while), as opposed to a bad request.
fn is_provider_failure(e: &Error) -> bool {
    matches!(
        e,
        Error::Provider(_)
            | Error::Network(_)
            | Error::RateLimited { .. }
            | Error::Io(_)
            | Error::Other(_)
    )
}

async fn run_install(
    state: Arc<State>,
    entry: ModelEntry,
    _guard: InFlightGuard,
    notify: Notifier,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelProgress>();
    let models = state.models.clone();
    let install_entry = entry.clone();
    // Dropping this future cancels the install; staged bytes are kept for a later resume.
    let install = async move {
        models
            .install(&install_entry, move |p| {
                let _ = tx.send(ModelProgress::from(&p));
            })
            .await
    };
    tokio::pin!(install);

    let mut result: Option<Result<InstalledModel>> = None;
    let mut stop_reason: Option<&str> = None;
    loop {
        tokio::select! {
            biased;
            () = state.shutdown.cancelled() => {
                stop_reason = Some("nuggetd is shutting down");
                break;
            }
            () = notify.closed() => {
                stop_reason = Some("client disconnected");
                break;
            }
            Some(p) = rx.recv() => {
                if notify.send(notification::MODELS_PROGRESS, &p).await.is_err() {
                    stop_reason = Some("client disconnected");
                    break;
                }
            }
            r = &mut install => {
                result = Some(r);
                break;
            }
        }
    }

    // The final `installed`/`failed` progress is queued before `install` returns.
    while let Ok(p) = rx.try_recv() {
        if notify
            .send(notification::MODELS_PROGRESS, &p)
            .await
            .is_err()
        {
            break;
        }
    }

    let detail = match (&result, stop_reason) {
        (Some(Ok(m)), _) => {
            tracing::info!(model = %entry.id, sha256 = %m.sha256, "model install finished");
            if let Some(local) = &state.local {
                for slot in local.slots.iter().filter(|s| s.model().id == entry.id) {
                    slot.set_installed(true);
                    spawn_ensure(&state, slot.clone());
                }
            }
            json!({"result": "installed", "sha256": m.sha256, "revision": m.revision})
        }
        (Some(Err(e)), _) => {
            tracing::warn!(model = %entry.id, error = %e, "model install failed");
            json!({"result": "failed", "error": e.to_string()})
        }
        (None, reason) => {
            let reason = reason.unwrap_or("stopped");
            tracing::info!(model = %entry.id, reason, "model install stopped; staged data kept for resume");
            let _ = notify
                .send(
                    notification::MODELS_PROGRESS,
                    &ModelProgress {
                        id: entry.id.clone(),
                        downloaded: state.models.staged_bytes(&entry),
                        total: entry.size,
                        phase: ModelPhase::Failed,
                        error: Some(format!("cancelled: {reason}")),
                    },
                )
                .await;
            json!({"result": "cancelled", "reason": reason})
        }
    };
    append_audit(&state, "model.install.result", &entry.id, detail).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ipc::ChatMessage;

    fn msg(role: IpcRole, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            text: text.into(),
        }
    }

    #[test]
    fn system_messages_become_the_system_prompt() {
        let req = to_infer_request(vec![
            msg(IpcRole::System, "be brief"),
            msg(IpcRole::User, "hi"),
            msg(IpcRole::Assistant, "hello"),
            msg(IpcRole::System, "no emoji"),
            msg(IpcRole::User, "bye"),
        ])
        .unwrap();
        assert_eq!(req.system.as_deref(), Some("be brief\n\nno emoji"));
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[1], Message::assistant_text("hello"));
        assert_eq!(req.feature, nugget_infer::FEATURE_CHAT);
    }

    #[test]
    fn rejects_chats_without_a_final_user_message() {
        for msgs in [
            vec![],
            vec![msg(IpcRole::System, "x")],
            vec![msg(IpcRole::User, "q"), msg(IpcRole::Assistant, "a")],
        ] {
            let err = to_infer_request(msgs).unwrap_err();
            assert_eq!(err.code, code::INVALID_PARAMS);
        }
    }

    #[test]
    fn provider_failures_vs_request_errors() {
        assert!(is_provider_failure(&Error::Network("x".into())));
        assert!(is_provider_failure(&Error::RateLimited {
            retry_after_ms: None
        }));
        assert!(!is_provider_failure(&Error::Invalid("x".into())));
        assert!(!is_provider_failure(&Error::Cancelled));
    }
}
