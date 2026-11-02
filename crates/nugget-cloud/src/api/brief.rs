//! `GET /v1/brief?tz=`: today's morning brief (see [`crate::brief`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::cards::TzQuery;
use super::{ApiResult, AppState, user};

pub(super) async fn get(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<TzQuery>,
) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    super::cards::timezone(&s, u.id, q.tz.as_deref()).await?;
    let b = s.intel().brief(u.id, OffsetDateTime::now_utc()).await?;
    Ok(Json(json!({ "brief": b })))
}
