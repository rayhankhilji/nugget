//! Double Tap drafting: `draft.begin` / `draft.cancel` / `draft.feedback` / `draft.prewarm` /
//! `draft.read_ahead` / `draft.read_ahead_cancel` / `voice.status`, over
//! [`nugget_draft::Drafter`] and the user's [`nugget_voice`] profile.
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
//!   thread a few seconds after startup (`voice_load_delay_ms`, or at the first draft or
//!   `voice.status` if sooner), then refreshed incrementally after ingest commits and draft
//!   feedback, debounced ([`voice`]).
//! - **Prompt cache.** Every `draft.prewarm` (the trigger's first tap) warms the exact system
//!   prefix of the coming draft on a non-cloud provider (connection only for cloud ones). With
//!   `prewarm_on_start = true` the feature also caches the drafting rules on the default
//!   non-cloud provider at start ([`Drafter::prewarm_idle`]); off by default, since it sends a
//!   request at startup and may start the on-device chat server. `draft.end` audits prompt and
//!   cached prompt tokens.
//! - **Read-ahead.** While a reply field keeps focus, the desktop sends `draft.read_ahead` with
//!   the context it would send at the trigger; the engine prefills that whole prompt into a
//!   non-cloud provider's cache, generating nothing ([`Drafter::read_ahead`]). The call answers
//!   within ~100 ms: `{"started": false, "skipped": <reason>}` when it did nothing (`"disabled"`,
//!   `"refused"`, `"cloud"`, `"rate_limited"`, `"unchanged"`, `"unavailable"`, `"cancelled"`),
//!   `{"started": true, "done": true}` when it already finished, else `{"started": true}` (it
//!   keeps running in the background). `draft.read_ahead_cancel` (focus left the field) answers
//!   `{"cancelled": bool}`. Nothing about a read-ahead is audited or stored; the outcome is
//!   logged at debug level without content.
//! - **Small local model.** `[draft.local] model` names a manifest chat model that the local
//!   runtime serves in its own `llama-server` (role `draft`, provider `local-draft`, no tiers);
//!   drafts routed by tier to a non-cloud provider go to it first ([`LocalBudget::provider`]).
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
//! voice_load_delay_ms = 5000            # wait after startup before loading the voice profile
//! prewarm_on_start = false        # cache the drafting rules on the local server at start
//!
//! [draft.local]                   # smaller prompts for on-device and LAN providers
//! enabled = true                  # false: they get the cloud budget above
//! budget_tokens = 300             # retrieval budget for them
//! max_snippets = 3                # best retrieved passages kept
//! include_sensitive = false       # let passages marked sensitive into local prompts
//! model = "qwen3-1.7b-q8"         # a manifest chat model served for Double Tap alone (unset:
//!                                 # local drafts use the tier's chat server)
//!
//! [draft.read_ahead]              # prefill the prompt while a reply field keeps focus
//! enabled = true                  # false: draft.read_ahead skips ("disabled")
//! min_interval_ms = 5000          # minimum time between two read-aheads that do work
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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use nugget_core::capability::Tier;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::RuntimeRole;
use nugget_core::ipc::{
    ChatCancel, ChatDelta, ChatDone, DraftContext, DraftFeedback, DraftRequest, method,
    notification,
};
use nugget_core::{Error, Result};
use nugget_draft::{
    DEFAULT_EXCLUDED_APPS, DraftConfig, DraftStream, Drafter, LocalBudget, MAX_BUDGET_TOKENS,
    ProfileVoice, ReadAheadConfig, Refusal, RetrievalRunner, VoiceFeedback, Warmer, gate_with,
};
use nugget_infer::{ChatEvent, ChatRequest, Provider, StopReason};
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
    method::DRAFT_READ_AHEAD,
    method::DRAFT_READ_AHEAD_CANCEL,
];

/// How long `draft.read_ahead` waits for the read-ahead before answering `{"started": true}`
/// (quick skips, the common case, are answered with their reason).
const READ_AHEAD_ANSWER_WAIT: Duration = Duration::from_millis(100);

/// Shortest time between two `draft.read_ahead` calls that pass the field gate (capped by
/// `[draft.read_ahead] min_interval_ms`, so `0` turns it off). The engine's own rate limit
/// counts only prefills it sent, and every call cancels the one before it, so a burst of
/// calls would each start a retrieval (on the blocking pool, not cancelled) and voice lookup
/// without ever being limited; the call answers in ~100 ms, so the IPC server's per-connection
/// cap does not bound that work either. The desktop sends at most one every ~1.5 s (dwell).
const READ_AHEAD_MIN_SPACING: Duration = Duration::from_secs(1);

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
    grounding: bool,
    grounding_check: nugget_draft::GroundingCheck,
    voice_refresh_debounce_ms: u64,
    voice_load_delay_ms: u64,
    voice: VoiceSection,
    local: LocalSection,
    read_ahead: ReadAheadSection,
    /// Warm the default local provider's prompt cache with the drafting rules at start.
    prewarm_on_start: bool,
}

/// `[draft.local]`: the prompt budget for non-cloud providers ([`LocalBudget`]).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct LocalSection {
    enabled: bool,
    budget_tokens: u32,
    max_snippets: usize,
    include_sensitive: bool,
    /// Manifest id of the small chat model local Double Tap drafts with, served in its own
    /// `llama-server` (runtime role `draft`, provider `local-draft`). Unset: none.
    model: Option<String>,
}

impl Default for LocalSection {
    fn default() -> Self {
        let d = LocalBudget::default();
        Self {
            enabled: d.enabled,
            budget_tokens: d.budget_tokens,
            max_snippets: d.max_snippets,
            include_sensitive: d.include_sensitive,
            model: None,
        }
    }
}

impl LocalSection {
    /// `model`, trimmed; `None` when unset or blank.
    fn model(&self) -> Option<&str> {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }
}

/// `[draft.read_ahead]`: prefill the prompt while a reply field keeps focus
/// ([`ReadAheadConfig`]).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ReadAheadSection {
    enabled: bool,
    min_interval_ms: u64,
}

impl Default for ReadAheadSection {
    fn default() -> Self {
        let d = ReadAheadConfig::default();
        Self {
            enabled: d.enabled,
            min_interval_ms: d.min_interval_ms,
        }
    }
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
            grounding: d.grounding,
            grounding_check: d.grounding_check,
            voice_refresh_debounce_ms: 5_000,
            voice_load_delay_ms: 5_000,
            voice: VoiceSection::default(),
            local: LocalSection::default(),
            read_ahead: ReadAheadSection::default(),
            prewarm_on_start: false,
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
            grounding: self.grounding,
            grounding_check: self.grounding_check,
            local: LocalBudget {
                enabled: self.local.enabled,
                budget_tokens: self.local.budget_tokens.min(MAX_BUDGET_TOKENS),
                max_snippets: self.local.max_snippets,
                include_sensitive: self.local.include_sensitive,
                // The small model's server, when one is configured (see `crate::local`).
                provider: self
                    .local
                    .model()
                    .map(|_| nugget_runtime::provider_id(RuntimeRole::Draft)),
                ..LocalBudget::default()
            },
            read_ahead: ReadAheadConfig {
                enabled: self.read_ahead.enabled,
                min_interval_ms: self.read_ahead.min_interval_ms,
            },
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

/// The engine config for the `[draft]` table `table`, exactly as Double Tap builds it (an
/// invalid table is an error, as for the draft feature).
pub(crate) fn draft_config_from_table(table: &toml::Table) -> Result<DraftConfig> {
    Ok(section::<DraftSection>("draft", table)?.draft_config())
}

/// The manifest id of `[draft.local] model` in the `[draft]` table `table`: trimmed, `None` when
/// unset, blank or when the table does not parse (the draft feature reports that error).
pub(crate) fn local_draft_model(table: &toml::Table) -> Option<String> {
    let s = section::<DraftSection>("draft", table).ok()?;
    s.local.model().map(str::to_owned)
}

/// Opens the provider's connection (and starts a stopped local server) on trigger, and warms a
/// local server's prompt cache with the draft's system prefix.
struct ProviderWarmer;

#[async_trait]
impl Warmer for ProviderWarmer {
    async fn warm(&self, provider: Arc<dyn Provider>) {
        if let Err(e) = provider.prewarm().await {
            tracing::debug!(provider = %provider.id(), error = %e, "draft: prewarm failed");
        }
    }

    async fn warm_prefix(&self, provider: Arc<dyn Provider>, prefix: ChatRequest) {
        if let Err(e) = provider.prewarm_prefix(&prefix).await {
            tracing::debug!(provider = %provider.id(), error = %e, "draft: prefix prewarm failed");
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
    /// When the last `draft.read_ahead` past the gate was admitted ([`READ_AHEAD_MIN_SPACING`]).
    read_ahead_at: Mutex<Option<Instant>>,
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
        voice::Timing {
            debounce: Duration::from_millis(cfg.voice_refresh_debounce_ms),
            load_delay: Duration::from_millis(cfg.voice_load_delay_ms),
        },
        state.shutdown.clone(),
    ));
    // Opt-in: cache the drafting rules on the default (non-cloud) provider now.
    let idle_warmed = if cfg.enabled && cfg.prewarm_on_start {
        drafter.prewarm_idle()
    } else {
        None
    };
    tracing::info!(
        enabled = cfg.enabled,
        excluded_apps = drafter.config().excluded_apps.len(),
        app_providers = drafter.config().app_providers.len(),
        local_budget = drafter.config().local.enabled,
        idle_warmed = idle_warmed.as_ref().map(ProviderId::as_str),
        "draft feature started"
    );
    Arc::new(DraftFeature {
        state: state.clone(),
        drafter,
        voice,
        enabled: cfg.enabled,
        read_ahead_at: Mutex::new(None),
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
            method::DRAFT_READ_AHEAD => self.read_ahead(parse_params(params)?).await,
            method::DRAFT_READ_AHEAD_CANCEL => Ok(self.read_ahead_cancel()),
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

    /// Start a read-ahead in the background and answer within [`READ_AHEAD_ANSWER_WAIT`]: with
    /// the skip reason or completion if it finished by then, else `{"started": true}`. Nothing
    /// is audited or stored, and nothing about the field is logged.
    async fn read_ahead(&self, req: DraftRequest) -> Result<Value, RpcError> {
        if !self.enabled {
            return Ok(json!({ "started": false, "skipped": "disabled" }));
        }
        if self.state.shutdown.is_cancelled() {
            return Err(shutting_down());
        }
        let config = self.drafter.config();
        if !config.read_ahead.enabled {
            return Ok(json!({ "started": false, "skipped": "disabled" }));
        }
        // Refused fields cost nothing and never count against the spacing.
        if gate_with(&req.context, &config.excluded_apps, config.allow_terminals).is_err() {
            return Ok(json!({ "started": false, "skipped": "refused" }));
        }
        let spacing =
            READ_AHEAD_MIN_SPACING.min(Duration::from_millis(config.read_ahead.min_interval_ms));
        if !admit(&self.read_ahead_at, Instant::now(), spacing) {
            return Ok(json!({ "started": false, "skipped": "rate_limited" }));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let drafter = self.drafter.clone();
        let shutdown = self.state.shutdown.clone();
        self.state.tasks.spawn(async move {
            let r = tokio::select! {
                r = drafter.read_ahead(req) => r,
                // A read-ahead in flight at shutdown is dropped (its request with it).
                () = shutdown.cancelled() => Err(nugget_draft::ReadAheadSkip::Cancelled),
            };
            match &r {
                Ok(o) => tracing::debug!(
                    provider = %o.provider,
                    prompt_chars = o.prompt_chars,
                    prepare_ms = o.prepare_ms,
                    warm_ms = o.warm_ms,
                    "draft: read-ahead done"
                ),
                Err(skip) => tracing::debug!(skipped = skip.as_str(), "draft: read-ahead skipped"),
            }
            let _ = tx.send(r);
        });
        Ok(
            match tokio::time::timeout(READ_AHEAD_ANSWER_WAIT, rx).await {
                Ok(Ok(Ok(_))) => json!({ "started": true, "done": true }),
                Ok(Ok(Err(skip))) => json!({ "started": false, "skipped": skip.as_str() }),
                // The task was dropped (shutdown) before it answered.
                Ok(Err(_)) => json!({ "started": false, "skipped": "cancelled" }),
                Err(_) => json!({ "started": true }),
            },
        )
    }

    fn read_ahead_cancel(&self) -> Value {
        let cancelled = self.drafter.cancel_read_ahead();
        tracing::debug!(cancelled, "draft: read-ahead cancel");
        json!({ "cancelled": cancelled })
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

/// Whether a call at `now` may start, at most one per `spacing`; records it if so.
fn admit(last: &Mutex<Option<Instant>>, now: Instant, spacing: Duration) -> bool {
    let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|t| now.saturating_duration_since(t) < spacing) {
        return false;
    }
    *last = Some(now);
    true
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
            "prompt_tokens": timings.prompt_tokens,
            "cached_prompt_tokens": timings.cached_prompt_tokens,
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

    #[test]
    fn local_subsection_sets_the_local_budget() {
        let c = parse("").unwrap().draft_config();
        assert_eq!(c.local, LocalBudget::default());
        let c = parse(
            r#"
            budget_tokens = 2000
            [local]
            budget_tokens = 999999
            max_snippets = 2
            "#,
        )
        .unwrap()
        .draft_config();
        assert!(c.local.enabled);
        assert_eq!(c.local.budget_tokens, MAX_BUDGET_TOKENS);
        assert_eq!(c.local.max_snippets, 2);
        assert_eq!(c.budget_tokens, 2000);
        assert!(
            !parse("[local]\nenabled = false")
                .unwrap()
                .draft_config()
                .local
                .enabled
        );
        assert!(parse("[local]\nnope = 1").is_err());
        assert!(!parse("").unwrap().prewarm_on_start);
        assert!(parse("prewarm_on_start = true").unwrap().prewarm_on_start);
    }

    #[test]
    fn local_model_routes_local_drafts_to_the_draft_server() {
        let c = parse("").unwrap().draft_config();
        assert_eq!(c.local.provider, None);
        let t: toml::Table = toml::from_str("").unwrap();
        assert_eq!(local_draft_model(&t), None);

        let src = "[local]\nmodel = \"  qwen3-4b-q4km \"\nmax_snippets = 2";
        let c = parse(src).unwrap().draft_config();
        assert_eq!(
            c.local.provider.as_ref().map(ProviderId::as_str),
            Some("local-draft")
        );
        assert_eq!(c.local.max_snippets, 2);
        let t: toml::Table = toml::from_str(src).unwrap();
        assert_eq!(local_draft_model(&t).as_deref(), Some("qwen3-4b-q4km"));

        // A blank model is no model.
        let src = "[local]\nmodel = \" \"";
        assert_eq!(parse(src).unwrap().draft_config().local.provider, None);
        assert_eq!(local_draft_model(&toml::from_str(src).unwrap()), None);
        // An invalid table names no model (the feature reports the error).
        let src = "[local]\nmodel = \"m\"\nnope = 1";
        assert!(parse(src).is_err());
        assert_eq!(local_draft_model(&toml::from_str(src).unwrap()), None);
        assert_eq!(
            local_draft_model(&toml::from_str("[local]\nmodel = 3").unwrap()),
            None
        );
    }

    #[test]
    fn read_ahead_calls_are_spaced_out() {
        let last = Mutex::new(None);
        let t0 = Instant::now();
        let s = READ_AHEAD_MIN_SPACING;
        assert!(admit(&last, t0, s));
        // A burst is refused, however many calls it has...
        for ms in [0, 1, 500, 999] {
            assert!(!admit(&last, t0 + Duration::from_millis(ms), s), "{ms}");
        }
        // ...and refused calls do not push the next admission back.
        assert!(admit(&last, t0 + s, s));
        assert!(!admit(&last, t0 + s + Duration::from_millis(10), s));
        // No spacing: everything is admitted.
        assert!(admit(&last, t0 + s, Duration::ZERO));
    }

    #[test]
    fn read_ahead_subsection_maps_to_the_engine() {
        let c = parse("").unwrap().draft_config();
        assert_eq!(c.read_ahead, ReadAheadConfig::default());
        assert!(c.read_ahead.enabled);
        assert_eq!(c.read_ahead.min_interval_ms, 5_000);
        let c = parse("[read_ahead]\nenabled = false\nmin_interval_ms = 250")
            .unwrap()
            .draft_config();
        assert_eq!(
            c.read_ahead,
            ReadAheadConfig {
                enabled: false,
                min_interval_ms: 250,
            }
        );
        // Other settings keep their defaults.
        assert_eq!(c.local, LocalBudget::default());
        let err = parse("[read_ahead]\nnope = 1").unwrap_err().to_string();
        assert!(err.contains("[draft]"), "{err}");
        assert!(parse("read_ahead = true").is_err());
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

    /// Records what the warmer asks of it.
    struct Recorder {
        id: ProviderId,
        calls: std::sync::Mutex<Vec<Option<ChatRequest>>>,
    }

    #[async_trait]
    impl Provider for Recorder {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn meta(&self) -> nugget_infer::ProviderMeta {
            nugget_infer::ProviderMeta {
                kind: nugget_infer::ProviderKind::OpenaiCompatible,
                model: "m".into(),
                locality: nugget_core::capability::Locality::OnDevice,
                context_tokens: None,
            }
        }

        async fn chat(&self, _req: ChatRequest) -> Result<nugget_infer::ChatStream> {
            Err(Error::Unsupported("no chat".into()))
        }

        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(Error::Unsupported("no embeddings".into()))
        }

        async fn prewarm(&self) -> Result<()> {
            self.calls.lock().unwrap().push(None);
            Ok(())
        }

        async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
            self.calls.lock().unwrap().push(Some(req.clone()));
            Err(Error::Network("down".into()))
        }
    }

    #[tokio::test]
    async fn warmer_sends_the_prefix_to_the_provider() {
        let p = Arc::new(Recorder {
            id: ProviderId::new("local-chat"),
            calls: std::sync::Mutex::new(Vec::new()),
        });
        let prefix = ChatRequest::default().with_system("rules");
        // Failures are logged, never raised.
        ProviderWarmer.warm_prefix(p.clone(), prefix.clone()).await;
        ProviderWarmer.warm(p.clone()).await;
        assert_eq!(*p.calls.lock().unwrap(), vec![Some(prefix), None]);
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
