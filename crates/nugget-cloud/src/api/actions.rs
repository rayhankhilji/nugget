//! `/v1/actions`: propose, preview, approve, cancel and check outbound actions. See
//! [`crate::actions`]: nothing runs before `POST /v1/actions/{id}/approve` with the digest of
//! the preview the user saw.
//!
//! | Route | |
//! |---|---|
//! | `POST /v1/actions` | propose `{kind, toolkit, input, idempotency_key}` → the action, `pending` |
//! | `GET /v1/actions?status=&include=snoozed` | the user's recent actions (snoozed ones only with `include=snoozed`) |
//! | `GET /v1/actions/{id}` | status |
//! | `PATCH /v1/actions/{id}` | `{input}`: edit the editable fields → new preview and NEW digest |
//! | `POST /v1/actions/{id}/snooze` | `{until}` (RFC 3339, capped at the expiry; null shows it again) |
//! | `GET /v1/actions/{id}/preview` | the dry run: fields, diff, digest (no side effects) |
//! | `POST /v1/actions/{id}/approve` | `{digest}` → runs it once |
//! | `POST /v1/actions/{id}/cancel` | `{}` |

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ApiError, ApiResult, AppState, caller, user};
use crate::actions::ActionKind;
use crate::actions::store::{ActionError, ActionView, Proposal};

const STATUSES: &[&str] = &[
    "pending",
    "executing",
    "succeeded",
    "failed",
    "cancelled",
    "expired",
    "denied",
];

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/actions", post(propose).get(list))
        .route("/v1/actions/{id}", get(status).patch(update))
        .route("/v1/actions/{id}/snooze", post(snooze))
        .route("/v1/actions/{id}/preview", get(preview))
        .route("/v1/actions/{id}/approve", post(approve))
        .route("/v1/actions/{id}/cancel", post(cancel))
}

impl From<ActionError> for ApiError {
    fn from(e: ActionError) -> Self {
        let msg = e.to_string();
        let (status, code) = match e {
            ActionError::Invalid(_) => (StatusCode::BAD_REQUEST, "bad_action"),
            ActionError::NotConnected(_) => (StatusCode::BAD_REQUEST, "not_connected"),
            ActionError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ActionError::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
            ActionError::NotPending(_) => (StatusCode::CONFLICT, "not_pending"),
            ActionError::Expired => (StatusCode::GONE, "expired"),
            ActionError::DigestMismatch => (StatusCode::PRECONDITION_FAILED, "digest_mismatch"),
            ActionError::Denied(_) => (StatusCode::FORBIDDEN, "denied_by_policy"),
            ActionError::Db(e) => return e.into(),
        };
        ApiError::new(status, code, msg)
    }
}

fn bad(m: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_action", m)
}

#[derive(Deserialize)]
struct ProposeBody {
    kind: String,
    toolkit: String,
    input: Value,
    idempotency_key: String,
}

fn check_key(k: &str) -> ApiResult<()> {
    let ok = !k.is_empty()
        && k.len() <= 128
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c));
    if ok {
        Ok(())
    } else {
        Err(bad("idempotency_key: 1 to 128 of A-Z a-z 0-9 - _ : ."))
    }
}

async fn propose(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<ProposeBody>,
) -> ApiResult<Json<ActionView>> {
    // Proposing and approving need a live subscription (402); reading and cancelling do not,
    // so a lapsed user can still see and withdraw what is pending.
    let c = caller(&s, &h).await?;
    c.entitled()?;
    let u = c.user;
    s.limits
        .action
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("action", w))?;
    let kind = ActionKind::parse(&b.kind).ok_or_else(|| bad("unknown action kind"))?;
    check_key(&b.idempotency_key)?;
    let toolkit = b.toolkit.to_ascii_lowercase();
    Ok(Json(
        s.actions()
            .propose(
                u.id,
                Proposal {
                    kind,
                    toolkit: &toolkit,
                    input: &b.input,
                    idempotency_key: &b.idempotency_key,
                },
            )
            .await?,
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    /// `snoozed` lists snoozed actions too.
    include: Option<String>,
}

async fn list(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    if let Some(st) = &q.status
        && !STATUSES.contains(&st.as_str())
    {
        return Err(bad("unknown status"));
    }
    let include_snoozed = match q.include.as_deref() {
        None | Some("") => false,
        Some("snoozed") => true,
        Some(_) => return Err(bad("include may only be snoozed")),
    };
    let actions = s
        .actions()
        .list(u.id, q.status.as_deref(), include_snoozed)
        .await?;
    Ok(Json(json!({ "actions": actions })))
}

async fn status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions().get(u.id, id).await?))
}

/// The dry run. The plan was computed at proposal time and nothing upstream is called.
async fn preview(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions().get(u.id, id).await?))
}

#[derive(Deserialize)]
struct ApproveBody {
    digest: String,
}

async fn approve(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<ApproveBody>,
) -> ApiResult<Json<ActionView>> {
    let c = caller(&s, &h).await?;
    c.entitled()?;
    let u = c.user;
    s.limits
        .action
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("action", w))?;
    Ok(Json(s.actions().approve(u.id, id, &b.digest).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    input: Value,
}

/// The user's edit: like a proposal, it needs a live subscription and counts against the
/// action rate limit; it re-validates, re-plans and re-assesses, and returns a new digest.
async fn update(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<ActionView>> {
    let c = caller(&s, &h).await?;
    c.entitled()?;
    let u = c.user;
    s.limits
        .action
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("action", w))?;
    Ok(Json(s.actions().edit(u.id, id, &b.input).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnoozeBody {
    /// RFC 3339; null (or absent) shows the action again.
    #[serde(default)]
    until: Option<String>,
}

async fn snooze(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<SnoozeBody>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    let until = b
        .until
        .as_deref()
        .map(|t| {
            time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                .map_err(|_| bad("until must be an RFC 3339 time"))
        })
        .transpose()?;
    Ok(Json(s.actions().snooze(u.id, id, until).await?))
}

async fn cancel(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions().cancel(u.id, id).await?))
}
