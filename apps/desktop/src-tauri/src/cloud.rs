//! Nugget Cloud from the shell (docs/architecture-cloud.md, section 4): sign-in, the session,
//! Composio connections, sync and ask, and billing (Stripe Checkout and portal), through
//! `nugget-cloud-client`.
//!
//! Tokens never reach the webview: the PKCE verifier, the access token and the refresh token
//! (keychain) stay in Rust. The sign-in deep link (`nugget://auth/callback`) is exchanged here
//! (see `deeplink.rs`) and the page only hears [`SESSION_EVENT`], whose payload carries no
//! secret.

use std::sync::Arc;

use nugget_cloud_client::{
    Action, ActionKind, ActionStatus, Answer, Api, AppInfo, AskRequest, Callback, CloudConfig,
    CloudError, Connections, Interval, KeychainStore, Me, ModelTier, Plans, ProposeAction,
    Provider, Session, SyncReport, SystemClock,
};
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::open;

pub mod intel;

/// `{ signed_in: bool, error: string | null }`: the session changed (signed in from a link,
/// signed out, or found signed out by the server).
pub const SESSION_EVENT: &str = "nugget:cloud-session";
/// No payload: Composio sent the browser back; the Connections page reloads its list.
pub const CONNECTIONS_EVENT: &str = "nugget:cloud-connections";
/// No payload: Stripe Checkout sent the browser back (`nugget://billing/done`); the page reloads
/// the account and the plans.
pub const BILLING_EVENT: &str = "nugget:cloud-billing";

const MAX_QUESTION_LEN: usize = 4_000;
const MAX_ID_LEN: usize = 128;
const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;
/// Serialized size of an action's input.
const MAX_ACTION_INPUT_BYTES: usize = 256 * 1024;

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
            Ok(status) => emit_session(&app, status.signed_in, None),
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
    model: Option<String>,
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
    let model = match model.as_deref() {
        None => None,
        Some("fast") => Some(ModelTier::Fast),
        Some("smart") => Some(ModelTier::Smart),
        Some(_) => return Err(CloudError::Invalid("not a known model".into()).into()),
    };
    let api = cloud.api()?;
    checked(
        &app,
        api.ask(&AskRequest {
            question,
            conversation_id,
            model,
        })
        .await,
    )
}

#[tauri::command]
pub async fn cloud_apps(app: AppHandle, cloud: State<'_, Cloud>) -> Result<Vec<AppInfo>> {
    let api = cloud.api()?;
    checked(&app, api.apps().await)
}

fn check_action_kind(kind: &str) -> Result<ActionKind> {
    Ok(ActionKind::parse(kind)?)
}

fn check_action_status(status: &str) -> Result<ActionStatus> {
    Ok(ActionStatus::parse(status)?)
}

fn check_action_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(CloudError::Invalid("not a valid action".into()).into());
    }
    Ok(())
}

/// The approval echoes the hex SHA-256 the server showed with the preview.
fn check_digest(digest: &str) -> Result<()> {
    let ok = digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(CloudError::Invalid("not a valid action digest".into()).into())
    }
}

fn check_idempotency_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= MAX_IDEMPOTENCY_KEY_LEN
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-:.".contains(c));
    if ok {
        Ok(())
    } else {
        Err(CloudError::Invalid("not a valid idempotency key".into()).into())
    }
}

fn check_action_input(input: &serde_json::Value) -> Result<()> {
    if !input.is_object() {
        return Err(CloudError::Invalid("an action's input must be an object".into()).into());
    }
    let size = serde_json::to_vec(input).map_or(usize::MAX, |v| v.len());
    if size > MAX_ACTION_INPUT_BYTES {
        return Err(CloudError::Invalid("the action is too large".into()).into());
    }
    Ok(())
}

#[tauri::command]
pub async fn cloud_actions_list(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    status: Option<String>,
) -> Result<Vec<Action>> {
    let status = status.as_deref().map(check_action_status).transpose()?;
    let api = cloud.api()?;
    checked(&app, api.actions(status.map(ActionStatus::as_str)).await)
}

/// Propose a write to a connected app. Nothing is sent until it is approved
/// ([`cloud_action_approve`]).
#[tauri::command]
pub async fn cloud_action_propose(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    kind: String,
    toolkit: String,
    input: serde_json::Value,
    idempotency_key: String,
) -> Result<Action> {
    let kind = check_action_kind(&kind)?;
    check_toolkit(&toolkit)?;
    check_action_input(&input)?;
    check_idempotency_key(&idempotency_key)?;
    let api = cloud.api()?;
    checked(
        &app,
        api.propose_action(&ProposeAction {
            kind,
            toolkit,
            input,
            idempotency_key,
        })
        .await,
    )
}

/// A dry run: what the action would send, with no side effects.
#[tauri::command]
pub async fn cloud_action_preview(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    id: String,
) -> Result<Action> {
    check_action_id(&id)?;
    let api = cloud.api()?;
    checked(&app, api.action_preview(&id).await)
}

#[tauri::command]
pub async fn cloud_action_status(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    id: String,
) -> Result<Action> {
    check_action_id(&id)?;
    let api = cloud.api()?;
    checked(&app, api.action(&id).await)
}

/// Approve an action; `digest` is the one shown with its preview (the server refuses a
/// changed action with `digest_mismatch`).
#[tauri::command]
pub async fn cloud_action_approve(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    id: String,
    digest: String,
) -> Result<Action> {
    check_action_id(&id)?;
    check_digest(&digest)?;
    let api = cloud.api()?;
    checked(&app, api.approve_action(&id, &digest).await)
}

#[tauri::command]
pub async fn cloud_action_cancel(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    id: String,
) -> Result<Action> {
    check_action_id(&id)?;
    let api = cloud.api()?;
    checked(&app, api.cancel_action(&id).await)
}

/// The account: plan, subscription, entitlements and this month's usage.
#[tauri::command]
pub async fn cloud_me(app: AppHandle, cloud: State<'_, Cloud>) -> Result<Me> {
    let api = cloud.api()?;
    checked(&app, api.me().await)
}

#[tauri::command]
pub async fn cloud_billing_plans(app: AppHandle, cloud: State<'_, Cloud>) -> Result<Plans> {
    let api = cloud.api()?;
    checked(&app, api.plans().await)
}

/// Start a trial: opens Stripe Checkout in the browser. It comes back to
/// `nugget://billing/done` (see [`BILLING_EVENT`]).
#[tauri::command]
pub async fn cloud_billing_checkout(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    plan: String,
    interval: String,
) -> Result<()> {
    let interval = Interval::parse(&interval)?;
    let api = cloud.api()?;
    let link = checked(&app, api.checkout(&plan, interval).await)?;
    open_web(link.url).await
}

/// Opens the Stripe customer portal (card, invoices, plan changes, cancel) in the browser.
#[tauri::command]
pub async fn cloud_billing_portal(app: AppHandle, cloud: State<'_, Cloud>) -> Result<()> {
    let api = cloud.api()?;
    let link = checked(&app, api.portal().await)?;
    open_web(link.url).await
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

    #[test]
    fn action_kinds_and_statuses_are_known() {
        assert_eq!(
            check_action_kind("send_mail").unwrap(),
            ActionKind::SendMail
        );
        assert_eq!(
            check_action_kind("create_page").unwrap(),
            ActionKind::CreatePage
        );
        for bad in ["", "SEND_MAIL", "delete_mail", "send_mail "] {
            assert!(check_action_kind(bad).is_err(), "{bad}");
        }
        assert_eq!(
            check_action_status("pending").unwrap(),
            ActionStatus::Pending
        );
        for bad in ["", "unknown", "Pending", "pending&x=1"] {
            assert!(check_action_status(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn action_ids_are_bounded() {
        assert!(check_action_id("6f1c0b8e-2b7d-4a52-9a43-0e0f6a2c1d11").is_ok());
        assert!(check_action_id("").is_err());
        assert!(check_action_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn digests_are_lowercase_sha256_hex() {
        assert!(check_digest(&"0a".repeat(32)).is_ok());
        for bad in [
            String::new(),
            "0A".repeat(32),
            "0a".repeat(31),
            "0a".repeat(33),
            "g".repeat(64),
        ] {
            assert!(check_digest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn idempotency_keys_are_short_and_plain() {
        for ok in ["k", "ui:2026-11-02T10:00:00.123_ab-1", &"a".repeat(128)] {
            assert!(check_idempotency_key(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a/b", "é", &"a".repeat(129)] {
            assert!(check_idempotency_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn action_inputs_are_small_objects() {
        use serde_json::json;
        assert!(
            check_action_input(&json!({ "to": ["a@b.c"], "subject": "s", "body": "b" })).is_ok()
        );
        for bad in [json!(null), json!([]), json!("x"), json!(1)] {
            assert!(check_action_input(&bad).is_err(), "{bad}");
        }
        let big = json!({ "body": "x".repeat(MAX_ACTION_INPUT_BYTES) });
        assert!(check_action_input(&big).is_err());
    }
}
