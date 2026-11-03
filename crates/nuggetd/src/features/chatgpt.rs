//! "Sign in with ChatGPT" for local mode: `ai.chatgpt.status|sign_in|cancel|resume|sign_out`.
//! The flow and its rules are in [`nugget_infer::chatgpt`]; this is the daemon side.
//!
//! - `ai.chatgpt.sign_in` binds a loopback listener on `127.0.0.1` (random port), returns
//!   OpenAI's authorize URL for the desktop to open in the system browser, and finishes when the
//!   browser comes back (or after [`SIGN_IN_TIMEOUT`], or `ai.chatgpt.cancel`). A callback with
//!   the wrong `state` is answered and ignored; only a matching one is exchanged.
//! - The issued client id and the installation's host id are kept in `chatgpt-host.json` in the
//!   data directory (owner-only; not secrets, and kept across sign-outs as the docs ask). The
//!   tokens and the account live in the OS keychain only ([`CREDENTIALS`]), never on disk, in
//!   config, logs, the audit log or a status.
//! - Signed in WITH `chatgpt.tokens.use.direct`, `chatgpt-light` (light tier) and
//!   `chatgpt-smart` (synthesis, agentic) are routed first; tokens are refreshed in the
//!   background. Without that scope nothing is routed and the status says plan usage is off.
//! - A usage limit pauses ChatGPT requests until `ai.chatgpt.resume`; requests fail with a clear
//!   error rather than moving to another (possibly paid) provider.
//! - Not while Heed Cloud serves this computer (`cloud_mode`): plan usage is for local use.
//!
//! Auth requests are recorded in the egress ledger (`ai_setup`); inference requests are, like
//! any provider's, under `api.openai.com`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::SecretStore;
use nugget_core::Result;
use nugget_core::capability::Capabilities;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ProviderId;
use nugget_infer::chatgpt::{
    self, Account, Attempt, CHATGPT_LIGHT, CHATGPT_SMART, Callback, ChatgptProviders,
    ChatgptSession, Endpoints, LIGHT_TIERS, MANAGE_USAGE_URL, SMART_TIERS, TokenError, Tokens,
};
use nugget_ipc::{Notifier, RpcError, to_result};
use nugget_store::AuditEntry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};

pub(crate) const STATUS: &str = "ai.chatgpt.status";
pub(crate) const SIGN_IN: &str = "ai.chatgpt.sign_in";
pub(crate) const CANCEL: &str = "ai.chatgpt.cancel";
pub(crate) const RESUME: &str = "ai.chatgpt.resume";
pub(crate) const SIGN_OUT: &str = "ai.chatgpt.sign_out";
const METHODS: &[&str] = &[STATUS, SIGN_IN, CANCEL, RESUME, SIGN_OUT];

/// Keychain account of the tokens and the account.
pub(crate) const CREDENTIALS: &str = "chatgpt:credentials";
/// In the data directory: host id and issued client id.
pub(crate) const HOST_FILE: &str = "chatgpt-host.json";
pub(crate) const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);
const CALLBACK_PATH: &str = "/auth/callback";
const REFRESH_EVERY: Duration = Duration::from_secs(60);

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let f = Arc::new(Chatgpt::new(
        cx.state.clone(),
        Arc::new(cx.state.store.clone()),
        cx.state.chatgpt_endpoints.clone(),
    ));
    let feature: Arc<dyn Feature> = Arc::new(f.clone());
    let (restore, refresher) = (f.clone(), f.clone());
    cx.state.tasks.spawn(async move { restore.restore().await });
    let shutdown = cx.state.shutdown.clone();
    cx.state.tasks.spawn(async move {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(REFRESH_EVERY) => refresher.refresh_if_due().await,
            }
        }
    });
    Ok(feature)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatgptError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatgptModels {
    pub light: Option<String>,
    pub smart: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatgptAccount {
    pub email: Option<String>,
    pub name: Option<String>,
}

/// What the desktop shows. Never carries a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatgptStatus {
    /// `signed_out`, `signing_in` or `signed_in`.
    pub state: String,
    pub account: Option<ChatgptAccount>,
    /// `chatgpt.tokens.use.direct` was granted (without it nothing is sent).
    pub plan_usage: bool,
    /// The plan's usage limit was reached; ChatGPT requests are paused until resumed.
    pub paused: bool,
    pub error: Option<ChatgptError>,
    pub models: ChatgptModels,
    pub manage_usage_url: String,
}

/// Non-secret, kept across sign-outs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostRecord {
    pub host_id: String,
    pub client_id: Option<String>,
}

/// In the keychain only.
#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    client_id: String,
    account: Account,
    tokens: Tokens,
}

#[derive(Default)]
enum Phase {
    #[default]
    SignedOut,
    SigningIn(Option<oneshot::Sender<()>>),
    SignedIn,
}

#[derive(Default)]
struct Inner {
    phase: Phase,
    client_id: Option<String>,
    account: Option<Account>,
    error: Option<ChatgptError>,
    models: ChatgptModels,
    installed: Option<ChatgptProviders>,
}

pub(crate) struct Chatgpt {
    state: Arc<State>,
    egress: Arc<dyn EgressSink>,
    ep: Endpoints,
    http: reqwest::Client,
    session: Arc<ChatgptSession>,
    inner: Mutex<Inner>,
    change: tokio::sync::Mutex<()>,
}

fn err(code: &str, message: impl Into<String>) -> ChatgptError {
    ChatgptError {
        code: code.into(),
        message: message.into(),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| u.port().map_or(h.to_owned(), |p| format!("{h}:{p}")))
        })
        .unwrap_or_default()
}

/// Reads (or creates, owner-only) the host record.
pub(crate) fn load_host(dir: &Path) -> Result<HostRecord> {
    let path = dir.join(HOST_FILE);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(rec) = serde_json::from_str::<HostRecord>(&text)
        && chatgpt::is_host_id(&rec.host_id)
    {
        let client_id = rec.client_id.filter(|c| chatgpt::is_client_id(c));
        return Ok(HostRecord {
            host_id: rec.host_id,
            client_id,
        });
    }
    let rec = HostRecord {
        host_id: chatgpt::new_host_id()?,
        client_id: None,
    };
    save_host(dir, &rec)?;
    Ok(rec)
}

/// Writes atomically with owner-only permissions.
pub(crate) fn save_host(dir: &Path, rec: &HostRecord) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{HOST_FILE}.tmp"));
    let body = serde_json::to_vec_pretty(rec)?;
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        use std::io::Write;
        let mut f = o.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(HOST_FILE))?;
    Ok(())
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Heed</title>\
         <body style=\"font:16px system-ui;margin:3em\"><h1>{title}</h1><p>{body}</p></body>"
    )
}

/// Reads one HTTP request head (at most 8 KiB) and returns the request target.
async fn read_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8 * 1024 {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_owned)
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, html: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
         cache-control: no-store\r\nconnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

impl Chatgpt {
    pub(crate) fn new(state: Arc<State>, egress: Arc<dyn EgressSink>, ep: Endpoints) -> Self {
        Self {
            state,
            egress,
            ep,
            http: reqwest::Client::builder()
                .user_agent(nugget_core::identity::USER_AGENT)
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            session: Arc::new(ChatgptSession::new()),
            inner: Mutex::new(Inner::default()),
            change: tokio::sync::Mutex::new(()),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.state.secrets.clone()
    }

    fn data_dir(&self) -> PathBuf {
        self.state.paths.data.clone()
    }

    fn record(&self, url: &str, sent: usize) {
        self.egress.record(EgressEvent {
            at: time::OffsetDateTime::now_utc(),
            host: host_of(url),
            feature: "ai_setup".into(),
            bytes_sent: sent as u64,
            bytes_received: 0,
            items: Vec::new(),
        });
    }

    fn audit(&self, action: &str, detail: Value) {
        let store = self.state.store.clone();
        let entry = AuditEntry {
            actor: "desktop".into(),
            action: action.into(),
            target: "chatgpt".into(),
            detail,
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.append_audit(&entry) {
                tracing::warn!(error = %e, "chatgpt: audit append failed");
            }
        });
    }

    /// Whether Heed Cloud's models serve this computer now.
    fn cloud_mode(&self) -> bool {
        [
            nugget_infer::cloud::CLOUD_FAST,
            nugget_infer::cloud::CLOUD_SMART,
        ]
        .iter()
        .any(|id| self.state.router.provider(&ProviderId::new(*id)).is_some())
    }

    pub(crate) fn status(&self) -> ChatgptStatus {
        let i = self.inner();
        let state = match i.phase {
            Phase::SignedOut => "signed_out",
            Phase::SigningIn(_) => "signing_in",
            Phase::SignedIn => "signed_in",
        };
        let error = i.error.clone().or_else(|| {
            if self.session.needs_reauth() {
                Some(err("reauth_required", "Sign in to ChatGPT again."))
            } else if self.session.paused() {
                Some(err(
                    "usage_limit",
                    "Your ChatGPT plan's usage limit was reached. Check your usage, then resume.",
                ))
            } else {
                None
            }
        });
        ChatgptStatus {
            state: state.into(),
            account: i.account.as_ref().map(|a| ChatgptAccount {
                email: a.email.clone(),
                name: a.name.clone(),
            }),
            plan_usage: self.session.plan_usage(),
            paused: self.session.paused(),
            error,
            models: i.models.clone(),
            manage_usage_url: MANAGE_USAGE_URL.into(),
        }
    }

    fn route(&self, p: &ChatgptProviders) {
        let router = &self.state.router;
        for (provider, tiers) in [(&p.light, LIGHT_TIERS), (&p.smart, SMART_TIERS)] {
            router.set_provider_before(provider.clone(), tiers, &|_| true);
            router.set_capabilities(
                provider.id(),
                Capabilities {
                    streaming: true,
                    vision: true,
                    tools: true,
                    json_schema: true,
                    embeddings: false,
                    context_tokens: Some(128_000),
                },
            );
        }
    }

    fn unroute(&self) {
        if self.inner().installed.take().is_some() {
            self.state
                .router
                .remove_provider(&ProviderId::new(CHATGPT_LIGHT));
            self.state
                .router
                .remove_provider(&ProviderId::new(CHATGPT_SMART));
        }
    }

    /// Lists the plan's models and routes the providers (plan usage required).
    async fn install(&self) -> std::result::Result<(), ChatgptError> {
        if !self.session.plan_usage() {
            return Err(err(
                "scope_not_granted",
                "ChatGPT plan usage isn't enabled for Heed.",
            ));
        }
        let token = self
            .session
            .tokens()
            .map(|t| t.access_token)
            .unwrap_or_default();
        let url = format!("{}/models", self.ep.api.trim_end_matches('/'));
        self.record(&url, 0);
        let res = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| err("network", e.without_url().to_string()))?;
        if !res.status().is_success() {
            return Err(err(
                "network",
                format!("listing models: HTTP {}", res.status().as_u16()),
            ));
        }
        let body: Value =
            serde_json::from_str(&res.text().await.unwrap_or_default()).unwrap_or(Value::Null);
        let (_, light, smart) = chatgpt::pick_models(&body);
        let (Some(light), Some(smart)) = (light, smart) else {
            return Err(err(
                "no_models",
                "Your ChatGPT plan offers no models to Heed.",
            ));
        };
        let (session, egress, api) = (
            self.session.clone(),
            self.egress.clone(),
            self.ep.api.clone(),
        );
        let (l, s) = (light.clone(), smart.clone());
        let built =
            blocking(move || chatgpt::build_chatgpt_providers(session, egress, &api, &l, &s))
                .await
                .map_err(|e| err("network", e.to_string()))?;
        self.unroute();
        self.route(&built);
        let mut i = self.inner();
        i.models = ChatgptModels {
            light: Some(light),
            smart: Some(smart),
        };
        i.installed = Some(built);
        Ok(())
    }

    async fn save_credentials(&self, creds: &Credentials) -> bool {
        let secrets = self.secrets();
        let body = match serde_json::to_string(creds) {
            Ok(b) => b,
            Err(_) => return false,
        };
        blocking(move || secrets.set(CREDENTIALS, &body))
            .await
            .is_ok()
    }

    /// At start-up: signed in before? Restore, refresh if due, route.
    pub(crate) async fn restore(&self) {
        let secrets = self.secrets();
        let Ok(Some(text)) = blocking(move || secrets.get(CREDENTIALS)).await else {
            return;
        };
        let Ok(creds) = serde_json::from_str::<Credentials>(&text) else {
            tracing::warn!("chatgpt: stored credentials are unreadable; sign in again");
            return;
        };
        let _serial = self.change.lock().await;
        {
            let mut i = self.inner();
            i.phase = Phase::SignedIn;
            i.client_id = Some(creds.client_id.clone());
            i.account = Some(creds.account.clone());
        }
        self.session.set(creds.tokens);
        drop(_serial);
        self.refresh_if_due().await;
        if self.session.plan_usage()
            && !self.session.needs_reauth()
            && let Err(e) = self.install().await
        {
            tracing::warn!(code = %e.code, "chatgpt: not routed after restart");
            self.inner().error = Some(e);
        }
    }

    /// Refreshes the access token when it is close to expiry or was rejected.
    pub(crate) async fn refresh_if_due(&self) {
        if !self.session.needs_refresh() {
            return;
        }
        let _serial = self.change.lock().await;
        let (Some(t), Some(client)) = (self.session.tokens(), self.inner().client_id.clone())
        else {
            return;
        };
        let Some(rt) = t.refresh_token.clone() else {
            return;
        };
        self.record(&self.ep.token, 0);
        match chatgpt::refresh(&self.http, &self.ep, &client, &rt, now()).await {
            Ok(fresh) => {
                self.session.set(fresh.clone());
                let account = self.inner().account.clone();
                if let Some(account) = account {
                    let saved = self
                        .save_credentials(&Credentials {
                            client_id: client,
                            account,
                            tokens: fresh,
                        })
                        .await;
                    if !saved {
                        tracing::warn!("chatgpt: refreshed tokens could not be saved");
                    }
                }
            }
            Err(TokenError::Reauth(code)) => {
                tracing::info!(%code, "chatgpt: refresh token no longer valid; sign in again");
                self.session.require_reauth();
            }
            Err(TokenError::BadClient(_)) => {
                // The registration is gone: forget it, so the next sign-in registers anew.
                self.session.require_reauth();
                let dir = self.data_dir();
                let _ = blocking(move || {
                    let mut rec = load_host(&dir)?;
                    rec.client_id = None;
                    save_host(&dir, &rec)
                })
                .await;
                self.inner().client_id = None;
            }
            // Network trouble is not a sign-out: keep everything and try again later.
            Err(e) => tracing::warn!(error = %e, "chatgpt: refresh failed; will retry"),
        }
    }

    async fn sign_in(self: &Arc<Self>) -> std::result::Result<Value, RpcError> {
        if self.cloud_mode() {
            let mut st = self.status();
            st.error = Some(err(
                "cloud_mode",
                "Heed Cloud is on. Sign in with ChatGPT is for running Heed yourself.",
            ));
            return to_result(json!({"auth_url": null, "status": st}));
        }
        let _serial = self.change.lock().await;
        // A sign-in already waiting is replaced.
        if let Phase::SigningIn(cancel) = std::mem::take(&mut self.inner().phase)
            && let Some(c) = cancel
        {
            let _ = c.send(());
        }
        let dir = self.data_dir();
        let host = blocking(move || load_host(&dir))
            .await
            .map_err(|e| RpcError::internal(format!("chatgpt host id: {e}")))?;
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .map_err(|e| RpcError::internal(format!("chatgpt: loopback listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| RpcError::internal(e.to_string()))?
            .port();
        let attempt = Attempt::new(format!("http://127.0.0.1:{port}{CALLBACK_PATH}"))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let login_hint = self.inner().account.as_ref().and_then(|a| a.email.clone());
        let auth_url = chatgpt::authorize_url(
            &self.ep,
            host.client_id.as_deref(),
            &host.host_id,
            &attempt,
            login_hint.as_deref(),
        )
        .map_err(|e| RpcError::internal(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        {
            let mut i = self.inner();
            i.phase = Phase::SigningIn(Some(tx));
            i.error = None;
        }
        let this = self.clone();
        self.state.tasks.spawn(async move {
            let outcome = tokio::select! {
                _ = rx => Err(err("cancelled", "Sign-in was cancelled.")),
                () = tokio::time::sleep(SIGN_IN_TIMEOUT) => Err(err("timeout", "Sign-in timed out.")),
                r = this.await_callback(listener, &attempt, host) => r,
            };
            this.finish(outcome).await;
        });
        to_result(json!({"auth_url": auth_url, "status": self.status()}))
    }

    /// Waits for the callback with the right `state`, then completes the sign-in.
    async fn await_callback(
        &self,
        listener: TcpListener,
        attempt: &Attempt,
        mut host: HostRecord,
    ) -> std::result::Result<(), ChatgptError> {
        loop {
            let Ok((mut stream, peer)) = listener.accept().await else {
                continue;
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            let Some(target) = read_target(&mut stream).await else {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    &page("Heed", "Bad request."),
                )
                .await;
                continue;
            };
            let (path, query) = target.split_once('?').unwrap_or((&target, ""));
            if path != CALLBACK_PATH {
                respond(&mut stream, "404 Not Found", &page("Heed", "Not found.")).await;
                continue;
            }
            let cb = match chatgpt::parse_callback(query, &attempt.state) {
                Ok(cb) => cb,
                // Not our request (or a forgery): answer it and keep waiting for the real one.
                Err(_) => {
                    respond(
                        &mut stream,
                        "400 Bad Request",
                        &page(
                            "Heed",
                            "This sign-in link is not the one Heed is waiting for.",
                        ),
                    )
                    .await;
                    continue;
                }
            };
            let result = self.complete(cb, attempt, &mut host).await;
            let (title, body) = match &result {
                Ok(()) => (
                    "Signed in to ChatGPT",
                    "You can close this tab and go back to Heed.",
                ),
                Err(_) => (
                    "Sign-in didn't finish",
                    "Go back to Heed to see what happened.",
                ),
            };
            respond(&mut stream, "200 OK", &page(title, body)).await;
            return result;
        }
    }

    async fn complete(
        &self,
        cb: Callback,
        attempt: &Attempt,
        host: &mut HostRecord,
    ) -> std::result::Result<(), ChatgptError> {
        let (code, issued) = match cb {
            Callback::Denied { error } if error == "access_denied" => {
                return Err(err(
                    "access_denied",
                    "You didn't allow Heed to use ChatGPT.",
                ));
            }
            Callback::Denied { error } => {
                return Err(err(
                    "token_exchange_failed",
                    format!("OpenAI answered {error}."),
                ));
            }
            Callback::Code { code, client_id } => (code, client_id),
        };
        // A first registration's client id is saved before the exchange, so a retry after an
        // expired code reuses the same registration.
        if let Some(id) = issued
            && host.client_id.as_deref() != Some(id.as_str())
        {
            host.client_id = Some(id);
            let (dir, rec) = (self.data_dir(), host.clone());
            blocking(move || save_host(&dir, &rec))
                .await
                .map_err(|e| err("token_exchange_failed", e.to_string()))?;
        }
        let client_id = host
            .client_id
            .clone()
            .ok_or_else(|| err("token_exchange_failed", "OpenAI issued no client id."))?;
        self.record(&self.ep.token, 0);
        let (tokens, id_token) =
            chatgpt::exchange_code(&self.http, &self.ep, &client_id, &code, attempt, now())
                .await
                .map_err(|e| match e {
                    TokenError::Transient(m) => err("network", m),
                    other => err("token_exchange_failed", other.to_string()),
                })?;
        self.record(&self.ep.jwks, 0);
        let jwks = chatgpt::fetch_jwks(&self.http, &self.ep)
            .await
            .map_err(|e| err("network", e.to_string()))?;
        let account = chatgpt::verify_id_token(
            id_token.as_deref().unwrap_or(""),
            &jwks,
            &self.ep.issuer,
            &client_id,
            &attempt.nonce,
            now(),
        )
        .map_err(|why| {
            err(
                "invalid_id_token",
                format!("OpenAI's sign-in could not be verified: {why}."),
            )
        })?;
        let plan = tokens.plan_usage();
        let saved = self
            .save_credentials(&Credentials {
                client_id: client_id.clone(),
                account: account.clone(),
                tokens: tokens.clone(),
            })
            .await;
        self.session.set(tokens);
        self.session.resume();
        {
            let mut i = self.inner();
            i.client_id = Some(client_id);
            i.account = Some(account);
        }
        self.audit("chatgpt.sign_in", json!({"plan_usage": plan}));
        if !plan {
            return Err(err(
                "scope_not_granted",
                "ChatGPT plan usage isn't enabled for Heed. Try again, or use an API key instead.",
            ));
        }
        self.install().await?;
        if !saved {
            return Err(err(
                "no_keychain",
                "Signed in for now, but this computer has no keychain to remember it.",
            ));
        }
        Ok(())
    }

    async fn finish(&self, outcome: std::result::Result<(), ChatgptError>) {
        let mut i = self.inner();
        let signed_in = i.account.is_some() && self.session.tokens().is_some();
        i.phase = if signed_in {
            Phase::SignedIn
        } else {
            Phase::SignedOut
        };
        i.error = outcome.err();
        if let Some(e) = &i.error {
            tracing::info!(code = %e.code, "chatgpt: sign-in ended");
        } else {
            tracing::info!("chatgpt: signed in; ChatGPT serves the light and smart tiers");
        }
    }

    fn cancel(&self) -> ChatgptStatus {
        if let Phase::SigningIn(cancel) = &mut self.inner().phase
            && let Some(c) = cancel.take()
        {
            let _ = c.send(());
        }
        self.status()
    }

    fn resume(&self) -> ChatgptStatus {
        self.session.resume();
        let mut i = self.inner();
        if i.error.as_ref().is_some_and(|e| e.code == "usage_limit") {
            i.error = None;
        }
        drop(i);
        self.audit("chatgpt.resume", json!({}));
        self.status()
    }

    async fn sign_out(&self) -> ChatgptStatus {
        let _ = self.cancel();
        let _serial = self.change.lock().await;
        let client = self.inner().client_id.clone();
        if let (Some(t), Some(client)) = (self.session.tokens(), client)
            && let Some(rt) = t.refresh_token
        {
            self.record(&self.ep.revoke, 0);
            chatgpt::revoke(&self.http, &self.ep, &client, &rt).await;
        }
        self.session.clear();
        self.unroute();
        let secrets = self.secrets();
        let _ = blocking(move || secrets.delete(CREDENTIALS)).await;
        {
            let mut i = self.inner();
            let client = i.client_id.take();
            *i = Inner::default();
            // The registration survives sign-out (it is reused at the next sign-in).
            i.client_id = client;
        }
        self.audit("chatgpt.sign_out", json!({}));
        self.status()
    }
}

#[async_trait]
impl Feature for Arc<Chatgpt> {
    fn name(&self) -> &'static str {
        "chatgpt"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(
        &self,
        method: &str,
        _params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        match method {
            STATUS => to_result(self.status()),
            SIGN_IN => self.sign_in().await,
            CANCEL => to_result(self.cancel()),
            RESUME => to_result(self.resume()),
            SIGN_OUT => to_result(self.sign_out().await),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    async fn shutdown(&self) {
        let _ = self.cancel();
    }
}
