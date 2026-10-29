//! Double Tap drafting: `draft.begin` / `draft.cancel` / `draft.feedback` / `draft.prewarm` /
//! `voice.status`, over [`nugget_draft::Drafter`] and the user's [`nugget_voice`] profile.
//!
//! - **Streaming.** `draft.begin` answers [`DraftStarted`], then sends the post-processed draft
//!   as `chat.delta` notifications and one `chat.done` (same shapes as `retrieve.ask`) on the
//!   requesting connection. The stream stops on `draft.cancel` (`chat.done.error` =
//!   `"cancelled"`), on daemon shutdown (`"nuggetd is shutting down"`) and silently when the
//!   connection closes.
//! - **Refusals.** Secure fields, excluded apps and search boxes are refused before anything is
//!   read or sent, with a stable error: `data.refused` is `"secure"`, `"excluded_app"` or
//!   `"not_draftable"` or `"terminal"` (`"disabled"` when `[draft] enabled = false`). See [`refusal_error`].
//! - **Voice.** The profile is loaded from its encrypted snapshot (or built) on a blocking
//!   thread after startup, then refreshed incrementally after ingest commits and draft
//!   feedback, debounced ([`voice`]).
//! - **Audit.** Every refusal, start (by the engine), cancel, end and feedback is audited with
//!   lengths, kinds, provider, locality and app id only: never field text, thread or draft.
//!
//! Config, `[draft]` (all optional):
//! ```toml
//! [draft]
//! enabled = true                  # false: draft.begin / draft.prewarm refuse ("disabled")
//! tier = "synthesis"              # routing tier when neither request nor app names a provider
//! budget_tokens = 1500            # retrieval budget, capped at nugget_draft::MAX_BUDGET_TOKENS
//! app_providers = { "com.tinyspeck.slackmacgap" = "local-chat", "mail.google.com" = "remote" }
//! excluded_apps = ["com.example.bank"]   # ADDED to the built-in password-manager list...
//! replace_default_excluded = false       # ...unless this is true (then the list is used alone)
//! allow_terminals = false         # terminal emulators are refused ("terminal") unless true
//! allow_cloud_screenshots = false
//! temperature = 0.7
//! retrieval_timeout_ms = 1500
//! voice_timeout_ms = 500
//! prewarm_ttl_ms = 30000
//! voice_refresh_debounce_ms = 5000      # quiet time after ingest commits before a refresh
//!
//! [draft.voice]
//! emails = ["me@example.com"]     # who "me" is (completed from sent mail when infer_me)
//! names = ["Alex Morgan"]
//! infer_me = true
//! include_notes = true
//! sent_folders = []
//! min_channel_samples = 5
//! max_exemplars = 6
//! ```

pub(super) mod voice;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use nugget_core::capability::Tier;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    ChatCancel, ChatDelta, ChatDone, DraftContext, DraftFeedback, DraftRequest, method,
    notification,
};
use nugget_core::{Error, Result};
use nugget_draft::{
    DEFAULT_EXCLUDED_APPS, DraftConfig, DraftStream, Drafter, MAX_BUDGET_TOKENS, ProfileVoice,
    Refusal, RetrievalRunner, VoiceFeedback, Warmer, gate_with,
};
use nugget_infer::{ChatEvent, Provider, StopReason};
use nugget_ipc::{Notifier, RpcError, code, parse_params, to_result};
use nugget_retrieve::PackedContext;
use nugget_store::AuditEntry;
use nugget_voice::{MeIdentity, StoreFeedbackSink, VoiceConfig, VoiceSource as _};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Feature, FeatureContext, section};
use crate::daemon::{State, blocking};
use voice::SharedVoice;

use nugget_core::ipc::method::DRAFT_PREWARM;

const METHODS: &[&str] = &[
    method::DRAFT_BEGIN,
    method::DRAFT_CANCEL,
    method::DRAFT_FEEDBACK,
    method::VOICE_STATUS,
    DRAFT_PREWARM,
];

/// `[draft]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct DraftSection {
    enabled: bool,
    tier: Tier,
    budget_tokens: u32,
    app_providers: HashMap<String, ProviderId>,
    /// Added to [`DEFAULT_EXCLUDED_APPS`], unless `replace_default_excluded`.
    excluded_apps: Vec<String>,
    replace_default_excluded: bool,
    /// Draft into terminal emulators too (refused by default: text there can run as a command).
    allow_terminals: bool,
    allow_cloud_screenshots: bool,
    temperature: Option<f32>,
    retrieval_timeout_ms: u64,
    voice_timeout_ms: u64,
    prewarm_ttl_ms: u64,
    voice_refresh_debounce_ms: u64,
    voice: VoiceSection,
}

impl Default for DraftSection {
    fn default() -> Self {
        let d = DraftConfig::default();
        Self {
            enabled: true,
            tier: d.tier,
            budget_tokens: d.budget_tokens,
            app_providers: HashMap::new(),
            excluded_apps: Vec::new(),
            replace_default_excluded: false,
            allow_terminals: d.allow_terminals,
            allow_cloud_screenshots: d.allow_cloud_screenshots,
            temperature: d.temperature,
            retrieval_timeout_ms: d.retrieval_timeout_ms,
            voice_timeout_ms: d.voice_timeout_ms,
            prewarm_ttl_ms: d.prewarm_ttl_ms,
            voice_refresh_debounce_ms: 5_000,
            voice: VoiceSection::default(),
        }
    }
}

/// `[draft.voice]`: who "me" is, and profile tuning.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct VoiceSection {
    emails: Vec<String>,
    names: Vec<String>,
    infer_me: bool,
    include_notes: bool,
    sent_folders: Vec<String>,
    min_channel_samples: u32,
    max_exemplars: usize,
}

impl Default for VoiceSection {
    fn default() -> Self {
        let d = VoiceConfig::default();
        Self {
            emails: Vec::new(),
            names: Vec::new(),
            infer_me: d.infer_me,
            include_notes: d.include_notes,
            sent_folders: d.sent_folders,
            min_channel_samples: d.min_channel_samples,
            max_exemplars: d.max_exemplars,
        }
    }
}

impl DraftSection {
    fn draft_config(&self) -> DraftConfig {
        let mut excluded: Vec<String> = if self.replace_default_excluded {
            Vec::new()
        } else {
            DEFAULT_EXCLUDED_APPS
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        };
        for e in &self.excluded_apps {
            let e = e.trim();
            if !e.is_empty() && !excluded.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                excluded.push(e.to_owned());
            }
        }
        DraftConfig {
            tier: self.tier,
            budget_tokens: self.budget_tokens.clamp(1, MAX_BUDGET_TOKENS),
            app_providers: self.app_providers.clone(),
            allow_cloud_screenshots: self.allow_cloud_screenshots,
            allow_terminals: self.allow_terminals,
            excluded_apps: excluded,
            temperature: self.temperature,
            retrieval_timeout_ms: self.retrieval_timeout_ms,
            voice_timeout_ms: self.voice_timeout_ms,
            prewarm_ttl_ms: self.prewarm_ttl_ms,
            ..DraftConfig::default()
        }
    }

    fn me(&self) -> MeIdentity {
        MeIdentity::new(self.voice.emails.iter(), self.voice.names.iter())
    }

    fn voice_config(&self) -> VoiceConfig {
        VoiceConfig {
            infer_me: self.voice.infer_me,
            include_notes: self.voice.include_notes,
            sent_folders: self.voice.sent_folders.clone(),
            min_channel_samples: self.voice.min_channel_samples,
            max_exemplars: self.voice.max_exemplars.max(1),
            min_exemplars: VoiceConfig::default()
                .min_exemplars
                .min(self.voice.max_exemplars.max(1)),
            ..VoiceConfig::default()
        }
    }
}

/// Opens the provider's connection (and starts a stopped local server) on trigger.
struct ProviderWarmer;

#[async_trait]
impl Warmer for ProviderWarmer {
    async fn warm(&self, provider: Arc<dyn Provider>) {
        if let Err(e) = provider.prewarm().await {
            tracing::debug!(provider = %provider.id(), error = %e, "draft: prewarm failed");
        }
    }
}

/// Runs draft retrieval (synchronous SQLite calls between awaits) on a blocking-pool thread,
/// driven by the runtime's handle, as `retrieve.*` does, so a slow store never stalls the async
/// workers that stream drafts and answer `draft.cancel`.
struct BlockingRetrieval;

#[async_trait]
impl RetrievalRunner for BlockingRetrieval {
    async fn run(&self, job: BoxFuture<'static, Result<PackedContext>>) -> Result<PackedContext> {
        let handle = tokio::runtime::Handle::current();
        blocking(move || handle.block_on(job)).await
    }
}

/// The `draft` feature.
pub(crate) struct DraftFeature {
    state: Arc<State>,
    drafter: Drafter,
    voice: Arc<SharedVoice>,
    enabled: bool,
}

/// Start the feature: parse `[draft]`, build the drafter, and load the voice profile in the
/// background.
pub(crate) async fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let cfg: DraftSection = section("draft", &cx.config.draft)?;
    Ok(start_with(cx.state, &cfg))
}

fn start_with(state: &Arc<State>, cfg: &DraftSection) -> Arc<dyn Feature> {
    let voice = Arc::new(SharedVoice::default());
    let drafter = Drafter::new(
        state.store.clone(),
        state.retriever.clone(),
        state.router.clone(),
        Arc::new(ProfileVoice::from_arc(voice.clone())),
        cfg.draft_config(),
    )
    .with_feedback_sink(Arc::new(VoiceFeedback::new(StoreFeedbackSink::new(
        state.store.clone(),
    ))))
    .with_warmer(Arc::new(ProviderWarmer))
    .with_retrieval_runner(Arc::new(BlockingRetrieval));

    // Subscribe before anything can commit, so no change is missed while the profile loads.
    let events = state.ingest.subscribe();
    state.tasks.spawn(voice::run(
        voice.clone(),
        state.store.clone(),
        cfg.me(),
        cfg.voice_config(),
        events,
        Duration::from_millis(cfg.voice_refresh_debounce_ms),
        state.shutdown.clone(),
    ));
    tracing::info!(
        enabled = cfg.enabled,
        excluded_apps = drafter.config().excluded_apps.len(),
        app_providers = drafter.config().app_providers.len(),
        "draft feature started"
    );
    Arc::new(DraftFeature {
        state: state.clone(),
        drafter,
        voice,
        enabled: cfg.enabled,
    })
}

#[async_trait]
impl Feature for DraftFeature {
    fn name(&self) -> &'static str {
        "draft"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(&self, m: &str, params: Value, notify: Notifier) -> Result<Value, RpcError> {
        match m {
            method::DRAFT_BEGIN => self.begin(parse_params(params)?, notify).await,
            method::DRAFT_CANCEL => self.cancel(parse_params(params)?).await,
            method::DRAFT_FEEDBACK => self.feedback(parse_params(params)?).await,
            method::VOICE_STATUS => self.voice_status().await,
            DRAFT_PREWARM => self.prewarm(parse_params(params)?),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

/// The stable error for a refused draft. `code` is `INVALID_PARAMS` for secure fields and
/// excluded apps, `UNSUPPORTED` for fields nothing is drafted into and when drafting is
/// disabled; `data.refused` names the reason for the desktop to map.
fn refusal_error(r: &Refusal) -> RpcError {
    let (c, reason) = refusal_kind(r);
    let mut data = json!({ "refused": reason });
    if let Refusal::NotDraftable(kind) = r {
        data["field_kind"] = json!(kind.as_str());
    }
    RpcError::new(c, r.to_string()).with_data(data)
}

fn refusal_kind(r: &Refusal) -> (i64, &'static str) {
    match r {
        Refusal::SecureField => (code::INVALID_PARAMS, "secure"),
        Refusal::ExcludedApp(_) => (code::INVALID_PARAMS, "excluded_app"),
        Refusal::Terminal(_) => (code::INVALID_PARAMS, "terminal"),
        Refusal::NotDraftable(_) => (code::UNSUPPORTED, "not_draftable"),
    }
}

fn disabled_error() -> RpcError {
    RpcError::new(
        code::UNSUPPORTED,
        "Double Tap drafting is disabled ([draft] enabled = false)",
    )
    .with_data(json!({ "refused": "disabled" }))
}

fn shutting_down() -> RpcError {
    RpcError::new(code::SERVER_ERROR, "nuggetd is shutting down")
}

impl DraftFeature {
    async fn begin(&self, req: DraftRequest, notify: Notifier) -> Result<Value, RpcError> {
        if self.state.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let app = req.context.app.id.clone();
        if !self.enabled {
            self.audit_refusal("disabled", app.as_deref(), None).await;
            return Err(disabled_error());
        }
        // The engine gates again; gating here first keeps the refusal's kind for the client
        // and the audit, and nothing about the field is read past this point when refused.
        let config = self.drafter.config();
        if let Err(r) = gate_with(&req.context, &config.excluded_apps, config.allow_terminals) {
            let kind = match &r {
                Refusal::NotDraftable(k) => Some(k.as_str()),
                _ => None,
            };
            self.audit_refusal(refusal_kind(&r).1, app.as_deref(), kind)
                .await;
            return Err(refusal_error(&r));
        }
        let begin = self.drafter.begin(req);
        let (started, stream) = tokio::select! {
            r = begin => match r {
                Ok(s) => s,
                Err(e) => {
                    let err = RpcError::from(e);
                    audit(
                        &self.state,
                        "user",
                        "draft.failed",
                        app.as_deref().unwrap_or(""),
                        json!({ "app": app, "code": err.code }),
                    )
                    .await;
                    return Err(err);
                }
            },
            () = self.state.shutdown.cancelled() => return Err(shutting_down()),
        };
        // The engine audited `draft.begin` (stream, kind, app, provider, locality, passages).
        let state = self.state.clone();
        let provider = started.provider.clone();
        let stream_id = started.stream_id.clone();
        self.state
            .tasks
            .spawn(run_draft(state, provider, stream, stream_id, notify));
        to_result(started)
    }

    async fn cancel(&self, req: ChatCancel) -> Result<Value, RpcError> {
        // Idempotent: cancelling a finished or unknown draft is not an error.
        let running = self.drafter.cancel(&req.stream_id);
        tracing::info!(stream = %req.stream_id, running, "draft cancel requested");
        audit(
            &self.state,
            "user",
            "draft.cancel",
            &req.stream_id,
            json!({ "running": running }),
        )
        .await;
        Ok(Value::Null)
    }

    async fn feedback(&self, fb: DraftFeedback) -> Result<Value, RpcError> {
        // Accepted and edited drafts may become samples of the user's voice.
        let stores_sample = matches!(fb.outcome.as_str(), "accepted" | "edited");
        // Audited (no text) by the engine; the final text goes to the voice profile's store.
        self.drafter.feedback(fb).await?;
        if stores_sample {
            self.voice.kick();
        }
        Ok(Value::Null)
    }

    async fn voice_status(&self) -> Result<Value, RpcError> {
        // The profile's lock can be held by a refresh: wait off the async workers.
        let voice = self.voice.clone();
        to_result(blocking(move || Ok(voice.status())).await?)
    }

    fn prewarm(&self, hint: DraftContext) -> Result<Value, RpcError> {
        if !self.enabled || self.state.shutdown.is_cancelled() {
            return Ok(Value::Null);
        }
        match self.drafter.prewarm(&hint) {
            Some(p) => Ok(json!({
                "provider": p.provider,
                "locality": p.locality,
                "retrieval": p.retrieval,
            })),
            None => Ok(Value::Null),
        }
    }

    async fn audit_refusal(&self, reason: &str, app: Option<&str>, field_kind: Option<&str>) {
        tracing::info!(reason, app, "draft refused");
        audit(
            &self.state,
            "user",
            "draft.refused",
            app.unwrap_or(""),
            json!({ "reason": reason, "app": app, "field_kind": field_kind }),
        )
        .await;
    }
}

/// Append an audit entry (no content: callers pass ids, kinds and counts only).
async fn audit(state: &State, actor: &str, action: &str, target: &str, detail: Value) {
    let store = state.store.clone();
    let entry = AuditEntry {
        actor: actor.into(),
        action: action.into(),
        target: target.into(),
        detail,
    };
    if let Err(e) = blocking(move || store.append_audit(&entry)).await {
        tracing::warn!(action, error = %e, "cannot append audit entry");
    }
}

/// How a draft stream ended.
enum End {
    Done(StopReason),
    Failed(Error),
    Cancelled,
    Shutdown,
    Disconnected,
}

impl End {
    fn as_str(&self) -> &'static str {
        match self {
            End::Done(_) => "done",
            End::Failed(_) => "failed",
            End::Cancelled => "cancelled",
            End::Shutdown => "shutdown",
            End::Disconnected => "disconnected",
        }
    }
}

/// Forward a draft as `chat.delta` notifications, then one `chat.done`, and audit its end.
async fn run_draft(
    state: Arc<State>,
    provider: ProviderId,
    mut stream: DraftStream,
    stream_id: String,
    notify: Notifier,
) {
    let mut chars = 0usize;
    let mut done = ChatDone {
        stream_id: stream_id.clone(),
        error: None,
        input_tokens: None,
        output_tokens: None,
    };
    let end = tokio::select! {
        biased;
        () = state.shutdown.cancelled() => End::Shutdown,
        () = notify.closed() => End::Disconnected,
        end = pump(&mut stream, &stream_id, &notify, &mut chars, &mut done) => end,
    };
    let timings = stream.timings();
    // Dropping the stream cancels the draft and closes the provider request.
    drop(stream);

    match &end {
        End::Done(stop) => {
            done.error = match stop {
                StopReason::Refusal => Some("the model declined to answer".into()),
                StopReason::ContentFilter => Some("the provider filtered the output".into()),
                _ => None,
            };
        }
        End::Failed(e) => {
            if matches!(
                e,
                Error::Provider(_)
                    | Error::Network(_)
                    | Error::RateLimited { .. }
                    | Error::Io(_)
                    | Error::Other(_)
            ) {
                state.router.report_failure(&provider);
            }
            tracing::warn!(stream = %stream_id, %provider, error = %e, "draft failed");
            done.error = Some(e.to_string());
        }
        End::Cancelled => done.error = Some("cancelled".into()),
        End::Shutdown => done.error = Some("nuggetd is shutting down".into()),
        End::Disconnected => {}
    }
    tracing::info!(stream = %stream_id, end = end.as_str(), chars, ?timings, "draft finished");
    audit(
        &state,
        "daemon",
        "draft.end",
        &stream_id,
        json!({
            "provider": provider,
            "end": end.as_str(),
            "chars": chars,
            "first_token_ms": timings.first_token_ms,
            "total_ms": timings.total_ms,
            "prewarmed": timings.prewarmed,
            "attempts": timings.attempts,
        }),
    )
    .await;
    if !matches!(end, End::Disconnected) {
        let _ = notify.send(notification::CHAT_DONE, &done).await;
    }
}

async fn pump(
    stream: &mut DraftStream,
    stream_id: &str,
    notify: &Notifier,
    chars: &mut usize,
    done: &mut ChatDone,
) -> End {
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ChatEvent::TextDelta(text)) if !text.is_empty() => {
                *chars += text.chars().count();
                let delta = ChatDelta {
                    stream_id: stream_id.to_owned(),
                    text,
                };
                if notify.send(notification::CHAT_DELTA, &delta).await.is_err() {
                    return End::Disconnected;
                }
            }
            Ok(ChatEvent::Done {
                stop_reason,
                input_tokens,
                output_tokens,
            }) => {
                done.input_tokens = input_tokens;
                done.output_tokens = output_tokens;
                return End::Done(stop_reason);
            }
            Ok(_) => {}
            Err(Error::Cancelled) => return End::Cancelled,
            Err(e) => return End::Failed(e),
        }
    }
    End::Failed(Error::Provider(
        "draft stream ended before completion".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<DraftSection> {
        let t: toml::Table = toml::from_str(s).unwrap();
        section("draft", &t)
    }

    #[test]
    fn defaults_match_the_engine() {
        let s = parse("").unwrap();
        assert!(s.enabled);
        let c = s.draft_config();
        assert_eq!(c, DraftConfig::default());
        assert_eq!(s.voice_config(), VoiceConfig::default());
        assert!(s.me().is_empty());
    }

    #[test]
    fn excluded_apps_add_to_the_defaults_unless_replaced() {
        let s = parse(
            r#"
            excluded_apps = ["com.example.bank", "1password", " "]
            budget_tokens = 999999
            app_providers = { "mail.google.com" = "remote" }
            "#,
        )
        .unwrap();
        let c = s.draft_config();
        assert_eq!(c.excluded_apps.len(), DEFAULT_EXCLUDED_APPS.len() + 1);
        assert!(c.excluded_apps.iter().any(|a| a == "com.example.bank"));
        assert_eq!(c.budget_tokens, MAX_BUDGET_TOKENS);
        assert_eq!(
            c.app_providers
                .get("mail.google.com")
                .map(ProviderId::as_str),
            Some("remote")
        );

        let s = parse(
            r#"
            excluded_apps = ["com.example.bank"]
            replace_default_excluded = true
            "#,
        )
        .unwrap();
        assert_eq!(s.draft_config().excluded_apps, ["com.example.bank"]);
        assert!(!parse("").unwrap().draft_config().allow_terminals);
        assert!(
            parse("allow_terminals = true")
                .unwrap()
                .draft_config()
                .allow_terminals
        );
    }

    #[test]
    fn voice_subsection_and_unknown_keys() {
        let s = parse(
            r#"
            enabled = false
            [voice]
            emails = ["Me@Example.com"]
            names = ["Alex Morgan"]
            max_exemplars = 4
            "#,
        )
        .unwrap();
        assert!(!s.enabled);
        assert!(s.me().has_email("me@example.com"));
        assert_eq!(s.voice_config().max_exemplars, 4);
        let err = parse("nope = 1").unwrap_err().to_string();
        assert!(err.contains("[draft]"), "{err}");
        assert!(parse("[voice]\nnope = 1").is_err());
    }

    /// On a single-threaded runtime, a retrieval job that blocks its thread until another task
    /// runs only finishes if it runs off the runtime's thread.
    #[test]
    fn retrieval_runs_off_the_async_thread() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let runtime_thread = std::thread::current().id();
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let job: BoxFuture<'static, Result<PackedContext>> = Box::pin(async move {
                assert_ne!(std::thread::current().id(), runtime_thread);
                rx.recv_timeout(Duration::from_secs(10))
                    .map_err(|_| Error::Other("the async thread was blocked".into()))?;
                // Still inside the runtime: async work in the job is driven too.
                tokio::task::yield_now().await;
                Err(Error::Other("ran".into()))
            });
            let sender = tokio::spawn(async move {
                tokio::task::yield_now().await;
                tx.send(()).unwrap();
            });
            let out = BlockingRetrieval.run(job).await.unwrap_err();
            assert_eq!(out.to_string(), Error::Other("ran".into()).to_string());
            sender.await.unwrap();
        });
    }

    #[test]
    fn refusals_carry_a_stable_reason() {
        let e = refusal_error(&Refusal::SecureField);
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data, Some(json!({"refused": "secure"})));
        let e = refusal_error(&Refusal::ExcludedApp("1Password".into()));
        assert_eq!(e.data, Some(json!({"refused": "excluded_app"})));
        let e = refusal_error(&Refusal::NotDraftable(nugget_draft::FieldKind::Search));
        assert_eq!(e.code, code::UNSUPPORTED);
        assert_eq!(
            e.data,
            Some(json!({"refused": "not_draftable", "field_kind": "search"}))
        );
        let e = refusal_error(&Refusal::Terminal("Terminal".into()));
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data, Some(json!({"refused": "terminal"})));
        assert_eq!(disabled_error().data, Some(json!({"refused": "disabled"})));
    }
}
