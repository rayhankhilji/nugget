//! `/v1/actions`: propose, preview, approve, cancel and check outbound actions. See
//! [`crate::actions`]: nothing runs before `POST /v1/actions/{id}/approve` with the digest of
//! the preview the user saw.
//!
//! | Route | |
//! |---|---|
//! | `POST /v1/actions` | propose `{kind, toolkit, input, idempotency_key}` → the action, `pending` |
//! | `GET /v1/actions?status=` | the user's recent actions |
//! | `GET /v1/actions/{id}` | status |
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

use super::{ApiError, ApiResult, AppState, user};
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
        .route("/v1/actions/{id}", get(status))
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
    let u = user(&s, &h).await?;
    let kind = ActionKind::parse(&b.kind).ok_or_else(|| bad("unknown action kind"))?;
    check_key(&b.idempotency_key)?;
    let toolkit = b.toolkit.to_ascii_lowercase();
    Ok(Json(
        s.actions
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
    let actions = s.actions.list(u.id, q.status.as_deref()).await?;
    Ok(Json(json!({ "actions": actions })))
}

async fn status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions.get(u.id, id).await?))
}

/// The dry run. The plan was computed at proposal time and nothing upstream is called.
async fn preview(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions.get(u.id, id).await?))
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
    let u = user(&s, &h).await?;
    Ok(Json(s.actions.approve(u.id, id, &b.digest).await?))
}

async fn cancel(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ActionView>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.actions.cancel(u.id, id).await?))
}
