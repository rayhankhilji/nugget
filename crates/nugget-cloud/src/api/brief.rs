//! `GET /v1/brief?tz=`: today's morning brief (see [`crate::brief`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::cards::TzQuery;
use super::{ApiResult, AppState, caller};

pub(super) async fn get(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<TzQuery>,
) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    if !c.entitled()?.daily_brief {
        return Err(super::cards::not_in_plan());
    }
    let u = c.user.id;
    super::cards::timezone(&s, u, q.tz.as_deref()).await?;
    let b = s.intel().brief(u, OffsetDateTime::now_utc()).await?;
    Ok(Json(json!({ "brief": b })))
}
