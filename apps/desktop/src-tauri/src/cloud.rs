//! Nugget Cloud from the shell (docs/architecture-cloud.md, section 4): sign-in, the session,
//! Composio connections, sync and ask, through `nugget-cloud-client`.
//!
//! Tokens never reach the webview: the PKCE verifier, the access token and the refresh token
//! (keychain) stay in Rust. The sign-in deep link (`nugget://auth/callback`) is exchanged here
//! (see `deeplink.rs`) and the page only hears [`SESSION_EVENT`], whose payload carries no
//! secret.

use std::sync::Arc;

use nugget_cloud_client::{
    Answer, Api, AskRequest, Callback, CloudConfig, CloudError, Connections, KeychainStore,
    Provider, Session, SyncReport, SystemClock,
};
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::open;

/// `{ signed_in: bool, error: string | null }`: the session changed (signed in from a link,
/// signed out, or found signed out by the server).
pub const SESSION_EVENT: &str = "nugget:cloud-session";
/// No payload: Composio sent the browser back; the Connections page reloads its list.
pub const CONNECTIONS_EVENT: &str = "nugget:cloud-connections";

const MAX_QUESTION_LEN: usize = 4_000;
const MAX_ID_LEN: usize = 128;

/// The cloud client, or why there is none (this build has no cloud configuration).
pub enum Cloud {
    Ready(Api),
    Off(String),
}

impl Cloud {
    pub fn from_env() -> Self {
        // The desktop's single rustls provider is `ring` (see the updater); make it the process
        // default before building the client, which brings no provider of its own.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let built = CloudConfig::from_env().and_then(|cfg| {
            let session = Session::new(
                cfg,
                nugget_cloud_client::http_client()?,
                Arc::new(KeychainStore),
                Arc::new(SystemClock),
            );
            Ok(Api::new(Arc::new(session)))
        });
        match built {
            Ok(api) => Self::Ready(api),
            Err(e) => {
                tracing::info!(reason = %e, "Nugget Cloud is off");
                Self::Off(e.to_string())
            }
        }
    }

    fn api(&self) -> Result<&Api, CloudCmdError> {
        match self {
            Self::Ready(api) => Ok(api),
            Self::Off(why) => Err(CloudCmdError::from(CloudError::NotConfigured(why.clone()))),
        }
    }
}

/// Errors as the UI's `ApiError` reads them (`kind`, `message`), plus the cloud's own `cloud`
/// code (`not_signed_in`, `quota_exceeded`, ...).
#[derive(Debug)]
pub struct CloudCmdError(CloudError);

impl From<CloudError> for CloudCmdError {
    fn from(e: CloudError) -> Self {
        Self(e)
    }
}

impl Serialize for CloudCmdError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("CloudCmdError", 4)?;
        // `rpc` with no code: the UI shows the message itself, which says what happened.
        st.serialize_field("kind", "rpc")?;
        st.serialize_field("message", &self.0.to_string())?;
        st.serialize_field("code", &None::<i64>)?;
        st.serialize_field("cloud", self.0.code())?;
        st.end()
    }
}

type Result<T, E = CloudCmdError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Serialize)]
struct SessionEvent {
    signed_in: bool,
    error: Option<String>,
}

fn emit_session(app: &AppHandle, signed_in: bool, error: Option<String>) {
    let _ = app.emit_to("main", SESSION_EVENT, SessionEvent { signed_in, error });
}

/// Pass an API result on; a "signed out" answer also tells the page, so it shows Sign-in.
fn checked<T>(app: &AppHandle, r: Result<T, CloudError>) -> Result<T> {
    if let Err(CloudError::NotSignedIn) = &r {
        emit_session(app, false, None);
        nudge_model_handoff();
    }
    r.map_err(CloudCmdError)
}

/// Open an `https:` page (a sign-in or consent page) in the default browser.
async fn open_web(url: String) -> Result<()> {
    let target = open::classify(&url).map_err(CloudError::Invalid)?;
    if !matches!(target, open::Target::Web(_)) {
        return Err(CloudError::Invalid("not a web page".into()).into());
    }
    tauri::async_runtime::spawn_blocking(move || open::open(target))
        .await
        .map_err(|e| CloudError::Invalid(e.to_string()))?
        .map_err(CloudError::Invalid)?;
    Ok(())
}

/// Finish a sign-in from `nugget://auth/callback` (called by `deeplink::dispatch`).
pub fn complete_sign_in(app: &AppHandle, callback: Callback) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(cloud) = app.try_state::<Cloud>() else {
            return;
        };
        let result = match cloud.api() {
            Ok(api) => api.session().complete(callback).await,
            Err(e) => Err(e.0),
        };
        match result {
            Ok(status) => {
                emit_session(&app, status.signed_in, None);
                nudge_model_handoff();
            }
            Err(e) => {
                tracing::warn!(code = e.code(), "cloud sign-in failed");
                emit_session(&app, false, Some(e.to_string()));
            }
        }
        crate::tray::show_main(&app, None);
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudSessionView {
    pub configured: bool,
    pub signed_in: bool,
    pub email: Option<String>,
}

#[tauri::command]
pub async fn cloud_session(cloud: State<'_, Cloud>) -> Result<CloudSessionView> {
    Ok(match cloud.inner() {
        Cloud::Off(_) => CloudSessionView {
            configured: false,
            signed_in: false,
            email: None,
        },
        Cloud::Ready(api) => {
            let s = api.session().status().await;
            CloudSessionView {
                configured: true,
                signed_in: s.signed_in,
                email: s.email,
            }
        }
    })
}

/// Start a sign-in: `provider` (`google`; `apple` is refused as not available yet in M1) opens
/// the browser; `email` sends a magic
/// link. Either way it finishes when the link comes back (see [`SESSION_EVENT`]).
#[tauri::command]
pub async fn cloud_sign_in(
    cloud: State<'_, Cloud>,
    provider: Option<String>,
    email: Option<String>,
) -> Result<()> {
    let api = cloud.api()?;
    match (provider, email) {
        (Some(p), None) => {
            let url = api.session().begin_oauth(Provider::parse(&p)?).await?;
            open_web(url).await
        }
        (None, Some(email)) => Ok(api.session().begin_magic_link(&email).await?),
        _ => Err(CloudError::Invalid("choose a provider or enter an email".into()).into()),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedOut {
    /// Whether Supabase revoked the session (the local copy is gone either way).
    pub revoked: bool,
}

#[tauri::command]
pub async fn cloud_sign_out(app: AppHandle, cloud: State<'_, Cloud>) -> Result<SignedOut> {
    let revoked = cloud.api()?.session().sign_out().await?;
    emit_session(&app, false, None);
    nudge_model_handoff();
    Ok(SignedOut { revoked })
}

#[tauri::command]
pub async fn cloud_connections_list(
    app: AppHandle,
    cloud: State<'_, Cloud>,
) -> Result<Connections> {
    let api = cloud.api()?;
    checked(&app, api.connections().await)
}

/// A Composio toolkit slug (`gmail`, `googlecalendar`).
fn check_toolkit(toolkit: &str) -> Result<()> {
    let ok = !toolkit.is_empty()
        && toolkit.len() <= 64
        && toolkit
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(CloudError::Invalid("not a known app".into()).into())
    }
}

/// Start connecting an app: opens Composio's consent page in the browser. Composio sends the
/// browser back to `nugget://connections/callback` (see [`CONNECTIONS_EVENT`]).
#[tauri::command]
pub async fn cloud_connect(app: AppHandle, cloud: State<'_, Cloud>, toolkit: String) -> Result<()> {
    check_toolkit(&toolkit)?;
    let api = cloud.api()?;
    let start = checked(&app, api.connect(&toolkit).await)?;
    open_web(start.redirect_url).await
}

#[tauri::command]
pub async fn cloud_disconnect(app: AppHandle, cloud: State<'_, Cloud>, id: String) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(CloudError::Invalid("not a valid connection".into()).into());
    }
    let api = cloud.api()?;
    checked(&app, api.disconnect(&id).await)
}

#[tauri::command]
pub async fn cloud_sync(app: AppHandle, cloud: State<'_, Cloud>) -> Result<SyncReport> {
    let api = cloud.api()?;
    checked(&app, api.sync().await)
}

#[tauri::command]
pub async fn cloud_ask(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    question: String,
    conversation_id: Option<String>,
) -> Result<Answer> {
    let question = question.trim().to_owned();
    if question.is_empty() || question.chars().count() > MAX_QUESTION_LEN {
        return Err(CloudError::Invalid("ask a question (up to 4,000 characters)".into()).into());
    }
    if conversation_id
        .as_ref()
        .is_some_and(|c| c.is_empty() || c.len() > MAX_ID_LEN)
    {
        return Err(CloudError::Invalid("not a valid conversation".into()).into());
    }
    let api = cloud.api()?;
    checked(
        &app,
        api.ask(&AskRequest {
            question,
            conversation_id,
        })
        .await,
    )
}

// ---- the cloud model for nuggetd ----------------------------------------------------------

/// Wakes [`run_model_handoff`] early: signed in or out, or nuggetd (re)connected.
static HANDOFF_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Hand nuggetd a fresh token now (sign-in, sign-out, a new daemon connection).
pub fn nudge_model_handoff() {
    HANDOFF_WAKE.notify_one();
}

/// Retry soon after a failed handoff (daemon away, network down).
const HANDOFF_RETRY: std::time::Duration = std::time::Duration::from_secs(15);

/// Keep nuggetd's Heed cloud model (`cloud.llm.session`) in step with the session, for the life
/// of the app. Signed in: the short-lived access token goes over the owner-only IPC socket
/// before each expiry (`next_handoff_in`) and on every nudge; signed out: it is withdrawn. The
/// token is never written to disk or logged; the refresh token never leaves this process.
pub fn start_model_handoff(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let wait = model_handoff_once(&app).await;
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = HANDOFF_WAKE.notified() => {}
            }
        }
    });
}

/// One handoff; returns when the next one is due.
async fn model_handoff_once(app: &AppHandle) -> std::time::Duration {
    use nugget_core::ipc::{CloudLlmSession, method};
    let idle = std::time::Duration::from_secs(3600);
    let Some(cloud) = app.try_state::<Cloud>() else {
        return idle;
    };
    let Cloud::Ready(api) = cloud.inner() else {
        return idle;
    };
    let daemon = app.state::<crate::daemon::Daemon>();
    let (params, next) = match api.session().handoff().await {
        Ok(h) => {
            let next = nugget_cloud_client::next_handoff_in(h.expires_at, now_secs());
            let params = CloudLlmSession {
                api_base: Some(h.api_base),
                access_token: Some(h.access_token),
                expires_at: Some(h.expires_at),
            };
            (params, next)
        }
        // Signed out: withdraw whatever nuggetd holds.
        Err(CloudError::NotSignedIn) => (CloudLlmSession::default(), idle),
        // A refresh that failed for now (offline): nuggetd keeps the token it has until it
        // expires, then uses local models.
        Err(e) => {
            tracing::debug!(code = e.code(), "cloud model: no fresh token yet");
            return HANDOFF_RETRY;
        }
    };
    let signed_in = params.access_token.is_some();
    match daemon
        .call::<_, serde_json::Value>(app, method::CLOUD_LLM_SESSION, &params)
        .await
    {
        Ok(_) => next,
        Err(e) if signed_in => {
            tracing::debug!(error = %e, "cloud model: nuggetd did not take the token yet");
            HANDOFF_RETRY
        }
        Err(_) => next,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Whether nuggetd is using the Heed cloud model now (no token in the answer).
#[tauri::command]
pub async fn cloud_model_status(
    app: AppHandle,
) -> Result<nugget_core::ipc::CloudLlmStatus, crate::daemon::CmdError> {
    app.state::<crate::daemon::Daemon>()
        .call(
            &app,
            nugget_core::ipc::method::CLOUD_LLM_STATUS,
            &serde_json::json!({}),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_serialize_for_the_ui() {
        let v = serde_json::to_value(CloudCmdError(CloudError::QuotaExceeded("used up".into())))
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "kind": "rpc", "message": "used up", "code": null, "cloud": "quota_exceeded" })
        );
    }

    #[test]
    fn toolkits_are_slugs() {
        assert!(check_toolkit("gmail").is_ok());
        assert!(check_toolkit("googlecalendar").is_ok());
        for bad in ["", "Gmail", "../x", "a b", &"a".repeat(65)] {
            assert!(check_toolkit(bad).is_err(), "{bad}");
        }
    }
}
