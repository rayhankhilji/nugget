//! Heed cloud model routing: `cloud.llm.session` / `cloud.llm.status`.
//!
//! The desktop app owns the Supabase session (refresh token in the OS keychain, refreshed by
//! `nugget-cloud-client`). While the user is signed in it hands nuggetd the short-lived access
//! token over this IPC socket (owner-only, peer uid checked), again before each expiry and on
//! every reconnect; signing out withdraws it. nuggetd keeps the token in memory only: never on
//! disk, in config, in logs or in the audit log.
//!
//! With a valid token, `heed-cloud-fast` (triage, light work, ghost text) and
//! `heed-cloud-smart` (drafting, asks, the agent) from [`nugget_infer::cloud`] are routed FIRST
//! on their tiers, ahead of local models, which stay as the fallback: a provider error, the
//! quota answer (429), a refused token or an expired one falls through to them at once.
//! Signed out, the cloud providers leave the router and everything is local again.
//!
//! The cloud providers are `Locality::Cloud`, so the egress ledger shows the Heed host for each
//! request and every existing rule about what may leave the device applies unchanged: sensitive
//! passages are withheld, screenshots only with `allow_cloud_screenshots`, read-ahead sends no
//! content (it only opens the connection), triage of local items and embeddings stay local.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::egress::EgressSink;
use nugget_core::ipc::{CloudLlmSession, CloudLlmStatus, method};
use nugget_infer::Secret;
use nugget_infer::cloud::{
    CloudProviders, CloudSession, EXPIRY_MARGIN_SECS, FAST_TIERS, SMART_TIERS,
    build_cloud_providers, cloud_capabilities, llm_base_url,
};
use nugget_ipc::{Notifier, RpcError, code, to_result};
use nugget_store::AuditEntry;
use serde_json::{Value, json};

use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};

const METHODS: &[&str] = &[method::CLOUD_LLM_SESSION, method::CLOUD_LLM_STATUS];

/// Longest token lifetime accepted: the handoff is for short-lived access tokens only.
pub(crate) const MAX_TOKEN_TTL_SECS: i64 = 24 * 3600;
const MAX_TOKEN_LEN: usize = 8 * 1024;

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    Ok(Arc::new(CloudLlm::new(
        cx.state.clone(),
        Arc::new(cx.state.store.clone()),
    )))
}

struct Installed {
    api_base: String,
    providers: CloudProviders,
}

pub(crate) struct CloudLlm {
    state: Arc<State>,
    egress: Arc<dyn EgressSink>,
    session: Arc<CloudSession>,
    installed: Mutex<Option<Installed>>,
    /// Serialises session changes (a build resolves DNS on a blocking thread).
    change: tokio::sync::Mutex<()>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// `https://api.heed.example:8443/x` → `api.heed.example:8443` (the base was validated).
fn host_of(api_base: &str) -> String {
    let rest = api_base.split_once("://").map_or(api_base, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

fn invalid(m: impl Into<String>) -> RpcError {
    RpcError::invalid_params(m.into())
}

impl CloudLlm {
    pub(crate) fn new(state: Arc<State>, egress: Arc<dyn EgressSink>) -> Self {
        Self {
            state,
            egress,
            session: Arc::new(CloudSession::new()),
            installed: Mutex::new(None),
            change: tokio::sync::Mutex::new(()),
        }
    }

    fn installed(&self) -> std::sync::MutexGuard<'_, Option<Installed>> {
        self.installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn status(&self) -> CloudLlmStatus {
        let inst = self.installed();
        let active = inst.is_some() && self.session.is_active();
        CloudLlmStatus {
            active,
            api_host: inst.as_ref().map(|i| host_of(&i.api_base)),
            expires_at: inst.as_ref().and(self.session.expires_at()),
            providers: inst
                .as_ref()
                .map(|i| {
                    vec![
                        i.providers.fast.id().clone(),
                        i.providers.smart.id().clone(),
                    ]
                })
                .unwrap_or_default(),
        }
    }

    fn audit(&self, action: &str, detail: Value) {
        let store = self.state.store.clone();
        let entry = AuditEntry {
            actor: "desktop".into(),
            action: action.into(),
            target: "heed-cloud".into(),
            detail,
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.append_audit(&entry) {
                tracing::warn!(error = %e, "cloud llm: audit append failed");
            }
        });
    }

    /// Route the cloud providers first on their tiers (again: a provider replaced or moved by
    /// a config change is put back in front), with their known capabilities.
    fn route(&self, p: &CloudProviders) {
        let router = &self.state.router;
        for (provider, tiers) in [(&p.fast, FAST_TIERS), (&p.smart, SMART_TIERS)] {
            router.set_provider_before(provider.clone(), tiers, &|_| true);
            router.set_capabilities(provider.id(), cloud_capabilities());
        }
    }

    fn unroute(&self, p: &CloudProviders) {
        self.state.router.remove_provider(p.fast.id());
        self.state.router.remove_provider(p.smart.id());
    }

    pub(crate) async fn set_session(
        &self,
        req: CloudLlmSession,
    ) -> Result<CloudLlmStatus, RpcError> {
        let _serial = self.change.lock().await;
        let Some(token) = req.access_token.filter(|t| !t.is_empty()) else {
            return Ok(self.sign_out());
        };
        if token.len() > MAX_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid("access_token is not a token"));
        }
        let expires_at = req
            .expires_at
            .ok_or_else(|| invalid("expires_at is required"))?;
        let now = now();
        if expires_at - EXPIRY_MARGIN_SECS <= now {
            return Err(invalid("the access token has expired"));
        }
        if expires_at - now > MAX_TOKEN_TTL_SECS {
            return Err(invalid("only short-lived access tokens are accepted"));
        }
        let api_base = req
            .api_base
            .ok_or_else(|| invalid("api_base is required"))?
            .trim()
            .trim_end_matches('/')
            .to_owned();
        llm_base_url(&api_base).map_err(|e| invalid(e.to_string()))?;

        let same = self
            .installed()
            .as_ref()
            .is_some_and(|i| i.api_base == api_base);
        if !same {
            let (session, egress, base) =
                (self.session.clone(), self.egress.clone(), api_base.clone());
            let built = blocking(move || build_cloud_providers(&base, session, egress))
                .await
                .map_err(|e| RpcError::new(code::PROVIDER, format!("heed cloud: {e}")))?;
            if let Some(old) = self.installed().take() {
                self.unroute(&old.providers);
            }
            *self.installed() = Some(Installed {
                api_base: api_base.clone(),
                providers: built,
            });
        }
        let was_active = self.session.is_active();
        self.session.set(Secret::new(token), expires_at);
        if let Some(inst) = self.installed().as_ref() {
            self.route(&inst.providers);
        }
        if !was_active || !same {
            tracing::info!(api = %api_base, "Heed cloud model on: routed first, local models as fallback");
            self.audit("cloud.llm.on", json!({ "api_base": api_base }));
        }
        Ok(self.status())
    }

    fn sign_out(&self) -> CloudLlmStatus {
        self.session.clear();
        if let Some(inst) = self.installed().take() {
            self.unroute(&inst.providers);
            tracing::info!("Heed cloud model off: local models only");
            self.audit("cloud.llm.off", json!({}));
        }
        self.status()
    }
}

#[async_trait]
impl Feature for CloudLlm {
    fn name(&self) -> &'static str {
        "cloud_llm"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> Result<Value, RpcError> {
        match method {
            method::CLOUD_LLM_SESSION => {
                let req: CloudLlmSession = serde_json::from_value(params)
                    .map_err(|_| invalid("cloud.llm.session: bad params"))?;
                to_result(self.set_session(req).await?)
            }
            method::CLOUD_LLM_STATUS => to_result(self.status()),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    async fn shutdown(&self) {
        self.session.clear();
    }
}
