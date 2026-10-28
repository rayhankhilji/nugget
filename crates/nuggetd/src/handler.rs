//! The IPC [`Handler`]: one function per `nugget_core::ipc::method`.

use std::collections::HashSet;
use std::sync::{Arc, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    self, ChatCancel, ChatDelta, ChatDone, ChatStarted, EgressRecentRequest, ModelInfo,
    ModelInstallRequest, ModelPhase, ModelProgress, PROTOCOL_VERSION, ProbeRequest, ProviderInfo,
    Role as IpcRole, SearchHit, SearchRequest, StatusResponse, method, notification,
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
use crate::daemon::{InFlightGuard, State, blocking};

/// Upper bounds for list-style requests.
const MAX_SEARCH_RESULTS: u32 = 200;
const MAX_EGRESS_EVENTS: u32 = 1000;

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
            other => Err(RpcError::method_not_found(other)),
        }
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
            .collect()
    }

    async fn probe(&self, req: ProbeRequest) -> Result<Value, RpcError> {
        let cfg = self
            .state
            .config_of(&req.provider)
            .cloned()
            .ok_or_else(|| self.unknown_provider(&req.provider))?;
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
                    json!({"model": cfg.model, "error": e.to_string()}),
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

        let probes = self.state.probes.clone();
        let persist = {
            let cfg = cfg.clone();
            let caps = caps.clone();
            blocking(move || {
                probes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .put(&cfg, caps)
            })
            .await
        };
        if let Err(e) = persist {
            tracing::warn!(provider = %req.provider, error = %e, "cannot persist probe result");
        }
        let checks: Vec<Value> = report
            .checks
            .iter()
            .map(|c| json!({"name": c.name, "passed": c.passed, "detail": c.detail, "elapsed_ms": c.elapsed_ms}))
            .collect();
        self.audit(
            "provider.probe",
            req.provider.as_str(),
            json!({"model": cfg.model, "capabilities": caps, "checks": checks}),
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
        let store = self.state.store.clone();
        let hits = blocking(move || store.search_text(&req.query, limit)).await?;
        to_result(hits.into_iter().map(SearchHit::from).collect::<Vec<_>>())
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
