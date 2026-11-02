//! `/v1/cards`: the "Needs you" inbox and what the user does with a card (see [`crate::cards`]).
//!
//! - `GET /v1/cards?tz=`: `{cards, last_refresh}`, open cards best first.
//! - `POST /v1/cards/refresh`: run knowledge and cards now; `{cards, last_refresh, report}`.
//! - `POST /v1/cards/seen {ids}`: the desktop showed these.
//! - `POST /v1/cards/{id}/accept | done`, `/dismiss {reason, note?}`, `/snooze {until}`: `{card}`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ApiError, ApiResult, AppState, user};
use crate::cards::{self, CardOp, CardsError};
use crate::intel::{self, IntelError};

#[derive(Deserialize)]
pub(super) struct TzQuery {
    pub tz: Option<String>,
}

impl From<CardsError> for ApiError {
    fn from(e: CardsError) -> Self {
        match e {
            CardsError::Db(e) => e.into(),
            CardsError::Conflict => Self::new(StatusCode::CONFLICT, "conflict", e.to_string()),
            CardsError::Proactive(nugget_core::Error::NotFound(_)) => {
                Self::new(StatusCode::NOT_FOUND, "not_found", "no such card")
            }
            CardsError::Proactive(nugget_core::Error::Invalid(m)) => {
                Self::new(StatusCode::BAD_REQUEST, "bad_transition", m)
            }
            CardsError::Proactive(e) => {
                tracing::error!(%e, "cards");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "cards failed",
                )
            }
        }
    }
}

impl From<IntelError> for ApiError {
    fn from(e: IntelError) -> Self {
        match e {
            IntelError::Db(e) => e.into(),
            IntelError::Cards(e) => e.into(),
            IntelError::Gateway(e) => e.into(),
            IntelError::Pipeline(e) => {
                tracing::error!(%e, "intelligence pipeline");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "the knowledge pipeline failed",
                )
            }
        }
    }
}

/// Records the desktop's zone when it sends one, and returns the zone to use.
pub(super) async fn timezone(
    s: &AppState,
    user: Uuid,
    tz: Option<&str>,
) -> ApiResult<Option<String>> {
    if let Some(tz) = tz
        && !intel::set_timezone(&s.db, user, tz).await?
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_timezone",
            "tz must be an IANA time zone such as Europe/London",
        ));
    }
    Ok(intel::profile(&s.db, user).await?.timezone)
}

async fn inbox(s: &AppState, user: Uuid, tz: Option<String>) -> ApiResult<Value> {
    let (list, last) = cards::inbox(&s.db, user, &cards::config(tz)).await?;
    Ok(json!({
        "cards": list.iter().map(cards::wire).collect::<Vec<_>>(),
        "last_refresh": last.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
    }))
}

pub(super) async fn list(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<TzQuery>,
) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    let tz = timezone(&s, u.id, q.tz.as_deref()).await?;
    Ok(Json(inbox(&s, u.id, tz).await?))
}

pub(super) async fn refresh(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<TzQuery>,
) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    let tz = timezone(&s, u.id, q.tz.as_deref()).await?;
    let (report, _) = s.intel().run(u.id, OffsetDateTime::now_utc()).await?;
    let mut out = inbox(&s, u.id, tz).await?;
    out["report"] = json!(report);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub(super) struct SeenBody {
    ids: Vec<String>,
}

pub(super) async fn seen(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<SeenBody>,
) -> ApiResult<StatusCode> {
    let u = user(&s, &h).await?;
    let tz = intel::profile(&s.db, u.id).await?.timezone;
    let ids: Vec<String> = b.ids.into_iter().take(200).collect();
    cards::act(
        &s.db,
        u.id,
        &cards::config(tz),
        CardOp::Seen(ids),
        OffsetDateTime::now_utc(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub(super) struct ActBody {
    reason: Option<String>,
    note: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    until: Option<OffsetDateTime>,
}

pub(super) async fn act(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    body: Option<Json<ActBody>>,
) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let now = OffsetDateTime::now_utc();
    let bad = |m: &str| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", m);
    let op = match action.as_str() {
        "accept" => CardOp::Accept(id),
        "done" => CardOp::Done(id),
        "dismiss" => {
            let reason = cards::dismiss_reason(
                b.reason.as_deref().unwrap_or("not_relevant"),
                b.note.as_deref(),
            )
            .ok_or_else(|| {
                bad("reason must be not_relevant, already_handled, wrong_info, not_important or other")
            })?;
            CardOp::Dismiss(id, reason)
        }
        "snooze" => {
            let until = b.until.ok_or_else(|| bad("snooze needs `until`"))?;
            if until <= now || until > now + time::Duration::days(366) {
                return Err(bad("`until` must be in the next year"));
            }
            CardOp::Snooze(id, until)
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such card action",
            ));
        }
    };
    let tz = intel::profile(&s.db, u.id).await?.timezone;
    let card = cards::act(&s.db, u.id, &cards::config(tz), op, now).await?;
    Ok(Json(json!({ "card": card.as_ref().map(cards::wire) })))
}
